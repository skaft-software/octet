//! Public Pi helper imports exercised inside the real Node/Rust command path.
#![cfg(unix)]
use super::*;
use super::pi_contract_support::{command, pi_app};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_public_helpers_load_and_persist_through_the_real_host() {
    let (_directory, mut app) = pi_app(r#"
      import { uuidv7, calculateCost, collapseSystemMessages, createAssistantMessageEventStream,
        getCurrentSystemPrompt, getCurrentTools } from '@earendil-works/pi-ai';
      import { VERSION, CONFIG_DIR_NAME, getAgentDir, isToolCallEventType, isReadToolResult, parseFrontmatter, truncateHead, convertToLlm,
        serializeConversation } from '@earendil-works/pi-coding-agent';
      export default pi => pi.registerCommand('probe', { handler: async (_args, ctx) => {
        const custom = convertToLlm([{ role: 'custom', content: 'helper-text', details: { private: true }, timestamp: 3 }]);
        // Caller-owned data only: this exercises the official custom-provider
        // example's pure helpers without registering/calling any provider.
        const usage = { input: 100000, output: 200000, cacheRead: 300000, cacheWrite: 400000,
          cacheWrite1h: 250000, totalTokens: 1000000,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
        const cost = usage.cost;
        const priced = calculateCost({ cost: { input: 2, output: 4, cacheRead: 0.5, cacheWrite: 3,
          tiers: [{ inputTokensAbove: 500000, input: 10, output: 20, cacheRead: 1, cacheWrite: 15 }] } }, usage);
        const a = { name: 'a', description: 'a', parameters: { type: 'object', properties: {} } };
        const b = { ...a, name: 'b' }, replacement = { ...a, description: 'replacement' };
        const user = { role: 'user', content: 'helper-user', timestamp: 2 };
        const messages = [
          { role: 'system', content: [{ type: 'text', text: 'base' }, { type: 'text', text: 'α' }],
            sections: { tone: 'old', removed: 'gone' }, toolsAdded: [a, b], timestamp: 0 },
          user,
          { role: 'system', content: 'later', sections: { tone: 'new', removed: null, format: 'format' },
            toolsRemoved: [{ name: 'a' }], toolsAdded: [replacement], timestamp: 3 },
        ];
        const before = JSON.stringify(messages), current = getCurrentTools(messages);
        const collapsed = collapseSystemMessages({ messages, extra: 'not propagated' });
        const answer = { role: 'assistant', content: [{ type: 'text', text: 'helper-answer' }],
          api: 'anthropic-messages', provider: 'fixture', model: 'fixture', usage, stopReason: 'stop', timestamp: 4 };
        const stream = createAssistantMessageEventStream(), delta = { type: 'text_delta', contentIndex: 0, delta: 'hi', partial: answer };
        const done = { type: 'done', reason: 'stop', message: answer };
        const result = stream.result();
        stream.push(delta); stream.push(done); stream.push({ type: 'late' });
        const events = []; for await (const event of stream) events.push(event);
        const errorStream = createAssistantMessageEventStream();
        const error = { ...answer, stopReason: 'error', errorMessage: 'fixture-error' };
        errorStream.push({ type: 'error', reason: 'error', error });
        const ended = createAssistantMessageEventStream(), waiting = ended[Symbol.asyncIterator]().next();
        ended.end(answer);
        const endedNext = await waiting;
        const ai = {
          cost: priced, costIdentity: priced === cost, totalTokens: usage.totalTokens,
          prompt: getCurrentSystemPrompt(messages), toolNames: current.map(tool => tool.name),
          toolIdentity: current[0] === b && current[1] === replacement,
          collapsed: { roles: collapsed.messages.map(message => message.role), timestamp: collapsed.messages[0].timestamp,
            content: collapsed.messages[0].content, sections: collapsed.messages[0].sections,
            userIdentity: collapsed.messages[1] === user, envelopeKeys: Object.keys(collapsed) },
          replayStable: getCurrentSystemPrompt(collapsed.messages) === getCurrentSystemPrompt(messages),
          unchanged: JSON.stringify(messages) === before,
          stream: { types: events.map(event => event.type), eventIdentity: events[0] === delta && events[1] === done,
            resultIdentity: await result === answer, stablePromise: stream.result() === result,
            errorIdentity: await errorStream.result() === error,
            endIdentity: await ended.result() === answer, ended: endedNext.done },
        };
        pi.appendEntry('helper-proof', {
          ai,
          guard: isToolCallEventType('read', { toolName: 'read', input: { path: 'a' } }),
          resultGuard: isReadToolResult({ toolName: 'read' }),
          yaml: parseFrontmatter('---\ncount: 7\n---\nbody').frontmatter.count,
          head: truncateHead('first\nsecond', { maxLines: 1 }).content,
          summary: serializeConversation(custom), privateLeaked: 'details' in custom[0],
          version: VERSION, config: CONFIG_DIR_NAME, agentDir: typeof getAgentDir() === 'string', uuid: uuidv7(123).slice(0, 13),
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
    assert!(entries.contains("\"version\":\"1.0.2\""), "{entries}");
    assert!(entries.contains("\"config\":\".pi\""), "{entries}");
    assert!(entries.contains("00000000-007b"), "{entries}");
    assert!(entries.contains("\"guard\":true"), "{entries}");
    assert!(entries.contains("\"resultGuard\":true"), "{entries}");
    assert!(entries.contains("\"yaml\":7"), "{entries}");
    assert!(entries.contains("\"head\":\"first\""), "{entries}");
    assert!(entries.contains("[User]: helper-text"), "{entries}");
    assert!(entries.contains("\"privateLeaked\":false"), "{entries}");
    let proofs: Vec<_> = reopened.entries().iter().filter_map(|entry| {
        reopened.extension_entry(&entry.id, "octet-pi-compat")
            .filter(|private| private.entry_type == "helper-proof")
    }).collect();
    assert_eq!(proofs.len(), 1, "exactly one durable helper command receipt");
    // Assert the computed values in the reopened native session, not merely
    // import/export presence or a JS-only/synthetic-host observation.
    assert_eq!(proofs[0].data["ai"], serde_json::json!({
        "cost": { "input": 1, "output": 4, "cacheRead": 0.3, "cacheWrite": 7.25, "total": 12.55 },
        "costIdentity": true, "totalTokens": 1000000,
        "prompt": "base\nα\n\nlater\n\nnew\n\nformat", "toolNames": ["b", "a"], "toolIdentity": true,
        "collapsed": { "roles": ["system", "user"], "timestamp": 0, "content": "base\nα\n\nlater",
            "sections": { "tone": "new", "format": "format" }, "userIdentity": true, "envelopeKeys": ["messages"] },
        "replayStable": true, "unchanged": true,
        "stream": { "types": ["text_delta", "done"], "eventIdentity": true, "resultIdentity": true,
            "stablePromise": true, "errorIdentity": true, "endIdentity": true, "ended": true },
    }));
}
