# #179 Streamable HTTP MCP execution

Baseline: `df5a7e809715961b9344af6b52e43a6ca48f56b3` (`df5a7e80`).
Owner: `mcp-transport`; writes restricted to `extensions/octet-mcp/**` and this file.
No commits, remote servers, real credentials, publication, or global formatting.

## Initial inspection

- Clean worktree at initial inspection; transport 1,397 lines, existing HTTP tests 776 lines.
- Read complete package README, REFERENCE, CHANGELOG and directly linked installation/release docs; inspected legacy host resource-owner contract, runtime, manager, config, SDK ownership flow and existing HTTP fixtures.
- Confirmed nine documented defects against source: unpinned `HTTPSConnection` DNS; ignored handler owner; thread-only DNS; delayed SSE control routing and pre-parse redaction; unbounded reply/cancel thread launches; per-exchange SSE budgets; EOF dispatch/content-length truncation; empty ID conflation; renewed initialized/catalog deadlines.
- Experimental process-owner gate will remain. Qualification must not be inferred from local tests or removing warnings.

## Commands / outcomes (incremental)

- `git status --short; git rev-parse HEAD`: initially clean; baseline above.
- Initial source and documentation reads completed.
- Baseline: `(cd extensions/octet-mcp && python3 -m unittest discover -s tests -t . -v)`: **49 passed**, 5.880 s; `/tmp/octet-mcp-baseline-tests.log`.
- First network/framing candidate: same complete suite **49 passed**, 6.843 s. An owner-edit script in that shell invocation used the wrong cwd and raised `FileNotFoundError` before editing; reran from repository root (not counted as successful owner implementation).
- Owner-fenced candidate: same complete suite **49 passed**, 6.094 s; `/tmp/octet-mcp-candidate-tests.log`. New adversarial regressions still pending.

- First new regression run: complete suite **64 passed**, 16.189 s; `/tmp/octet-mcp-hardening-tests.log` (subsequently replaced by later run).
- Strict chunk-finalization review found stdlib accepts missing final trailer CRLF; added strict chunk reader and regression variants rather than relying on stdlib EOF acceptance.
- Targeted follow-ups exposed a **test admission race**: 150 ms control expiry/socket timeout released slots while filling them (two observed failures). Replaced the admission test's timing dependency with event-blocked exchanges and frozen watchdog creation; added a separate real-socket watchdog test. Production limits were not relaxed.
- Complete suite after real loopback TLS success/SNI/hostname/trust coverage: **67 passed**, 15.510 s; `/tmp/octet-mcp-hardening-tests.log`.
- `/usr/bin/python3 -m unittest discover -s tests -t . -v` (Python 3.9.6): **66 passed, 1 failed**, 15.948 s; `/tmp/octet-mcp-python39-tests.log`. Failure is unchanged `tests/test_release.py:22`, which asserts optional `tomllib` is present (stdlib adds it only in 3.11); remote hardening tests all passed. No skip or weakened assertion added.

## Candidate plan

Pin validated addresses without secondary DNS; make resolver subprocesses cancellable/reaped; preserve TLS hostname/SNI, exact URL and no proxy/redirect/cookie boundaries. Fence remote sessions to an immutable host owner, deny ownerless/cross-owner remote access and pass that owner to credential composition. Route SSE as events arrive without altering protocol identity; bound controls and aggregate budgets across resumptions; enforce framing and cursor reset; carry an absolute startup/catalog deadline. Add deterministic tests before recording closure evidence.

## Final verification / disposition

- Final complete Python 3.14.7 suite: `(cd extensions/octet-mcp && python3 -m unittest discover -s tests -t . -v)` — **67 passed**, 15.461 s; `/tmp/octet-mcp-final-tests.log`.
- Repeat regression stress: `python3 -W error::ResourceWarning -m unittest tests.test_http_hardening -v` — **18 passed** on each of three consecutive runs (9.500 / 9.484 / 9.510 s); `/tmp/octet-mcp-repeat-{1,2,3}.log`. Logs contain no `ResourceWarning`, ignored exception or traceback.
- Minimum supported runtime: `/usr/bin/python3 -m unittest tests.test_http_hardening -v` — **18 passed**, 9.136 s, Python 3.9.6; `/tmp/octet-mcp-python39-hardening.log`.
- Python 3.9 complete-suite release-test failure above confirmed unchanged using `git show df5a7e80:extensions/octet-mcp/tests/test_release.py`; it is not a transport regression. Python 3.11+ requirement for that test is now documented, not skipped.
- `python3 -m compileall -q extensions/octet-mcp/octet_mcp extensions/octet-mcp/tests` — exit 0.
- `git diff --check -- extensions/octet-mcp docs/swarm-audit/EXECUTION-mcp.md` — exit 0. Reviewed changed manager/runtime/transport/docs and all new networking/ownership/test files. Other workers' paths were not edited.

