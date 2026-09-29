//! Tests for mapping a migrated `pi` configuration onto octet's own types.
//!
//! Why this is a separate module: this module is a pure mapping layer with no I/O
//! beyond reading the migration input, so its tests are table-shaped and belong
//! next to the mapping table they pin rather than inside the traversal code.

use super::*;
use octet_ai::ModelId;

fn paths(temp: &tempfile::TempDir) -> MigrationPaths {
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    MigrationPaths::new(home).unwrap()
}

fn mapped_model(provider: &str, model: &str) -> MigrationOutcome<Model> {
    MigrationOutcome::mapped("settings.json", Model::new(provider, model).unwrap()).unwrap()
}

fn setup_with_models(models: Vec<MigrationOutcome<Model>>) -> MigratedSetup {
    MigratedSetup::with_parts("pi", models, Vec::new(), Vec::new(), Vec::new(), Vec::new()).unwrap()
}

fn setup() -> MigratedSetup {
    let model =
        MigrationOutcome::mapped("settings.json", Model::new("openai", "gpt-4o").unwrap()).unwrap();
    let skill = MigrationOutcome::mapped(
        "skills/review/SKILL.md",
        Skill::new("review", "Review this change.").unwrap(),
    )
    .unwrap();
    let server = MigrationOutcome::mapped(
        "settings.json",
        McpServer::new(
            "docs",
            McpTransport::stdio("docs-mcp", vec!["--stdio".into()]).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    MigratedSetup::with_parts("pi", vec![model], vec![skill], vec![server], vec![], vec![]).unwrap()
}

#[test]
fn ingestion_is_disabled_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(&temp);
    let plan = build_ingestion_plan(&paths, &setup(), false).unwrap();
    assert!(plan.conflicts.is_empty());
    let backup = apply_ingestion_plan(&paths, &plan).unwrap().unwrap();
    assert!(backup.join("manifest.json").exists());
    let skill = fs::read_to_string(paths.skills.join("review/SKILL.md")).unwrap();
    assert!(skill.contains("disable-model-invocation: true"));
    let mcp: Value = serde_json::from_slice(&fs::read(&paths.mcp).unwrap()).unwrap();
    assert_eq!(mcp["servers"]["docs"]["enabled"], false);
    assert_eq!(mcp["servers"]["docs"]["command"], "docs-mcp");
    let second = build_ingestion_plan(&paths, &setup(), false).unwrap();
    assert!(second.conflicts.is_empty());
    assert!(second.changes.is_empty());
}

#[test]
fn failed_apply_rolls_back_prior_writes_without_overwriting_the_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(&temp);
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let original = b"# existing destination configuration\n";
    fs::write(&paths.config, original).unwrap();
    let mut setup = setup();
    setup
        .push_model(mapped_model("openai", "gpt-4o-mini"))
        .unwrap();
    let plan = build_ingestion_plan(&paths, &setup, false).unwrap();
    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.changes[0].target, paths.config);
    assert_eq!(plan.changes[1].target, paths.mcp);
    assert_eq!(plan.changes.last().unwrap().target, paths.state);

    // A second writer wins after planning. The production CAS, rather than
    // a mocked write error, must fail after the first target was committed.
    let concurrent = b"concurrent writer owns this file\n";
    fs::write(&paths.mcp, concurrent).unwrap();
    let error = apply_ingestion_plan(&paths, &plan).unwrap_err().to_string();
    assert!(error.contains("was rolled back"), "{error}");
    assert!(error.contains("Backup retained"), "{error}");
    assert_eq!(fs::read(&paths.config).unwrap(), original);
    assert_eq!(fs::read(&paths.mcp).unwrap(), concurrent);
    assert!(!paths.state.exists());
    assert!(!paths.skills.join("review/SKILL.md").exists());
    let backups = fs::read_dir(&paths.backups)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].join("manifest.json").is_file());
}

