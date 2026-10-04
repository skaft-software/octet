import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { stripTypeScriptTypes } from 'node:module';
import { mkdtemp, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Theme, createThemeFromJson, loadThemeFromPath, theme } from '../lib/theme.mjs';
import { backgroundTokens, foregroundTokens, parsePiTheme, piThemeToNativeToml } from '../lib/theme-palette.mjs';
import { colorToHex, parseColor } from '../lib/theme-colors.mjs';

const fixture = () => ({ name: 'Palette 😀', appearance: 'dark',
  vars: { primary: 'secondary', secondary: '#0af', bg: 'okhsl(250 60% 55%)' },
  colors: Object.fromEntries([...foregroundTokens.map(name => [name, name === 'text' ? '' : 'primary']), ...backgroundTokens.map(name => [name, 'bg'])]),
});

test('Pi JSON palette resolves references, all color forms and exact optional-token fallbacks', () => {
  const input = fixture();
  input.colors.accent = 'oklch(62% 0.1 200)'; input.colors.border = 255; input.colors.thinkingXhigh = 5;
  for (const token of ['scrollbarTrack', 'scrollbarThumb', 'thinkingMax', 'searchMatchBg', 'searchMatchText']) delete input.colors[token];
  const palette = parsePiTheme('\uFEFF' + JSON.stringify(input));
  assert.equal(palette.colors.muted, '#0af');
  assert.equal(palette.colors.scrollbarTrack, palette.colors.muted);
  assert.equal(palette.colors.scrollbarThumb, '');
  assert.equal(palette.colors.thinkingMax, 5);
  assert.equal(palette.colors.searchMatchBg, palette.colors.selectedBg);
  assert.equal(palette.colors.searchMatchText, '');
  assert.ok(Object.isFrozen(palette.colors));
  assert.equal(input.colors.muted, 'primary', 'caller data is not mutated');
  assert.equal(createThemeFromJson(input).getFgAnsi('text'), '\x1b[39m');
});

test('palette schema/bounds reject malformed colors, invalid references and misleading formats', () => {
  for (const change of [
    input => { input.vars.primary = 'primary'; },
    input => { input.vars.primary = 'missing'; },
    input => { input.vars.primary = 'toString'; },
    input => { input.colors.accent = 256; },
    input => { input.colors.accent = -1; },
    input => { input.colors.accent = 1.5; },
    input => { input.colors.accent = '#ffff'; },
    input => { input.colors.accent = 'okhsl(0 150% 50%)'; },
    input => { input.colors.accent = 'oklch(NaN 0.1 0)'; },
    input => { input.colors.accent = '\x1b[31m'; },
    input => { input.colors.accent = null; },
    input => { input.colors.unknown = ''; },
    input => { delete input.colors.accent; },
    input => { input.name = 'bad/name'; },
    input => { input.name = '\n'; },
    input => { input.name = '\ud800'; },
    input => { input.appearance = 'auto'; },
    input => { input.export = { pageBg: 'bad' }; },
    input => { input.vars = Array(257).fill(''); },
    input => { input.vars = Object.fromEntries(Array.from({ length: 257 }, (_, i) => [i, ''])); },
    input => { input.extra = true; },
  ]) {
    const input = fixture(); change(input); assert.throws(() => parsePiTheme(input));
  }
  assert.throws(() => parsePiTheme('[metadata]\nname = "Native"'), /JSON, not native TOML/);
  assert.throws(() => parsePiTheme(' '.repeat(262145)), /bounds_exceeded/);
});

test('native conversion labels TOML honestly, preserves index/default colors, and reports unsupported role/export projection', () => {
  const input = fixture(); input.colors.accent = 12; input.export = { pageBg: '#fff' };
  const converted = piThemeToNativeToml(input);
  assert.equal(converted.sourceFormat, 'pi-theme-json'); assert.equal(converted.format, 'octet-theme-toml');
  assert.match(converted.toml, /foreground = "default"/);
  assert.match(converted.toml, /accent = "index:12"/);
  assert.match(converted.toml, /border = "#00aaff"/);
  assert.match(converted.toml, new RegExp(`selected_bg = "${colorToHex(parseColor(input.vars.bg))}"`));
  assert.match(converted.toml, /adaptive = false/);
  assert.ok(converted.unmappedColors.includes('thinkingMax'));
  assert.ok(converted.unmappedColors.includes('customMessageText'));
  assert.deepEqual(converted.unmappedExport, ['pageBg']);
  assert.throws(() => parsePiTheme(converted.toml), /not native TOML/);
  input.name = 'n'.repeat(81); assert.throws(() => piThemeToNativeToml(input), /bounds_exceeded native theme name/);
});

