// Offline, hash-verified Pi 1.0.2 oracle. Never imports Octet's implementation.
// Usage: node oracle.mjs /path/to/reviewed/pi --write | --check
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { resolve } from 'node:path';

const repo = process.argv[2];
assert.ok(repo, 'supply the reviewed, offline Pi checkout');
const commit = 'cd32f7725fdbddbaecdff5b1e68491563394e0ca';
const hashes = {
  'packages/tui/src/oklab.ts': '45b067e6e3605b385f595adecd7c0216f1c6b6686680d5c73f661286de736be6',
  'packages/tui/src/colors.ts': 'd4fe729c424d2c07bc64cf0c3edfdbf5642865cba395dfb37234c6c88d65f468',
  'packages/coding-agent/src/modes/interactive/theme/system-theme.ts': '877a3dc24fd194f2dc5ee9efe8f1699acdcdac6defa197009bc7b39920bbb729',
};
for (const path of ['packages/tui/package.json', 'packages/coding-agent/package.json']) {
  assert.equal(JSON.parse(readFileSync(resolve(repo, path), 'utf8')).version, '1.0.2', path);
}
const source = path => {
  const text = readFileSync(resolve(repo, path), 'utf8');
  assert.equal(createHash('sha256').update(text).digest('hex'), hashes[path], path);
  return stripTypeScriptTypes(text);
};
const url = text => `data:text/javascript;base64,${Buffer.from(text).toString('base64')}`;
const mathUrl = url(source('packages/tui/src/oklab.ts'));
const colorsUrl = url(source('packages/tui/src/colors.ts').replace('"./oklab.ts"', JSON.stringify(mathUrl)));
const bridgeUrl = url(`export * from ${JSON.stringify(colorsUrl)}; export { oklabToOkhslLightness } from ${JSON.stringify(mathUrl)};`);
const extraExports = '\nexport { FAMILIES, TOKEN_FAMILIES, TOKEN_SLOTS, LEVELS, RULES, SOLVE_ORDER, PANELS, READABLE_FLOOR, FOREGROUND_LEVEL, FOREGROUND_TOKENS };\n';
const systemUrl = url(source('packages/coding-agent/src/modes/interactive/theme/system-theme.ts')
  .replace('"@earendil-works/pi-tui"', JSON.stringify(bridgeUrl)) + extraExports);
const upstream = await import(systemUrl).catch(error => {
  console.error(error.name, error.message.replace(/data:text\/javascript;base64,[A-Za-z0-9+/=]+/g, '<verified upstream module>'));
  process.exit(1);
});
const rgb = hex => ({r: parseInt(hex.slice(1, 3), 16), g: parseInt(hex.slice(3, 5), 16), b: parseInt(hex.slice(5, 7), 16)});
const dracula = {
  background: rgb('#282a36'), foreground: rgb('#f8f8f2'),
  palette: ['#21222c', '#ff5555', '#50fa7b', '#f1fa8c', '#bd93f9', '#ff79c6', '#8be9fd', '#f8f8f2',
    '#6272a4', '#ff6e6e', '#69ff94', '#ffffa5', '#d6acff', '#ff92df', '#a4ffff', '#ffffff'].map(rgb),
};
const frappe = {
  background: rgb('#303446'), foreground: rgb('#c6d0f5'),
  palette: ['#51576d', '#e78284', '#a6d189', '#e5c890', '#8caaee', '#f4b8e4', '#81c8be', '#b5bfe2',
    '#626880', '#e67172', '#8ec772', '#d9ba73', '#7b9ef0', '#f2a4db', '#5abfb5', '#a5adce'].map(rgb),
};
const inputs = [
  ['dracula', dracula], ['solarizedLight', {background: rgb('#fdf6e3'), foreground: rgb('#657b83')}],
  ['backgroundOnly', {background: rgb('#1e1e1e')}], ['midGray', {background: rgb('#808080'), foreground: rgb('#ffffff')}],
  ['frappe', frappe], ['referenceDark', {background: rgb('#000000'), foreground: rgb('#e5e5e7')}],
  ['referenceLight', {background: rgb('#ffffff'), foreground: rgb('#000000')}],
  ['indexedUnknown', {}], ['indexedLight', {appearanceHint: 'light'}], ['indexedDark', {appearanceHint: 'dark'}],
  ['foregroundWithoutBackground', {foreground: rgb('#ffffff')}], ['paletteWithoutBackground', {palette: dracula.palette}],
  ['unreadableDarkForeground', {background: rgb('#282a36'), foreground: rgb('#30323c')}],
  ['unreadableLightForeground', {background: rgb('#ffffff'), foreground: rgb('#eeeeee')}],
  ['oppositeAppearanceHint', {...dracula, appearanceHint: 'light'}],
  ['paperBackgroundOnly', {background: rgb('#f7f7f5')}],
];
for (const saturation of [-0.5, 0, 0.35, 0.8, 1, 2]) {
  for (const [name, input] of [['dracula', dracula], ['frappe', frappe], ['indexed', {}]]) {
    inputs.push([`${name}Saturation${saturation}`, {...input, saturation}]);
  }
}
for (const value of [0, 17, 34, 51, 68, 85, 102, 117, 118, 119, 120, 127, 128, 137, 153, 170, 187, 204, 221, 238, 255]) {
  const background = {r: value, g: value, b: value};
  inputs.push([`gray${value}`, {background}]);
  inputs.push([`gray${value}WhiteForeground`, {background, foreground: rgb('#ffffff')}]);
  inputs.push([`gray${value}BlackForeground`, {background, foreground: rgb('#000000')}]);
}
for (const hex of ['#011d2f', '#3e1820', '#214326', '#514535', '#b7bec8', '#ffeeee', '#d9f5ef']) {
  inputs.push([`coloredBackground${hex}`, {background: rgb(hex)}]);
  inputs.push([`frappeBackground${hex}`, {...frappe, background: rgb(hex)}]);
}
for (const slot of [1, 2, 3, 4, 5, 6, 8, 13]) {
  const palette = dracula.palette.map(c => ({...c}));
  palette[slot] = rgb('#9ac4b8');
  inputs.push([`mutatedPaletteSlot${slot}`, {...dracula, palette}]);
}
const tuple = ({r,g,b}) => [r,g,b];
const vectors = inputs.map(([name, input]) => ({name, input: {
  ...(input.background && {background: tuple(input.background)}),
  ...(input.foreground && {foreground: tuple(input.foreground)}),
  ...(input.palette && {palette: input.palette.map(tuple)}),
  ...(input.saturation !== undefined && {saturation: input.saturation}),
  ...(input.appearanceHint && {appearanceHint: input.appearanceHint}),
}, expected: upstream.generateSystemThemeColors(input)}));
const fixture = {version: '1.0.2', commit, hashes, provenance: 'Reviewed version/hash contract; no Git operation is performed by this oracle.', vectors};