#[test]
fn canonical_provider_qualified_model_uses_the_catalog_id() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(&temp);
    let setup = setup_with_models(vec![mapped_model("openai", "gpt-4o-mini")]);

    let plan = build_ingestion_plan(&paths, &setup, false).unwrap();
    assert_eq!(plan.counts.models, 1);
    assert_eq!(plan.counts.skipped, 0);
    assert_eq!(plan.diagnostic_count, 0);
    assert!(plan.model_diagnostics.is_empty());
    let config = plan
        .changes
        .iter()
        .find(|change| change.target == paths.config)
        .unwrap();
    assert!(std::str::from_utf8(&config.desired)
        .unwrap()
        .contains("model = \"gpt-4o-mini\""));
}

#[test]
fn unsupported_source_items_do_not_create_migration_state() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(&temp);
    let setup = MigratedSetup::with_parts(
        "pi",
        vec![MigrationOutcome::mapped(
            "settings.json",
            Model::new("unsupported", "model").unwrap(),
        )
        .unwrap()],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();

    let plan = build_ingestion_plan(&paths, &setup, false).unwrap();
    assert!(plan.changes.is_empty());
    assert_eq!(plan.counts.skipped, 1);
    assert_eq!(plan.diagnostic_count, 1);
    assert_eq!(plan.model_diagnostics.len(), 1);
    assert_eq!(plan.model_diagnostics[0].path, "settings.json");
    assert_eq!(plan.model_diagnostics[0].reason, UNKNOWN_MODEL_DIAGNOSTIC);
    assert!(apply_ingestion_plan(&paths, &plan).unwrap().is_none());
    assert!(!paths.state.exists());
}

#[test]
fn later_unknown_model_does_not_fall_back_to_lower_precedence_model() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(&temp);
    let setup = setup_with_models(vec![
        mapped_model("openai", "gpt-4o-mini"),
        mapped_model("anthropic", "gpt-4o-mini"),
    ]);

    let plan = build_ingestion_plan(&paths, &setup, false).unwrap();
    assert!(plan.changes.is_empty());
    assert_eq!(plan.counts.skipped, 1);
    assert_eq!(plan.model_diagnostics.len(), 1);
    assert_eq!(plan.model_diagnostics[0].reason, UNKNOWN_MODEL_DIAGNOSTIC);
}

#[test]
fn catalog_model_resolution_is_provider_exact_and_rejects_custom_ids_and_ambiguity() {
    let mut catalog = ModelCatalog::builtin().unwrap();
    assert_eq!(
        resolve_catalog_model(&catalog, "openai", "gpt-4o-mini"),
        CatalogModelResolution::Resolved("gpt-4o-mini".to_owned())
    );
    assert_eq!(
        resolve_catalog_model(&catalog, "anthropic", "gpt-4o-mini"),
        CatalogModelResolution::Unknown
    );

    let template = (*catalog
        .resolve(&ModelId("gpt-4o-mini".to_owned()))
        .unwrap()
        .spec)
        .clone();
    let mut custom_id = template.clone();
    custom_id.id = ModelId("custom/openai/fixture".to_owned());
    custom_id.api_name = "fixture-api".to_owned();
    catalog.register_model(custom_id).unwrap();
    assert_eq!(
        resolve_catalog_model(&catalog, "openai", "custom/openai/fixture"),
        CatalogModelResolution::Unknown
    );

    let mut first = template.clone();
    first.id = ModelId("shared-api-one".to_owned());
    first.api_name = "shared-api".to_owned();
    let mut second = first.clone();
    second.id = ModelId("shared-api-two".to_owned());
    catalog.register_model(first).unwrap();
    catalog.register_model(second).unwrap();

    let setup = setup_with_models(vec![mapped_model("openai", "shared-api")]);
    let mut counts = PlanCounts::default();
    let selection = selected_model_in_catalog(&setup, &catalog, &mut counts);
    assert_eq!(selection.model, None);
    assert_eq!(counts.skipped, 1);
    assert_eq!(selection.diagnostics.len(), 1);
    assert_eq!(selection.diagnostics[0].reason, AMBIGUOUS_MODEL_DIAGNOSTIC);
}

