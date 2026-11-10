// Test-only oracle: replay one scenario through the published, unmodified
// SessionManager. Imports narrow modules because the package root also loads
// optional UI/tool integrations. Reads only the fixture's own directories.
import assert from 'node:assert/strict';
import { copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const version = process.argv[2];
assert.ok(['1.0.2', '1.5.0'].includes(version));
const alias = `pi-session-${version.replaceAll('.', '-')}`;
const base = new URL(`./node_modules/${alias}/`, import.meta.url);
assert.equal(JSON.parse(readFileSync(new URL('package.json', base))).version, version);
const { SessionManager } = await import(new URL('dist/core/session-manager.js', base));
const { convertToLlm } = await import(new URL('dist/core/messages.js', base));
let input = '';
for await (const chunk of process.stdin) input += chunk;
const { cases, directory } = JSON.parse(input);

function snapshot(manager) {
  const entries = manager.getEntries();
  assert.equal(manager.getEntryCount(), entries.length);
  assert.equal(manager.getLeafEntry()?.id ?? null, manager.getLeafId());
  for (const entry of entries) {
    assert.deepEqual(manager.getEntry(entry.id), entry);
    assert.deepEqual(manager.getChildren(entry.id), entries.filter(e => e.parentId === entry.id));
    assert.equal(manager.getBranch(entry.id).at(-1).id, entry.id);
  }
  const treeOrder = [];
  const labels = {};
  const visit = nodes => {
    for (const node of nodes) {
      treeOrder.push(node.entry.id);
      assert.equal(node.label, manager.getLabel(node.entry.id));
      if (node.label !== undefined) labels[node.entry.id] = node.label;
      visit(node.children);
    }
  };
  visit(manager.getTree());
  const context = manager.buildSessionContext();
  const projection = manager.buildSessionProjection();
  assert.deepEqual(projection.messages, context.messages);
  assert.deepEqual(projection.entries.map(e => e.sourceEntry), manager.buildContextEntries());
  return {
    entries, treeOrder, labels, leaf: manager.getLeafId(),
    path: manager.getBranch().map(e => e.id),
    context: convertToLlm(context.messages),
    settings: {
      model: context.model ? `${context.model.provider}/${context.model.modelId}` : null,
      thinking: context.thinkingLevel,
    },
    name: manager.getSessionName() ?? null,
  };
}

const results = [];
for (const test of cases) {
  const root = join(directory, test.name);
  const cwd = join(root, 'work');
  const sessionDir = join(root, 'sessions');
  const checkpointDir = join(root, 'checkpoints');
  for (const path of [cwd, sessionDir, checkpointDir]) mkdirSync(path, { recursive: true });
  let manager = SessionManager.create(cwd, sessionDir);
  const ids = new Map();
  const slots = new Map();
  const steps = [];
  const target = name => {
    assert.ok(ids.has(name), `unknown logical entry ${name}`);
    return ids.get(name);
  };
  for (const [index, op] of test.operations.entries()) {
    let result;
    let error = null;
    try {
      switch (op.op) {
        case 'message': result = manager.appendMessage(op.message); break;
        case 'custom': result = manager.appendCustomEntry(op.customType, op.data); break;
        case 'custom_message': result = manager.appendCustomMessageEntry(op.customType, op.content, op.display, op.details); break;
        case 'model': result = manager.appendModelChange(op.provider, op.modelId); break;
        case 'thinking': result = manager.appendThinkingLevelChange(op.level); break;
        case 'usage': result = manager.appendUsage(op.kind, op.provider, op.model, op.usage, op.note).id; break;
        case 'label': result = manager.appendLabelChange(target(op.target), op.label); break;
        case 'name': result = manager.appendSessionInfo(op.name); break;
        case 'edit': result = manager.appendContextEdit(target(op.target), op.replacement); break;
        case 'branch': manager.branch(target(op.target)); break;
        case 'reset': manager.resetLeaf(); break;
        case 'summary': result = manager.branchWithSummary(op.target === null ? null : target(op.target), op.summary, op.details, op.fromHook, op.usage); break;
        case 'compaction': result = manager.appendCompaction(op.summary, op.firstKept === null ? null : target(op.firstKept), op.tokensBefore ?? 0, op.details, op.fromHook, op.usage); break;
        case 'reopen':
          // Pi keeps a setup-only session in memory and writes no file; there is
          // nothing on disk to reopen yet. Record that instead of creating one.
          if (!existsSync(manager.getSessionFile())) { error = 'unpersisted'; break; }
          manager = SessionManager.open(manager.getSessionFile(), sessionDir);
          break;
        case 'fork': {
          const path = manager.createBranchedSession(target(op.target));
          assert.ok(existsSync(path), 'a branched session with conversation is written');
          manager = SessionManager.open(path, sessionDir);
          break;
        }
        case 'fork_all': manager = SessionManager.forkFrom(manager.getSessionFile(), cwd, sessionDir); break;
        case 'new':
          slots.set(op.slot, manager.getSessionFile());
          manager.newSession();
          break;
        case 'switch': manager.setSessionFile(slots.get(op.slot)); break;
        case 'recent': manager = SessionManager.continueRecent(cwd, sessionDir); break;
        case 'probe': {
          assert.equal(SessionManager.findById(cwd, manager.getSessionId(), sessionDir), manager.getSessionFile());
          assert.ok((await SessionManager.list(cwd, sessionDir)).some(s => s.id === manager.getSessionId()));
          assert.ok((await SessionManager.listAll(sessionDir)).some(s => s.id === manager.getSessionId()));
          break;
        }
        case 'context': break;
        default: throw new Error(`unknown operation ${op.op}`);
      }
      if (op.as) {
        assert.equal(typeof result, 'string');
        ids.set(op.as, result);
      }
    } catch (e) {
      // Rejection semantics are part of the contract too.
      if (!op.reject) throw e;
      error = 'rejected';
    }
    if (op.reject) assert.equal(error, 'rejected', 'Pi accepted an expected-invalid operation');
    const aliases = Object.fromEntries([...ids].map(([name, id]) => [id, name]));
    const state = { ...snapshot(manager), aliases };
    let disk = null;
    const file = manager.getSessionFile();
    if (file && existsSync(file)) {
      const copy = join(checkpointDir, `${index}.jsonl`);
      copyFileSync(file, copy);
      const before = readFileSync(copy);
      disk = { state: { ...snapshot(SessionManager.open(copy, sessionDir)), aliases } };
      assert.deepEqual(readFileSync(copy), before, 'opening a current Pi file changed its bytes');
    }
    steps.push({ state, error, disk });
  }
  results.push({ name: test.name, file: manager.getSessionFile(), steps });
}
process.stdout.write(JSON.stringify({ version, results }));
