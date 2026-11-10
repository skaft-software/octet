//! Test-only differential harness for Pi session semantics.
//!
//! The same operation sequence is replayed through the published Pi
//! `SessionManager` (the oracle, at 1.0.2 and 1.5.0) and through octet's real
//! `Session`/`SessionStore` API, then compared on the durable tree, the selected
//! branch and the exact model-visible message list. Every Pi-written file is
//! also imported through octet's real importer and compared the same way.
//!
//! `scripts/fixtures/session-parity/README.md` documents the normalization and
//! the known-gap ledger; that ledger is the only place a divergence may be
//! declared.

mod canonical;
mod ledger;
mod native;
mod oracle;

use std::path::{Path, PathBuf};

use canonical::{Mismatch, PiMode, Scope};
use ledger::Ledger;
use native::{Native, Outcome};
use oracle::PiStep;
use serde_json::{json, Value};

const VERSIONS: [&str; 2] = ["1.0.2", "1.5.0"];

/// One divergence that the ledger does not accept.
struct Failure {
    version: String,
    scenario: String,
    step: usize,
    op: String,
    check: String,
    fingerprint: String,
    detail: String,
}

impl Failure {
    fn new(
        version: &str,
        case: &Case,
        step: usize,
        op: &str,
        check: impl Into<String>,
        fingerprint: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            version: version.to_owned(),
            scenario: case.name.clone(),
            step,
            op: op.to_owned(),
            check: check.into(),
            fingerprint: fingerprint.into(),
            detail: detail.into(),
        }
    }
}

/// One differential sequence: a name and the operations to replay.
pub(crate) struct Case {
    pub(crate) name: String,
    pub(crate) operations: Vec<Value>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives two levels below the repository root")
        .to_owned()
}

fn fixture_root(repo: &Path) -> PathBuf {
    repo.join("scripts/fixtures/session-parity")
}