// Generate only recipe constants (not the solver) from the same reviewed source.
const familyNames = Object.keys(upstream.FAMILIES);
const tokenNames = Object.keys(upstream.TOKEN_FAMILIES);
const levelNames = Object.keys(upstream.LEVELS);
const ti = name => name === 'background' ? tokenNames.length : tokenNames.indexOf(name);
const f = number => Number.isInteger(number) ? `${number}.0` : String(number);
const recipe = `// Generated by oracle.mjs from hash-verified Pi 1.0.2 system-theme.ts.\n// Copyright (c) 2025 Mario Zechner. MIT license: see LICENSE.\n// The solver is independently ported in ../pi.rs; these are upstream recipe data.\nuse super::{Curve, Family, Rule, Token};\n\n`
  + `pub(super) const FAMILIES: &[Family] = &[\n${familyNames.map(name => {
    const x = upstream.FAMILIES[name];
    return `    Family { hue: ${f(x.hue)}, min: ${f(x.saturation.min)}, max: ${f(x.saturation.max)} }, // ${name}`;
  }).join('\n')}\n];\n\n`
  + `pub(super) const TOKENS: &[Token] = &[\n${tokenNames.map(name => {
    const family = upstream.TOKEN_FAMILIES[name];
    return `    Token { name: "${name}", family: ${familyNames.indexOf(family)}, slot: ${upstream.TOKEN_SLOTS[name] ?? upstream.FAMILIES[family].slot}, panel: ${upstream.PANELS.includes(name)}, foreground: ${upstream.FOREGROUND_TOKENS.includes(name)} },`;
  }).join('\n')}\n];\n\n`
  + `pub(super) const LEVELS: &[[Curve; 2]] = &[\n${levelNames.map(name => {
    const pair = ['dark', 'light'].map(appearance => {
      const x = upstream.LEVELS[name][appearance];
      return `Curve { coefficients: [${x.coefficients.map(f).join(', ')}], reachable: [${x.reachable.map(f).join(', ')}] }`;
    });
    return `    [${pair.join(', ')}], // ${name}`;
  }).join('\n')}\n];\n\n`
  + `pub(super) const RULES: &[Rule] = &[\n${upstream.RULES.map(x => `    Rule { token: ${ti(x.token)}, on: &[${x.on.map(ti).join(', ')}], level: ${levelNames.indexOf(x.level)} },`).join('\n')}\n];\n\n`
  + `pub(super) const SOLVE_ORDER: &[usize] = &[${upstream.SOLVE_ORDER.map(ti).join(', ')}];\n`
  + `pub(super) const READABLE_FLOOR: [usize; 2] = [${levelNames.indexOf(upstream.READABLE_FLOOR.dark)}, ${levelNames.indexOf(upstream.READABLE_FLOOR.light)}];\n`
  + `pub(super) const FOREGROUND_LEVEL: usize = ${levelNames.indexOf(upstream.FOREGROUND_LEVEL)};\n`;
assert.equal(new Set(upstream.SOLVE_ORDER).size, tokenNames.length);
if (process.argv.includes('--write')) {
  writeFileSync(new URL('./vectors.json', import.meta.url), JSON.stringify(fixture, null, 2) + '\n');
  writeFileSync(new URL('./recipe.rs', import.meta.url), recipe);
} else if (process.argv.includes('--check')) {
  assert.deepEqual(JSON.parse(readFileSync(new URL('./vectors.json', import.meta.url), 'utf8')), JSON.parse(JSON.stringify(fixture)));
}
console.log(`Hash-verified upstream oracle: ${vectors.length} vectors × ${tokenNames.length} tokens; ${familyNames.length} families, ${levelNames.length} levels, ${upstream.RULES.length} rules. No Octet/Rust code executed.`);
