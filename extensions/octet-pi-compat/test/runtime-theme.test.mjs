import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync, symlinkSync, realpathSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { configure, routeExtensions } from '../configure.mjs';
import { planThemeImport, writeThemeImport } from '../lib/theme-import.mjs';
import { backgroundTokens, foregroundTokens } from '../lib/theme-palette.mjs';
import * as palettes from '../lib/theme.mjs';
import { launch } from './helper.mjs';

function fixture(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-runtime-theme-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const palette = (name, color) => ({ name, appearance: 'dark', colors: Object.fromEntries([
    ...foregroundTokens.map(token => [token, color]), ...backgroundTokens.map(token => [token, '']),
  ]) });
  const first = join(root, 'first.json'), second = join(root, 'second.json');
  writeFileSync(first, JSON.stringify(palette('ghostty-dark', '#c792ea')));
  writeFileSync(second, JSON.stringify(palette('changed-palette', '#123456')));
  return { root, first, second };
}
const purple = '\x1b[38;2;199;146;234m', changed = '\x1b[38;2;18;52;86m';

// Real reviewed capture + runtime process, component frames and renderer requests.
// No Pi CLI, provider/inference, native palette capability, or user settings.
test('reviewed imported palette reaches load-time helpers, retained editor/footer and all renderer slots', async t => {
  const { root, first, second } = fixture(t), entry = join(root, 'factory.mjs');
  writeFileSync(entry, `import {CustomEditor, getEditorTheme, getMarkdownTheme, getSelectListTheme} from '@earendil-works/pi-coding-agent';
import {Text} from '@earendil-works/pi-tui';
const binding = await import(${JSON.stringify(new URL('../lib/theme.mjs', import.meta.url).href)});
const early = getMarkdownTheme().heading('early-helper');
const editorHelper = getEditorTheme(), listHelper = getSelectListTheme();
export default pi => {
  // Palette-dependent metadata must match reviewed capture and live loading.
  pi.registerCommand('palette-probe', {description: Buffer.from(early).toString('hex'), handler(){}});
  pi.registerCommand('palette-swap', {handler(){binding.configureBridgeTheme({name:'changed-palette',path:${JSON.stringify(second)}});}});
  pi.registerMessageRenderer('color', (_message, _options, theme) => new Text(theme.fg('success', 'message'), 0, 0));
  pi.registerEntryRenderer('color', (_entry, _options, theme) => new Text(theme.fg('warning', 'entry'), 0, 0));
  pi.registerTool({name:'palette_tool',label:'palette tool',description:'palette fixture',parameters:{type:'object',properties:{}},
    async execute(){return {content:[{type:'text',text:'unused'}]};},
    renderCall(_args, theme, context){return context.lastComponent ?? new Text(theme.fg('toolTitle','tool-call'),0,0);},
    renderResult(_result,_options,theme,context){return context.lastComponent ?? new Text(theme.fg('toolOutput','tool-result'),0,0);}});
  pi.on('session_start', (_event, ctx) => {
    ctx.ui.setFooter((_tui, theme) => ({invalidate(){}, render(){return [theme.fg('accent','footer'), early,
      editorHelper.borderColor('editor-helper'), listHelper.selectedText('list-helper'), ctx.ui.theme.fg('muted','context-theme'),
      'palette-name=' + theme.name, 'palette-path=' + theme.sourcePath];}}));
    ctx.ui.setEditorComponent((tui, theme, keys) => {
      const color = theme.borderColor;
      return new (class extends CustomEditor {render(width){return [color('composer'), ...super.render(width)];}})(tui,theme,keys);
    });
  });
};`);
  const output = join(root, 'octet-pi-compat');
  const plan = planThemeImport({ output, paths: [first], selection: 'ghostty-dark', thinkingLevel: 'max' });
  // Like --from-pi: capture precedes publication of reviewed theme snapshots.
  const configured = configure({ output, reviewed: true, extensions: [entry], cwd: root,
    piTheme: plan.selected, themePaths: plan.themes.map(theme => theme.nativePath) });
  writeThemeImport(plan);
  const peer = launch(t, [entry], { config: join(output, 'bridge.json'), cwd: root, columns: 160 });
  peer.metadata.hooks = configured.registrations.hooks;
  await peer.init(['remote_ui', 'composer', 'editor_handoff', 'transcript_render_v1', 'session_entries', 'resource_paths_v1', 'message_injection', 'shortcuts']);
  await peer.start();
  const frame = marker => peer.wait(value => value.method === 'ui/frame' && value.params.lines.some(line => line.includes(marker)));
  const footer = (await frame('footer')).params.lines.join('\n');
  for (const marker of ['footer', 'early-helper', 'editor-helper', 'list-helper', 'context-theme']) assert.ok(footer.includes(purple + marker), footer);
  assert.match(footer, /palette-name=ghostty-dark/);
  assert.ok(footer.includes('palette-path=' + plan.selected.path));
  const editor = (await frame('composer')).params.lines.join('\n');
  assert.ok(editor.includes(purple + 'composer'), editor);
  const render = async kind => {
    const content = kind === 'tool'
      ? {kind, name:'palette_tool', arguments:{}, result:{content:[{type:'text',text:'canonical'}]}, expanded:false,
        is_partial:false, is_error:false, execution_started:true, args_complete:true, show_images:false}
      : kind === 'message' ? {kind, message:{role:'custom', customType:'color', content:'canonical', details:{}, timestamp:1}, expanded:false, output_pad:0}
        : {kind, entry:{type:'custom', id:'entry', customType:'color', data:{}, timestamp:1}, expanded:false};
    const response = await peer.request('transcript/render', {source_id:kind, width:80, render:content, context:peer.context()}).response;
    assert.ok(response.result, JSON.stringify(response));
    return response.result.lines.join('\n');
  };
  for (const kind of ['message', 'entry', 'tool']) assert.ok((await render(kind)).includes(purple), kind);
  const bridgeBefore = readFileSync(join(output, 'bridge.json'), 'utf8');
  assert.ok((await peer.command('palette-swap').response).result);
  // Same source IDs, data, width and lastComponent callbacks: stale palette
  // caches must be retired rather than accidentally passing on a changed key.
  for (const kind of ['message', 'entry', 'tool']) {
    const lines = await render(kind);
    assert.ok(lines.includes(changed), lines); assert.ok(!lines.includes(purple), lines);
  }
  const updated = (await frame('footer')).params.lines.join('\n');
  assert.ok(updated.includes(changed + 'footer'), updated);
  assert.ok(updated.includes(changed + 'editor-helper'), updated);
  assert.ok(updated.includes(changed + 'context-theme'), updated);
  assert.equal(readFileSync(join(output, 'bridge.json'), 'utf8'), bridgeBefore);
  assert.ok(!peer.seen.some(value => value.method === 'theme/select'), 'bridge palette binding never claims a native theme-selection service');
  await peer.close();
});