/// Fixed scenarios, ordered by file name.
fn fixed_cases(repo: &Path) -> anyhow::Result<Vec<Case>> {
    let mut paths = std::fs::read_dir(fixture_root(repo).join("scenarios"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    anyhow::ensure!(!paths.is_empty(), "no session parity scenarios found");
    paths
        .iter()
        .map(|path| {
            let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            let name = value["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("{} has no name", path.display()))?
                .to_owned();
            let operations = value["operations"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("{} has no operations", path.display()))?
                .clone();
            Ok(Case { name, operations })
        })
        .collect()
}

/// Deterministic test-only PRNG (splitmix64), so seeded runs are reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5DEE_CE66_D1A4_9B15)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

/// Indices on the root-to-leaf path, oldest first.
fn ancestors(parents: &[Option<usize>], leaf: Option<usize>) -> Vec<usize> {
    let mut path = Vec::new();
    let mut cursor = leaf;
    while let Some(index) = cursor {
        path.push(index);
        cursor = parents[index];
    }
    path.reverse();
    path
}

/// Generate one seeded random sequence that is valid for both implementations.
///
/// Fork and lifecycle operations are covered by the fixed scenarios; a random
/// sequence stays in one session file so every reference is reproducible.
fn random_case(seed: u64, sequence: usize) -> Case {
    let mut rng = Rng::new(seed ^ ((sequence as u64) << 8));
    let length = 8 + rng.below(9) as usize;
    let mut parents: Vec<Option<usize>> = Vec::new();
    let mut leaf: Option<usize> = None;
    let mut operations: Vec<Value> = Vec::new();
    let mut call = 0usize;
    for _ in 0..length {
        let existing = parents.len();
        let named = format!("e{existing}");
        let choice = rng.below(100);
        if choice < 30 {
            operations.push(json!({
                "op": "message", "as": named,
                "message": {"role": "user", "content": format!("u{existing}"), "timestamp": 0},
            }));
        } else if choice < 45 {
            operations.push(json!({
                "op": "message", "as": named,
                "message": {"role": "assistant",
                            "content": [{"type": "text", "text": format!("a{existing}")}],
                            "api": "openai-completions", "provider": "openai", "model": "fixture",
                            "stopReason": "stop", "timestamp": 0},
            }));
        } else if choice < 55 {
            call += 1;
            operations.push(json!({
                "op": "message", "as": named,
                "message": {"role": "assistant", "content": [
                    {"type": "toolCall", "id": format!("c{call}"), "name": "read",
                     "arguments": {"path": "x"}}
                ], "api": "openai-completions", "provider": "openai", "model": "fixture",
                   "stopReason": "toolUse", "timestamp": 0},
            }));
        } else if choice < 65 {
            operations.push(json!({
                "op": "message", "as": named,
                "message": {"role": "toolResult", "toolCallId": format!("c{call}"), "toolName": "read",
                            "content": [{"type": "text", "text": format!("r{existing}")}],
                            "isError": false, "timestamp": 0},
            }));
        } else if choice < 70 {
            operations.push(json!({
                "op": "custom", "as": named, "customType": "state", "data": {"n": existing},
            }));
        } else if choice < 75 {
            operations.push(json!({
                "op": "custom_message", "as": named, "customType": "notice",
                "content": format!("n{existing}"), "display": false,
            }));
        } else if choice < 80 {
            operations.push(json!({
                "op": "model", "as": named, "provider": "openai",
                "modelId": format!("m{existing}"),
            }));
        } else if choice < 85 {
            operations.push(json!({
                "op": "thinking", "as": named, "level": "high",
            }));
        } else if choice < 88 {
            if existing == 0 {
                operations.push(json!({"op": "reopen"}));
                continue;
            }
            let target = rng.below(existing as u64);
            operations.push(json!({
                "op": "label", "target": format!("e{target}"), "label": format!("l{existing}"),
            }));
            // A label does not create an octet entry, so the logical model stays
            // put; Pi's own branch materialization collapses it the same way.
            continue;
        } else if choice < 91 {
            if existing == 0 {
                operations.push(json!({"op": "reopen"}));
                continue;
            }
            let target = rng.below(existing as u64) as usize;
            operations.push(json!({"op": "branch", "target": format!("e{target}")}));
            leaf = Some(target);
            continue;
        } else if choice < 93 {
            operations.push(json!({"op": "reset"}));
            leaf = None;
            continue;
        } else if choice < 96 {
            let path = ancestors(&parents, leaf);
            if path.is_empty() {
                operations.push(json!({
                    "op": "thinking", "as": named, "level": "low",
                }));
            } else {
                let kept = path[rng.below(path.len() as u64) as usize];
                operations.push(json!({
                    "op": "compaction", "as": named, "summary": format!("s{existing}"),
                    "firstKept": format!("e{kept}"),
                }));
            }
        } else if choice < 99 {
            if existing == 0 {
                operations.push(json!({"op": "reopen"}));
                continue;
            }
            let target = rng.below(existing as u64);
            operations.push(json!({
                "op": "summary", "as": named, "target": format!("e{target}"),
                "summary": format!("b{existing}"),
            }));
        } else {
            operations.push(json!({"op": "reopen"}));
            continue;
        }
        parents.push(leaf);
        leaf = Some(parents.len() - 1);
    }
    Case {
        name: format!("random-{seed}-{sequence}"),
        operations,
    }
}

/// Replay one case on both sides and record every divergence.
fn evaluate(
    case: &Case,
    steps: &[PiStep],
    scratch: &Path,
    ledger: &mut Ledger,
    failures: &mut Vec<Failure>,
    version: &str,
) -> usize {
    let mut native = match Native::new(scratch) {
        Ok(native) => native,
        Err(error) => {
            failures.push(Failure::new(
                version,
                case,
                0,
                "open",
                "harness",
                "create-session",
                format!("{error:#}"),
            ));
            return 0;
        }
    };
    let mut compared = 0;
    for (index, op) in case.operations.iter().enumerate() {
        let Some(pi) = steps.get(index) else {
            failures.push(Failure::new(
                version,
                case,
                index,
                "oracle",
                "harness",
                "missing-step",
                format!("oracle recorded only {} steps", steps.len()),
            ));
            return compared;
        };
        let op_name = op["op"].as_str().unwrap_or("?");
        let refused = match native.apply(op) {
            Outcome::Applied => None,
            Outcome::Refused(reason) => Some(reason),
        };
        let mut mismatches: Vec<Mismatch> = Vec::new();
        if pi.rejected && refused.is_none() {
            mismatches.push(Mismatch {
                check: "capability".into(),
                fingerprint: format!("accepted-invalid:{op_name}"),
                detail: "Pi refused this operation and octet accepted it".into(),
            });
        }
        if let Some(reason) = &refused {
            if !pi.rejected {
                mismatches.push(Mismatch {
                    check: "capability".into(),
                    fingerprint: format!("refused:{op_name}"),
                    detail: reason.clone(),
                });
            }
        }
        if refused.is_none() {
            let expected = canonical::from_pi(&pi.state, PiMode::Native, pi.disk.is_some());
            match native.snapshot() {
                Ok(actual) => {
                    mismatches.extend(canonical::compare(&expected, &actual, "", Scope::Full));
                    if let Some(disk) = &pi.disk {
                        // Reopening from disk is its own comparison: Pi keeps no
                        // durable leaf pointer, so its reopened state can differ
                        // from the in-memory selection.
                        let expected_disk = canonical::from_pi(disk, PiMode::Native, true);
                        match native.reopened() {
                            Ok(reopened) => mismatches.extend(canonical::compare(
                                &expected_disk,
                                &reopened,
                                "reopen-",
                                Scope::TreeOnly,
                            )),
                            Err(error) => mismatches.push(Mismatch {
                                check: "reopen".into(),
                                fingerprint: "reopen".into(),
                                detail: format!("{error:#}"),
                            }),
                        }
                        let file = oracle::session_file(scratch, index);
                        // The importer reads the file, so the expectation is the
                        // file's own state, not the live branch pointer.
                        let expected_import = canonical::from_pi(disk, PiMode::Import, true);
                        match import_snapshot(scratch, index, &file) {
                            Ok(imported) => mismatches.extend(canonical::compare(
                                &expected_import,
                                &imported,
                                "import-",
                                Scope::Full,
                            )),
                            Err(reason) => mismatches.push(Mismatch {
                                check: "import-capability".into(),
                                fingerprint: "import:refused".into(),
                                detail: reason,
                            }),
                        }
                    }
                }
                Err(error) => failures.push(Failure::new(
                    version,
                    case,
                    index,
                    op_name,
                    "harness",
                    "snapshot",
                    format!("{error:#}"),
                )),
            }
        }
        for mismatch in &mismatches {
            if !ledger.accept(&case.name, index, mismatch) {
                failures.push(Failure::new(
                    version,
                    case,
                    index,
                    op_name,
                    mismatch.check.clone(),
                    mismatch.fingerprint.clone(),
                    mismatch.detail.clone(),
                ));
            }
        }
        compared += 1;
        if refused.is_some() && !pi.rejected {
            // A reference that cannot perform the operation has no differential
            // meaning after this point; the fixed scenarios put such operations
            // last so nothing else is skipped.
            return compared;
        }
    }
    compared
}

/// Import one Pi file with octet's real importer and snapshot the result.
fn import_snapshot(
    case_root: &Path,
    step: usize,
    file: &Path,
) -> Result<canonical::Snapshot, String> {
    let store_dir = case_root.join("imports").join(step.to_string());
    std::fs::create_dir_all(&store_dir).map_err(|error| error.to_string())?;
    let workspace = case_root.join("work");
    let store = crate::session_store::SessionStore::new(&store_dir, &workspace);
    std::fs::create_dir_all(store.dir()).map_err(|error| error.to_string())?;
    let report = crate::session_commands::import_session(&store, file, &workspace)
        .map_err(|error| format!("{error:#}"))?;
    let session = octet_agent::Session::open_read_only(&report.destination)
        .map_err(|error| format!("{error:#}"))?;
    let (model, thinking, settings_error) = if session.entries().is_empty() {
        (None, None, None)
    } else {
        match store.inspect_by_id(&report.id) {
            Ok(inspected) => (
                inspected.catalog.configured_model,
                inspected.catalog.configured_reasoning,
                None,
            ),
            Err(error) => (None, None, Some(format!("{error:#}"))),
        }
    };
    let name = store
        .load_metadata(&report.id)
        .map_err(|error| format!("{error:#}"))?
        .name;
    let mut snapshot =
        canonical::from_octet(&session, native::NAMESPACE, name, (model, thinking), true)
            .map_err(|error| format!("{error:#}"))?;
    snapshot.settings_error = settings_error;
    Ok(snapshot)
}

fn load_ledger(repo: &Path) -> Ledger {
    Ledger::load(&fixture_root(repo).join("known-gaps.json"))
        .expect("session parity ledger is readable")
}

fn fail(failures: Vec<Failure>, stale: Vec<String>) -> ! {
    use std::collections::BTreeMap;
    if failures.is_empty() && stale.is_empty() {
        unreachable!("fail() is only called with a failure");
    }
    let total = failures.len();
    let mut groups: BTreeMap<(String, String), (usize, Vec<String>, Vec<String>)> = BTreeMap::new();
    for failure in &failures {
        let key = (failure.check.clone(), failure.fingerprint.clone());
        let entry = groups
            .entry(key)
            .or_insert_with(|| (0, Vec::new(), Vec::new()));
        entry.0 += 1;
        let seen = format!("{}@{}", failure.scenario, failure.step);
        if entry.2.len() < 8 && !entry.2.contains(&seen) {
            entry.2.push(seen);
        }
        if entry.1.len() < 3 {
            entry.1.push(format!(
                "[{}] {} step {} ({}): {}",
                failure.version, failure.scenario, failure.step, failure.op, failure.detail
            ));
        }
    }
    let mut message = String::new();
    if total > 0 {
        message.push_str(&format!(
            "session parity: {total} unexpected divergence(s) in {} kind(s)\n",
            groups.len()
        ));
        for ((check, fingerprint), (count, examples, scenarios)) in groups {
            message.push_str(&format!("  {check} {fingerprint} x{count}\n"));
            for example in &examples {
                message.push_str(&format!("    e.g. {example}\n"));
            }
            message.push_str(&format!("    seen in: {}\n", scenarios.join(", ")));
        }
    }
    if !stale.is_empty() {
        message.push_str(&format!(
            "session parity: {} stale known-gap entries (a fix must delete its line)\n",
            stale.len()
        ));
        for gap in stale.iter() {
            message.push_str("  ");
            message.push_str(gap);
            message.push('\n');
        }
    }
    panic!("{message}");
}

#[test]
fn fixed_scenarios_match_pi() {
    let repo = repo_root();
    let cases = fixed_cases(&repo).expect("fixed scenarios load");
    let mut ledger = load_ledger(&repo);
    let mut failures = Vec::new();
    let mut compared = 0;
    let scratch = tempfile::tempdir().expect("scratch directory");
    for version in VERSIONS {
        let directory = scratch.path().join(version);
        let oracle = oracle::run(&repo, version, &directory, &cases).expect("Pi oracle runs");
        for case in &cases {
            let steps = oracle
                .get(&case.name)
                .unwrap_or_else(|| panic!("oracle did not report {}", case.name));
            compared += evaluate(
                case,
                steps,
                &directory.join(&case.name),
                &mut ledger,
                &mut failures,
                version,
            );
        }
    }
    let executed = cases.iter().map(|case| case.name.clone()).collect();
    let stale = ledger.stale(&executed);
    if !stale.is_empty() || !failures.is_empty() {
        fail(failures, stale);
    }
    println!(
        "session parity: {} fixed scenarios on {} Pi versions, {compared} steps compared, {} known gaps",
        cases.len(),
        VERSIONS.len(),
        ledger.len()
    );
}

#[test]
fn seeded_random_sequences_match_pi() {
    let repo = repo_root();
    let seeds = std::env::var("OCTET_PARITY_SEEDS").unwrap_or_else(|_| "1".to_owned());
    let sequences: usize = std::env::var("OCTET_PARITY_SEQUENCES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12);
    let mut cases = Vec::new();
    for seed in seeds
        .split(',')
        .filter_map(|seed| seed.trim().parse::<u64>().ok())
    {
        for sequence in 0..sequences {
            cases.push(random_case(seed, sequence));
        }
    }
    let mut ledger = load_ledger(&repo);
    let mut failures = Vec::new();
    let mut compared = 0;
    let scratch = tempfile::tempdir().expect("scratch directory");
    for version in VERSIONS {
        let directory = scratch.path().join(version);
        let oracle = oracle::run(&repo, version, &directory, &cases).expect("Pi oracle runs");
        for case in &cases {
            let steps = oracle
                .get(&case.name)
                .unwrap_or_else(|| panic!("oracle did not report {}", case.name));
            let root = directory.join(&case.name);
            let mut case_failures = Vec::new();
            compared += evaluate(case, steps, &root, &mut ledger, &mut case_failures, version);
            if case_failures.is_empty() {
                continue;
            }
            let (length, minimal_failures, prefix) =
                minimal_prefix(case, steps, &root, &repo, version);
            failures.push(Failure::new(
                version,
                case,
                0,
                "shrink",
                "harness",
                "minimal-prefix",
                format!(
                    "shrinks to {length} operation prefix: {}",
                    serde_json::to_string(&prefix).unwrap_or_default()
                ),
            ));
            failures.extend(minimal_failures);
        }
    }
    // Coverage of the ledger is the fixed scenarios' job: random sequences only
    // prove that no divergence outside the declared gaps appears in combinations.
    if !failures.is_empty() {
        fail(failures, Vec::new());
    }
    println!(
        "session parity: {} seeded random sequences on {} Pi versions, {compared} steps compared",
        cases.len(),
        VERSIONS.len()
    );
}

/// Shrink a failing sequence to the shortest prefix that still fails.
fn minimal_prefix(
    case: &Case,
    steps: &[PiStep],
    scratch: &Path,
    repo: &Path,
    version: &str,
) -> (usize, Vec<Failure>, Vec<Value>) {
    for length in 1..=case.operations.len() {
        let prefix = Case {
            name: case.name.clone(),
            operations: case.operations[..length].to_vec(),
        };
        let mut ledger = load_ledger(repo);
        let mut failures = Vec::new();
        evaluate(
            &prefix,
            &steps[..length],
            &scratch.join(format!("prefix-{length}")),
            &mut ledger,
            &mut failures,
            version,
        );
        if !failures.is_empty() {
            return (length, failures, prefix.operations);
        }
    }
    (case.operations.len(), Vec::new(), case.operations.clone())
}
