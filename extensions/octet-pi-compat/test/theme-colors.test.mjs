import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { colorToHex, parseColor } from '../lib/theme-colors.mjs';
import { foregroundTokens, backgroundTokens, parsePiTheme, piThemeToNativeToml } from '../lib/theme-palette.mjs';
import { themeColorVectors } from './theme-color-oracle.mjs';

const vectors = JSON.parse(readFileSync(new URL('./fixtures/theme-colors.json', import.meta.url), 'utf8'));

test('perceptual colors match the shared pinned Pi 1.0.2 golden vectors', () => {
  for (const [input, expected] of vectors.valid) assert.equal(colorToHex(parseColor(input)), expected, input);
  for (const input of vectors.invalid) assert.throws(() => parseColor(input), input);
});

test('perceptual theme variables, exports, optional tokens and native conversion use the same exact colors', () => {
  for (const [input, expected] of vectors.valid) {
    const document = { name: 'Perceptual', vars: { a: 'b', b: input },
      colors: Object.fromEntries([...foregroundTokens, ...backgroundTokens].map(token => [token, 'a'])),
      export: { pageBg: 'a' } };
    delete document.colors.searchMatchBg;
    assert.equal(parsePiTheme(document).colors.searchMatchBg, input);
    assert.equal(parsePiTheme(document).export.pageBg, input);
    const { toml } = piThemeToNativeToml(document);
    assert.ok(toml.includes(`accent = "${expected}"`), input);
    assert.ok(toml.includes(`selected_bg = "${expected}"`), input);
  }
});

test('shared native/adapter golden vectors reproduce hash-verified upstream source, not an approximation', {
  skip: !process.env.PI_REFERENCE_REPO && 'set PI_REFERENCE_REPO to the reviewed offline Pi checkout',
}, async () => {
  assert.deepEqual(vectors, await themeColorVectors(process.env.PI_REFERENCE_REPO));
});