test('individual route probes see the same planned palette before import publication', async t => {
  const { root, first } = fixture(t), entry = join(root, 'probe.mjs');
  writeFileSync(entry, `import {getMarkdownTheme} from '@earendil-works/pi-coding-agent';
export default pi => {
  if (!getMarkdownTheme().heading('probe').startsWith(${JSON.stringify(purple)})) throw Error('probe did not receive imported palette');
  pi.registerCommand('probe', {handler(){}});
};`);
  const plan = planThemeImport({ output: join(root, 'octet-pi-compat'), paths: [first], selection: 'ghostty-dark' });
  const [result] = await routeExtensions([entry], { cwd: root, installed: false, piTheme: plan.selected });
  assert.equal(result.route, 'shims', result.error);
});

test('palette binding is bounded, data-only and keeps retained theme helpers live', t => {
  const { first, second, root } = fixture(t);
  assert.equal(typeof palettes.configureBridgeTheme, 'function');
  t.after(() => palettes.configureBridgeTheme());
  const retained = palettes.theme, border = retained.borderColor, list = retained.selectList;
  palettes.configureBridgeTheme({ name: 'ghostty-dark', path: first });
  assert.equal(palettes.theme, retained);
  assert.equal(retained.name, 'ghostty-dark');
  assert.equal(retained.sourcePath, first);
  assert.ok(border('border').startsWith(purple));
  assert.ok(list.selectedText('selection').startsWith(purple));
  const thinking = retained.getThinkingBorderColor('max');
  palettes.configureBridgeTheme({ name: 'changed-palette', path: second });
  assert.ok(thinking('thinking').startsWith(changed));
  assert.ok(border('border').startsWith(changed));
  assert.ok(list.selectedText('selection').startsWith(changed));
  const link = join(root, 'link.json'); symlinkSync(first, link);
  assert.throws(() => palettes.configureBridgeTheme({ name: 'ghostty-dark', path: link }));
  assert.throws(() => palettes.configureBridgeTheme({ name: 'wrong', path: first }), /name/);
  assert.throws(() => palettes.configureBridgeTheme({ name: 'ghostty-dark', path: 'relative.json' }), /absolute/);
  const large = join(root, 'large.json'); writeFileSync(large, Buffer.alloc(262145));
  assert.throws(() => palettes.configureBridgeTheme({ name: 'large', path: large }), /bounds_exceeded/);
  assert.equal(retained.name, 'changed-palette', 'bad input never silently selects a fabricated fallback');
  palettes.configureBridgeTheme();
  assert.equal(retained.name, 'octet-pi-compat', 'manual configurations without a Pi import keep the compatibility palette');
});