test('Theme facade renders pinned color/style methods without acquiring the terminal or mutating the host palette', () => {
  const input = fixture(), loaded = createThemeFromJson(input);
  assert.ok(loaded instanceof Theme); assert.equal(loaded.name, input.name); assert.equal(loaded.getColorMode(), 'truecolor');
  assert.equal(loaded.fg('accent', 'x'), '\x1b[38;2;0;170;255mx\x1b[39m');
  assert.equal(loaded.bg('selectedBg', 'x'), loaded.getBgAnsi('selectedBg') + 'x\x1b[49m');
  assert.equal(loaded.getThinkingBorderColor('max')('x'), loaded.fg('thinkingMax', 'x'));
  assert.equal(loaded.getThinkingBorderColor('unknown')('x'), loaded.fg('thinkingOff', 'x'));
  assert.equal(loaded.getBashModeBorderColor()('x'), loaded.fg('bashMode', 'x'));
  assert.equal(loaded.style('x', { fg: 'accent', bg: 'selectedBg', bold: true }), loaded.getFgAnsi('accent') + loaded.getBgAnsi('selectedBg') + '\x1b[1mx\x1b[22m\x1b[49m\x1b[39m');
  assert.throws(() => loaded.fg('selectedBg', 'x'), /incorrect foreground\/background slot/);
  assert.throws(() => loaded.bg('accent', 'x'), /incorrect foreground\/background slot/);
  assert.throws(() => loaded.style('x', { fg: { kind: 'indexed', index: 256 } }), /0 to 255/);
  assert.throws(() => loaded.style('x', { bold: 'yes' }), /theme style bold/);
  assert.equal(theme.name, 'octet-pi-compat', 'loading a data palette does not select it in the native host');
  for (const role of foregroundTokens) assert.match(theme.getFgAnsi(role), /^\x1b\[/);
  for (const role of backgroundTokens) assert.match(theme.getBgAnsi(role), /^\x1b\[/);
  const fg = Object.fromEntries(foregroundTokens.map(name => [name, input.colors[name]]));
  const bg = Object.fromEntries(backgroundTokens.map(name => [name, input.colors[name]]));
  // Constructor takes resolved colors, not JSON variable references.
  for (const key of Object.keys(fg)) fg[key] = '#abc'; for (const key of Object.keys(bg)) bg[key] = '';
  const dim = new Theme(fg, bg, '256color', { dim: ['accent'], appearance: 'dark' });
  assert.match(dim.fg('accent', 'x'), /\x1b\[2mx\x1b\[22;39m$/);
  assert.equal(dim.bold(dim.bold('x')), '\x1b[1m\x1b[1mx\x1b[1m\x1b[22m');
});

test('explicit theme file loading is bounded/no-follow and rejects native TOML, directories and invalid UTF-8', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-pi-theme-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'palette.json'); await writeFile(path, JSON.stringify(fixture()));
  assert.equal(loadThemeFromPath(path).sourcePath, path);
  const link = join(dir, 'link.json'); await symlink(path, link);
  assert.throws(() => loadThemeFromPath(link)); assert.throws(() => loadThemeFromPath(dir), /regular file/);
  await writeFile(path, '[metadata]\nname = "native"'); assert.throws(() => loadThemeFromPath(path), /not native TOML/);
  await writeFile(path, Buffer.from([0xc0, 0xaf])); assert.throws(() => loadThemeFromPath(path), /UTF-8/);
  await writeFile(path, Buffer.alloc(262145)); assert.throws(() => loadThemeFromPath(path), /bounds_exceeded/);
});

const repo = process.env.PI_REFERENCE_REPO;
test('color conversion and palette fallbacks match hash-verified pinned Pi 1.0 source, including built-in dark/light JSON', {
  skip: !repo && 'set PI_REFERENCE_REPO to the reviewed offline Pi checkout',
}, async () => {
  const ref = '581e7ba78141a4d8b61cc9d11b8b22ae7e59195e';
  const source = path => execFileSync('git', ['-C', repo, 'show', `${ref}:${path}`], { encoding: 'utf8' });
  const math = source('packages/tui/src/oklab.ts'), colors = source('packages/tui/src/colors.ts');
  assert.equal(createHash('sha256').update(math).digest('hex'), '45b067e6e3605b385f595adecd7c0216f1c6b6686680d5c73f661286de736be6');
  assert.equal(createHash('sha256').update(colors).digest('hex'), 'd4fe729c424d2c07bc64cf0c3edfdbf5642865cba395dfb37234c6c88d65f468');
  const url = code => `data:text/javascript;base64,${Buffer.from(code).toString('base64')}`;
  const mathUrl = url(stripTypeScriptTypes(math));
  const oracle = await import(url(stripTypeScriptTypes(colors).replace('"./oklab.ts"', JSON.stringify(mathUrl))));
  for (const value of [0, 1, 15, 16, 42, 231, 255, '#0af', '#Ab12eF', 'oklch(62% 0.1 200)', 'oklch(100% 0.3 150)', 'okhsl(250 60% 55%)', 'okhsl(-90deg 1 0.5)']) {
    assert.equal(colorToHex(parseColor(value)), oracle.colorToHex(oracle.parseColor(value)), String(value));
  }
  const themeSource = source('packages/coding-agent/src/modes/interactive/theme/theme.ts');
  const pure = themeSource.slice(themeSource.indexOf('function resolveVarRefs('), themeSource.indexOf('// Appearance & Terminal Default Colors'));
  const paletteOracle = await import(url(stripTypeScriptTypes(pure) + '\nexport { resolveThemeColors, withThemeColorFallbacks };'));
  for (const name of ['dark', 'light']) {
    const input = JSON.parse(source(`packages/coding-agent/src/modes/interactive/theme/${name}.json`));
    assert.deepEqual(parsePiTheme(input).colors, paletteOracle.resolveThemeColors(paletteOracle.withThemeColorFallbacks(input.colors), input.vars));
    const loaded = createThemeFromJson(input), converted = piThemeToNativeToml(input);
    for (const token of foregroundTokens) {
      const value = parsePiTheme(input).colors[token];
      assert.equal(loaded.getFgAnsi(token), value === '' ? '\x1b[39m' : oracle.foregroundAnsi(oracle.parseColor(value), 'truecolor'));
    }
    assert.match(converted.toml, /\[colors\]/);
  }
});
