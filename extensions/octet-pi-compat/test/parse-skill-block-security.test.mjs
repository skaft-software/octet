import test from 'node:test';
import assert from 'node:assert/strict';
import { parseSkillBlock } from '../shims/coding-agent.mjs';

test('skill block parser preserves matching and earliest valid terminator semantics', () => {
  const header = '<skill name="n" location="l">\n';
  const parsed = (content, userMessage = undefined) => ({ name: 'n', location: 'l', content, userMessage });
  const cases = [
    [header + '\n</skill>', parsed('')],
    [header + 'abc\n</skill>', parsed('abc')],
    [header + 'abc\n</skill>\n\n user \n', parsed('abc', 'user')],
    [header + 'abc\n</skill>\n\n\n', parsed('abc')],
    [header + 'a\n</skill>\nx\n</skill>', parsed('a\n</skill>\nx')],
    [header + 'a\n</skill>\n\nb\n</skill>', parsed('a', 'b\n</skill>')],
    [header + 'abc\n</skill>\n', null],
    [header + 'abc\n</skill>\n\n', null],
    [header + 'abc\n</skill>\nx', null],
    ['<skill name="" location="l">\nabc\n</skill>', null],
    ['plain text', null],
  ];
  for (const [input, expected] of cases) assert.deepEqual(parseSkillBlock(input), expected);
  assert.deepEqual(parseSkillBlock('<skill name="n\nx" location="l">\nabc\n</skill>'),
    { name: 'n\nx', location: 'l', content: 'abc', userMessage: undefined });
});

test('skill block parser handles many misleading closing markers', () => {
  const prefix = '<skill name="n" location="l">\n';
  const body = ('\n</skill>\nx').repeat(100_000);
  assert.equal(parseSkillBlock(prefix + body), null);
  assert.deepEqual(parseSkillBlock(prefix + body + '\n</skill>'), {
    name: 'n', location: 'l', content: body, userMessage: undefined,
  });
});
