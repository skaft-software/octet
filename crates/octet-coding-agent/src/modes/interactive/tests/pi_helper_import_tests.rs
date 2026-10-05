//! Public Pi helper imports exercised inside the real Node/Rust command path.
#![cfg(unix)]
use super::*;
use super::pi_contract_support::{command, pi_app};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_public_helpers_load_and_persist_through_the_real_host() {
    let (_directory, mut app) = pi_app(r#"
      import { uuidv7 } from '@earendil-works/pi-ai';
      import { CONFIG_DIR_NAME, getAgentDir, isToolCallEventType, isReadToolResult, parseFrontmatter, truncateHead, convertToLlm,
        serializeConversation } from '@earendil-works/pi-coding-agent';
      export default pi => pi.registerCommand('probe', { handler: (_args, ctx) => {
        const custom = convertToLlm([{ role: 'custom', content: 'helper-text', details: { private: true }, timestamp: 3 }]);
        pi.appendEntry('helper-proof', {
          guard: isToolCallEventType('read', { toolName: 'read', input: { path: 'a' } }),
          resultGuard: isReadToolResult({ toolName: 'read' }),
          yaml: parseFrontmatter('---\ncount: 7\n---\nbody').frontmatter.count,
          head: truncateHead('first\nsecond', { maxLines: 1 }).content,
          summary: serializeConversation(custom), privateLeaked: 'details' in custom[0],
          config: CONFIG_DIR_NAME, agentDir: typeof getAgentDir() === 'string', uuid: uuidv7(123).slice(0, 13),
        });
      } });
    "#);
    let path = app.agent.session().path().to_owned();
    let mut shell = InteractiveShell::test_shell();
    let result = command(&mut app, &mut shell, "probe").await;
    app.executable_extensions.shutdown().await;
    result.unwrap();
    let reopened = Session::open_read_only(&path).unwrap();
    let entries = serde_json::to_value(reopened.entries()).unwrap().to_string();
    assert!(entries.contains("helper-proof"), "{entries}");
    assert!(entries.contains("\"config\":\".pi\""), "{entries}");
    assert!(entries.contains("00000000-007b"), "{entries}");
    assert!(entries.contains("\"guard\":true"), "{entries}");
    assert!(entries.contains("\"resultGuard\":true"), "{entries}");
    assert!(entries.contains("\"yaml\":7"), "{entries}");
    assert!(entries.contains("\"head\":\"first\""), "{entries}");
    assert!(entries.contains("[User]: helper-text"), "{entries}");
    assert!(entries.contains("\"privateLeaked\":false"), "{entries}");
}
