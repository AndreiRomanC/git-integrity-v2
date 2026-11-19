const test = require('node:test');
const assert = require('node:assert/strict');
const { create } = require('../frontend/polarion-connection.js');
function harness(responses = []) {
  const calls = [], logs = [], session = create({ now: () => 1234, log: line => logs.push(line) });
  const invoke = async (command, args) => {
    calls.push({ command, args });
    const data = responses.shift();
    if (data instanceof Error) { session.observe(command, args, null, data); throw data; }
    session.observe(command, args, data); return data;
  };
  return { calls, logs, session, invoke };
}
test('log 42: unavailable /user falls back to one read check; next check avoids the unsupported endpoint', async () => {
  const h = harness([{ code: 'api_unavailable', http: 404, session_token: true }, { code: 'ok' }, { code: 'ok' }]);
  assert.equal(h.calls.length, 0);
  const result = await h.session.check(h.invoke);
  assert.equal(result.state, 'connected'); assert.equal(result.login, undefined);
  assert.deepEqual(h.calls.map(c => c.args.kind), ['user', 'connection']);
  await h.session.check(h.invoke);
  assert.deepEqual(h.calls.map(c => c.args.kind), ['user', 'connection', 'connection']);
  assert.doesNotMatch(h.logs.join('\n'), /SECRET|session_token/);
});
test('a real P task read verifies the shared session without any extra request', () => {
  const h = harness(); h.session.observe('polarion_search', { kind: 'tasks' }, { code: 'ok', items: [] });
  assert.equal(h.session.snapshot().state, 'connected'); assert.equal(h.calls.length, 0);
  h.session.observe('polarion_read', { kind: 'user' }, { code: 'api_unavailable', http: 404 });
  assert.equal(h.session.snapshot().state, 'connected');
});
test('stored snapshots, cached catalogs and a token are not connection verification', () => {
  const h = harness();
  h.session.observe('polarion_workspace', {}, { code: 'ok', primary_task: {} });
  h.session.observe('polarion_search', { kind: 'projects' }, { code: 'ok', catalog_cached: true });
  h.session.observe('polarion_read', { kind: 'user' }, { code: 'api_unavailable', session_token: true });
  assert.equal(h.session.snapshot().state, 'unchecked');
});
test('real authentication failures clear success and do not trigger fallback probes', async () => {
  for (const code of ['auth_required', 'wrong_origin', 'not_json']) {
    const h = harness([{ code }]); h.session.observe('polarion_read', { kind: 'user' }, { code: 'ok', user_id: 'someone' });
    assert.equal((await h.session.check(h.invoke)).state, 'sign_in_required');
    assert.equal(h.calls.length, 1); assert.equal(h.session.snapshot().login, undefined);
  }
});
test('forbidden identity can coexist with allowed reads, but forbidden read check is not connected', async () => {
  const h = harness([{ code: 'forbidden' }, { code: 'forbidden' }]);
  assert.equal((await h.session.check(h.invoke)).state, 'failed');
  assert.equal(h.calls.length, 2);
});
test('offline, timeout and a closed window replace stale green; busy bridge does not sign out', async () => {
  for (const code of ['network_error','timeout']) {
    const h = harness(); h.session.observe('polarion_search', { kind: 'tasks' }, { code: 'ok' });
    h.session.observe('polarion_read', { kind: 'user' }, { code });
    assert.equal(h.session.snapshot().state, 'failed');
  }
  const h = harness(); h.session.observe('polarion_search', { kind: 'tasks' }, { code: 'ok' });
  h.session.observe('polarion_read', { kind: 'user' }, null, 'A Polarion search is already running.');
  assert.equal(h.session.snapshot().state, 'connected');
  h.session.observe('polarion_read', { kind: 'user' }, null, 'Open Polarion, sign in if needed, then retry Search.');
  assert.equal(h.session.snapshot().state, 'sign_in_required');
});
test('explicit checks are deduplicated and account name comes only from a user response', async () => {
  const h = harness([{ code: 'ok', user_id: 'verified', user_name: 'Verified name' }]);
  const first = h.session.check(h.invoke), second = h.session.check(h.invoke);
  assert.equal(first, second); assert.equal((await first).login, 'verified'); assert.equal(h.calls.length, 1);
});