**Disposition: partial; #179 must remain open and experimental.** Implemented local safeguards and 18 new deterministic tests spanning all nine recorded defects; kept process-owner activation, exact URL, no redirects/proxies/cookies, normal TLS validation, unavailable-by-default credentials, no ambiguous replay and stdio behavior. No external MCP server, real credential, publishing, Rust host qualification or live production claim.

### Nine-defect evidence map

All paths below are relative to `extensions/octet-mcp/`.

| Defect | Production evidence | Deterministic evidence |
| --- | --- | --- |
| 1: DNS rebinding / HTTPS SSRF | `octet_mcp/http_network.py:34`, `:59`, `:134`; `octet_mcp/streamable_http.py:1138` | `tests/test_http_hardening.py:51`, `:65`, `:75`, `:91`: special/mixed addresses, no secondary resolution, original-host SNI, real trusted TLS success, hostname/untrusted-cert rejection |
| 2: owner sharing | `octet_mcp/ownership.py:10`; `octet_mcp/manager.py:120`, `:178`, `:381`, `:740`; owner passed to adapter at `octet_mcp/streamable_http.py:1056` | `tests/test_http_hardening.py:355`: ownerless bootstrap allocates no worker/credential/network; session/instance/generation mismatch and argument spoof rejected |
| 3: DNS cleanup | `octet_mcp/http_network.py:59`; `_HttpOperation` at `octet_mcp/streamable_http.py:90`, close at `:473` | `tests/test_http_hardening.py:134`: real isolated blocking helpers killed/reaped on cancellation and close |
| 4: buffered peer identity | `octet_mcp/streamable_http.py:727` | `tests/test_http_hardening.py:160`, `:192`: server waits for method-not-found before terminal; numeric colliding and credential-equal peer IDs preserved; foreign response rejected |
| 5: control fanout | `octet_mcp/streamable_http.py:909`, `:984` | `tests/test_http_hardening.py:203`, `:229`, `:246`: admission cap, tracked shutdown, independent watchdog, aggregate action cap |
| 6: resetting aggregate budgets | operation counters at `octet_mcp/streamable_http.py:90`, SSE at `:727` | `tests/test_http_hardening.py:246`, `:249`, `:252`: control, bytes and events exhausted across POST+GET with one original POST |
| 7: truncated framing | `octet_mcp/http_network.py:102`; `octet_mcp/streamable_http.py:727`, `:1295`, `:1314` | `tests/test_http_hardening.py:119`, `:273`: valid chunked JSON/SSE accepted; short Content-Length, partial SSE, absent chunk/final trailer termination rejected |
| 8: empty cursor | operation-local commit/reset at `octet_mcp/streamable_http.py:727` | `tests/test_http_hardening.py:297`: explicit empty ID stops GETs, omitted ID preserves prior cursor |
| 9: startup deadline | `octet_mcp/streamable_http.py:239`, `:284` | `tests/test_http_hardening.py:327`, `:383`: controlled-clock initialized/catalog deadline; late credential callback cannot initiate I/O |

### Exact remaining closure gates

1. **Owner lifecycle/catalog integration is containment, not full shared-runtime support.** A remote resident binds once, initially via a trusted `/mcp` command; a different host owner requires a new process. Owner-specific host catalog visibility and automatic owner-settlement cleanup are not implemented/qualified. Foreign tool calls and commands fail closed instead of reusing sessions/credentials.
2. **Injected application callbacks are not killable.** Credential/progress callbacks must return promptly. A malicious or broken synchronous adapter can retain a bounded worker until return despite socket/DNS cancellation. The late-network fence is tested; guaranteed callback termination is not claimed. No stock credential broker exists.
3. **Qualification remains local.** Real loopback TLS and deterministic HTTP/SSE tests ran on macOS Python 3.14.7 and transport regressions on Python 3.9.6, not live external MCP services, Linux cleanup/resource-pressure environments, or Rust-host/Serve owner-switch integration. No release/publishing evidence exists for this candidate.
4. **Python 3.9 full-suite harness limitation:** unchanged release-manifest test needs `tomllib` from Python 3.11+. The entire 3.14 suite and all 3.9 transport regressions pass; the entire 3.9 suite does not.

## Diff identity / artifacts

Baseline: `df5a7e809715961b9344af6b52e43a6ca48f56b3` (`df5a7e80`).
Candidate patch SHA-256: **`1fb866d3364c0d573b50a938f2dba8e2ce7a33207e9c2330c1b27f0ad358a519`** (102,228 bytes).
Artifact: `/tmp/octet-mcp-df5a7e80.patch`.

Reconstruction: concatenate `git diff --binary df5a7e80 -- extensions/octet-mcp` with each `git diff --no-index --binary -- /dev/null PATH` for sorted `git ls-files --others --exclude-standard extensions/octet-mcp` paths, then SHA-256 those bytes. This includes the new source/tests/test-only TLS fixtures without staging any file. This execution record is excluded to avoid a self-referential digest. No commit/session reference was produced; the durable workspace artifact is this evidence file.