test('an invalid configured palette refuses startup before any factory executes', t => {
  const { root, first } = fixture(t), entry = join(root, 'factory.mjs'), marker = join(root, 'executed');
  // The factory writes beside itself, so its source embeds no path.
  writeFileSync(entry, "import {writeFileSync} from 'node:fs'; export default () => writeFileSync(new URL('./executed', import.meta.url),'executed');");
  const link = join(root, 'link.json'); symlinkSync(first, link);
  const config = join(root, 'bridge.json');
  writeFileSync(config, JSON.stringify({ extensions:[entry], pi_theme:{name:'ghostty-dark',path:link} }));
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('../runner.mjs', import.meta.url)), '--config', config, '--inspect'],
    { cwd:root, encoding:'utf8', timeout:10000 });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /\[pi-compat startup\]/);
  assert.ok(!existsSync(marker));
});

test('reviewed installed-Pi 1.0.2 highlight helper uses the imported palette', { skip: !process.env.PI_THEME_AGENT_DIR }, t => {
  const { root, first } = fixture(t), entry = join(root, 'installed.mjs');
  writeFileSync(entry, `import {highlightCode} from '@earendil-works/pi-coding-agent';
export default pi => pi.registerCommand('installed-color', {description: Buffer.from(highlightCode('const x = 1;', 'js').join('')).toString('hex'),handler(){}});`);
  const output = join(root, 'octet-pi-compat');
  const plan = planThemeImport({ output, paths: [first], selection: 'ghostty-dark' });
  const configured = configure({ output, reviewed: true, extensions: [entry], cwd: root,
    env: { ...process.env, HOME: root }, piAgentDir: process.env.PI_THEME_AGENT_DIR,
    routes: { [entry]: 'installed' }, piTheme: plan.selected, themePaths: plan.themes.map(theme => theme.nativePath) });
  writeThemeImport(plan);
  const expected = configured.registrations.commands[0].description;
  assert.ok(Buffer.from(expected, 'hex').toString().includes(purple));
  const live = spawnSync(process.execPath, [fileURLToPath(new URL('../runner.mjs', import.meta.url)), '--config', join(output, 'bridge.json'), '--inspect'],
    { cwd: root, env: { ...process.env, HOME: root }, encoding: 'utf8', timeout: 20000 });
  assert.equal(live.status, 0, live.stderr);
  assert.equal(JSON.parse(live.stdout).result.commands[0].description, expected);
});

test('installed Pi helper theme globals share the bridge palette without calling Pi initTheme', t => {
  const { first, second } = fixture(t);
  assert.equal(typeof palettes.bindInstalledPiTheme, 'function');
  t.after(() => palettes.configureBridgeTheme());
  palettes.configureBridgeTheme({ name: 'ghostty-dark', path: first });
  palettes.bindInstalledPiTheme();
  const current = globalThis[Symbol.for('@earendil-works/pi-coding-agent:theme')];
  const old = globalThis[Symbol.for('@mariozechner/pi-coding-agent:theme')];
  assert.equal(current, palettes.theme); assert.equal(old, current);
  assert.ok(current.fg('syntaxKeyword', 'keyword').startsWith(purple));
  palettes.configureBridgeTheme({ name: 'changed-palette', path: second });
  assert.ok(current.fg('syntaxKeyword', 'keyword').startsWith(changed));
});
