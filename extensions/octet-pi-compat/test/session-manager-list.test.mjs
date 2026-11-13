// Pi session browsers call SessionManager.list/listAll. octet keeps its own
// sessions, so the adapter refuses with a pointer to /resume rather than the
// raw "SessionManager.list is not a function" users saw from /sessions.
import test from 'node:test';
import assert from 'node:assert/strict';
import { SessionManager } from '../lib/children.mjs';

for (const name of ['list', 'listAll']) {
  test(`SessionManager.${name} names the octet way to open a session`, async () => {
    assert.equal(typeof SessionManager[name], 'function');
    await assert.rejects(SessionManager[name](process.cwd()), error => {
      assert.match(error.message, new RegExp(`unsupported_feature SessionManager\\.${name}: .*use /resume`));
      return true;
    });
  });
}