#[test]
fn changed_imported_skill_is_a_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(&temp);
    let plan = build_ingestion_plan(&paths, &setup(), false).unwrap();
    apply_ingestion_plan(&paths, &plan).unwrap();
    let target = paths.skills.join("review/SKILL.md");
    octet_agent::secure_fs::write_private_atomic(&target, b"user edit", MAX_SKILL_BYTES).unwrap();
    let conflict = build_ingestion_plan(&paths, &setup(), false).unwrap();
    assert_eq!(conflict.conflicts.len(), 1);
    let accepted = build_ingestion_plan(&paths, &setup(), true).unwrap();
    assert_eq!(accepted.conflicts.len(), 1);
    apply_ingestion_plan(&paths, &accepted).unwrap();
    assert!(fs::read_to_string(target)
        .unwrap()
        .contains("Imported Pi skill"));
}

#[test]
fn backup_restores_original_targets() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(&temp);
    octet_agent::secure_fs::create_private_directory_all(&paths.home.join(".octet")).unwrap();
    octet_agent::secure_fs::write_atomic_if_unchanged(
        &paths.config,
        None,
        b"model = \"openai/gpt-4.1\"\n",
        MAX_CONFIG_BYTES,
    )
    .unwrap();
    let plan = build_ingestion_plan(&paths, &setup(), false).unwrap();
    let backup = apply_ingestion_plan(&paths, &plan).unwrap().unwrap();
    restore_backup(&paths, &backup, false).unwrap();
    assert_eq!(
        fs::read_to_string(&paths.config).unwrap(),
        "model = \"openai/gpt-4.1\"\n"
    );
    assert!(!paths.skills.join("review/SKILL.md").exists());
}

#[test]
fn adapter_preserves_source_and_does_not_copy_mcp_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("pi");
    fs::create_dir_all(source.join("skills/review")).unwrap();
    let settings = br#"{"model":"openai/gpt-4o-mini","mcpServers":{"docs":{"command":"docs-mcp","args":["--stdio"],"env":{"TOKEN":"MIGRATION_SECRET"}}}}"#;
    fs::write(source.join("settings.json"), settings).unwrap();
    fs::write(source.join("skills/review/SKILL.md"), "Review.").unwrap();
    let before = sha256_hex(&fs::read(source.join("settings.json")).unwrap());
    let detected = pi_detect(&source).unwrap();
    assert!(detected.detected);
    let imported = pi_import(&source, &detected.config_paths).unwrap();
    assert_eq!(imported.models.len(), 1);
    assert_eq!(imported.skills.len(), 1);
    assert_eq!(imported.mcp_servers.len(), 1);
    assert!(imported
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.reason.contains("environment")));

    let paths = paths(&temp);
    let setup = normalize_adapter_result(imported).unwrap();
    let plan = build_ingestion_plan(&paths, &setup, false).unwrap();
    apply_ingestion_plan(&paths, &plan).unwrap();
    for target in [&paths.config, &paths.mcp, &paths.state] {
        assert!(
            !fs::read_to_string(target)
                .unwrap()
                .contains("MIGRATION_SECRET"),
            "credential leaked into {}",
            target.display()
        );
    }
    assert_eq!(
        before,
        sha256_hex(&fs::read(source.join("settings.json")).unwrap())
    );
}

#[test]
fn adapter_line_reader_is_bounded() {
    let mut exact = vec![b'x'; api::MAX_FRAME_BYTES];
    exact.push(b'\n');
    let mut reader = BufReader::new(exact.as_slice());
    assert_eq!(
        read_bounded_adapter_line(&mut reader)
            .unwrap()
            .unwrap()
            .len(),
        api::MAX_FRAME_BYTES
    );

    let mut oversized = vec![b'x'; api::MAX_FRAME_BYTES.saturating_add(1)];
    oversized.push(b'\n');
    let mut reader = BufReader::new(oversized.as_slice());
    assert_eq!(
        read_bounded_adapter_line(&mut reader).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn canonical_adapter_frames_reject_whitespace_and_duplicates() {
    assert!(parse_canonical_adapter_frame(r#"{"id":1, "jsonrpc":"2.0","result":{}}"#).is_err());
    assert!(
        parse_canonical_adapter_frame(r#"{"id":1,"id":1,"jsonrpc":"2.0","result":{}}"#).is_err()
    );
}
