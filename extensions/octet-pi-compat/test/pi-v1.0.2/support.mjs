import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createJiti } from 'jiti';
import { fileURLToPath } from 'node:url';
import { Editor } from '../../shims/tui.mjs';

const local = path => fileURLToPath(new URL(path, import.meta.url));
const pi = path => local(`../../node_modules/@earendil-works/pi-tui/dist/${path}.js`);

// Only imports change. The pinned suites themselves are byte-for-byte copies;
// in particular their timers, assertions, callbacks and input bytes are intact.
export async function runEditorSuite(name) {
  const provenance = JSON.parse(readFileSync(new URL('./provenance.json', import.meta.url), 'utf8'));
  const source = local(`./test/${name}.test.ts`);
  assert.equal(createHash('sha256').update(readFileSync(source)).digest('hex'),
    provenance.suites[name].sha256, 'the pinned upstream suite must not be weakened or rewritten');
  const jiti = createJiti(import.meta.url, {
    fsCache: false,
    alias: {
      '../src/components/editor.ts': local('./subject.mjs'),
      '../src/autocomplete.ts': pi('autocomplete'),
      '../src/keybindings.ts': pi('keybindings'),
      '../src/utils.ts': pi('utils'),
      '../src/tui-main-screen.ts': local('./fixtures.mjs'),
      './test-themes.ts': local('./fixtures.mjs'),
      './virtual-terminal.ts': local('./fixtures.mjs'),
    },
  });
  const subject = await jiti.import('../src/components/editor.ts');
  assert.equal(subject.Editor, Editor, 'upstream cases must target the actual adapter Editor export');
  await jiti.import(source);
}
