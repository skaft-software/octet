import test from 'node:test';
import assert from 'node:assert/strict';
import { Compile } from 'typebox/compile';
import { validateTool, toolWire } from '../lib/tools.mjs';

const definition = parameters => ({ name: 'schema_probe', description: 'Schema probe', parameters, execute: async () => ({ content: [] }) });
const schemas = [
  { type: 'object', properties: { runId: { type: 'string', format: 'uuid' }, step: { type: 'number', multipleOf: 1 } } },
  { type: 'object', properties: { metadata: { type: 'object', patternProperties: { '^(.*)$': {} }, additionalProperties: false } } },
  { $id: 'urn:probe', $defs: { word: { type: 'string' } }, type: 'object', properties: { word: { $ref: '#/$defs/word' } } },
  { type: 'array', prefixItems: [{ const: 'first' }], items: { type: 'number' }, contains: { minimum: 1 }, minContains: 1, maxContains: 2, unevaluatedItems: false },
  { type: 'object', dependentRequired: { first: ['last'] }, dependentSchemas: { first: { required: ['last'] } }, dependencies: { first: ['last'] }, propertyNames: { pattern: '^[a-z]+$' }, unevaluatedProperties: true },
  { type: 'array', items: [{ type: 'string' }], additionalItems: false },
  { type: 'object', if: { required: ['a'] }, then: { required: ['b'] }, else: { not: { required: ['c'] } }, properties: { a: true, b: false } },
  { type: 'object', $comment: 'annotation', readOnly: true, writeOnly: false, contentEncoding: 'base64', contentMediaType: 'application/json', contentSchema: {}, definitions: {} },
  { type: 'object', $anchor: 'root', $dynamicAnchor: 'node', $recursiveAnchor: true, properties: { next: { anyOf: [{ type: 'null' }, { $dynamicRef: '#node' }] } } },
];

test('Pi schema keyword families are admitted and serialized without dropping constraints', () => {
  for (const parameters of schemas) {
    validateTool(definition(parameters));
    assert.deepEqual(toolWire('schema_probe', { definition: definition(parameters) }).parameters, parameters);
  }
});

test('Pi TypeBox enforces reported format, patternProperties, multipleOf and applicators', () => {
  const cases = [
    [{ type: 'string', format: 'uuid' }, '053dc2bc-237c-4c08-b4de-77b5dbd05fa2', 'not-a-uuid'],
    [{ type: 'number', multipleOf: 2 }, 4, 3],
    [{ type: 'object', patternProperties: { '^x': { type: 'integer' } }, additionalProperties: false }, { x: 2 }, { x: 'bad' }],
    [{ type: 'object', if: { required: ['a'] }, then: { required: ['b'] } }, { a: 1, b: 2 }, { a: 1 }],
  ];
  for (const [schema, good, bad] of cases) {
    validateTool(definition({ type: 'object', properties: { value: schema } }));
    const validator = Compile(schema);
    assert.equal(validator.Check(good), true);
    assert.equal(validator.Check(bad), false);
  }
});

test('schema keyword shapes and dependency work stay bounded before publication', () => {
  for (const parameters of [
    { required: [1] }, { dependentRequired: { a: [false] } }, { dependencies: { a: [null] } },
    { format: 1 }, { multipleOf: 0 }, { maxContains: -1 }, { $vocabulary: { urn: 'yes' } },
    { required: Array.from({ length: 65537 }, (_, index) => `key${index}`) },
  ]) assert.throws(() => validateTool(definition(parameters)));
  assert.throws(() => validateTool({ ...definition({}), outputSchema: { patternProperties: { '[': {} } } }));
});

test('native default-format golden cases retain the pinned TypeBox decisions', async () => {
  const { readFile } = await import('node:fs/promises');
  const cases = JSON.parse(await readFile(new URL('../../../crates/octet-ai/src/json_repair/format_cases.json', import.meta.url), 'utf8'));
  for (const { format, values } of cases) {
    const validator = Compile({ type: 'string', format });
    for (const [value, expected] of values) assert.equal(validator.Check(value), expected, `${format}: ${value}`);
  }
});

test('undefined schema object members are absent, matching the JSON wire document', () => {
  // Original @onkernel/browser-loop removeDiscriminator spreads `{ required: void 0 }`.
  const parameters = { type: 'object', required: undefined, properties: { filter: { type: 'string', description: undefined }, gone: undefined } };
  validateTool(definition(parameters));
  assert.deepEqual(toolWire('schema_probe', { definition: definition(parameters) }).parameters, { type: 'object', properties: { filter: { type: 'string' } } });
  validateTool({ ...definition({}), outputSchema: { type: 'object', required: undefined } });
  for (const bad of [
    { required: [undefined] }, { required: 'a' }, { required: null }, { required: ['a', 'a'] }, { enum: [undefined] },
    { properties: { a: null } }, { madeUp: 1 }, { type: 'object', required: undefined, multipleOf: 0 },
  ]) assert.throws(() => validateTool(definition(bad)), JSON.stringify(bad));
});

test('schema node bound matches the host (4096 nodes), independent of keyword count', () => {
  // ~2k nodes with many keywords each (e.g. @onkernel/browser-loop browser_act) is accepted.
  const wide = { type: 'object', properties: Object.fromEntries(Array.from({ length: 2000 }, (_, i) => [`p${i}`, { type: 'string', description: 'd', minLength: 0, maxLength: 9, pattern: '^a' }])), required: ['p0'] };
  validateTool(definition(wide));
  const deep = { type: 'object', properties: Object.fromEntries(Array.from({ length: 4096 }, (_, i) => [`p${i}`, { type: 'string' }])) };
  assert.throws(() => validateTool(definition(deep)), /work limit/);
});
