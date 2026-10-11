const test = require('node:test');
const assert = require('node:assert/strict');
const { createComparisonAuthorCache, comparisonAuthorMatches, workspaceComparisonAuthorContext } = require('../frontend/comparison-authors.js');
const context = { repositoryPath: '/repo', leftRevision: 'a', rightRevision: 'b' };
const authors = [{ key: 'email:a@test', name: 'Name', email: 'a@test', paths: ['old', 'new', 'modified'] }];

test('selecting a merge-only author with no personal touches shows no files, not all files', async () => {
  const cache = createComparisonAuthorCache(async () => [
    ...authors, { key: 'email:merger@test', name: 'Integrator', email: 'merger@test', paths: [] },
  ]);
  const model = cache.get(context);
  await cache.load(model);
  const rows = [{ relative_path: 'modified' }, { relative_path: 'imported' }];
  cache.select(model, 'email:a@test');
  assert.deepEqual(rows.filter(row => comparisonAuthorMatches(row, model)), [rows[0]]);
  cache.select(model, 'email:merger@test');
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 0);
  cache.select(model, '');
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 2);
});

test('workspace author context uses resolved owner and revisions, and the actual list paths', async () => {
  const result = { repository_path: '/repo/modules/a', local_revision: 'head-sha', remote_revision: 'remote-sha',
    rows: [{ relative_path: 'new', local: { relative_path: 'new' }, remote: { relative_path: 'old' } }, { relative_path: 'untracked' }] };
  const context = workspaceComparisonAuthorContext(result);
  assert.deepEqual(context, { repositoryPath: '/repo/modules/a', leftRevision: 'head-sha', rightRevision: 'remote-sha', comparisonPaths: ['new', 'old', 'untracked'] });
  const cache = createComparisonAuthorCache(async () => authors);
  const model = cache.get(context);
  await cache.load(model); cache.select(model, authors[0].key);
  assert.equal(comparisonAuthorMatches(result.rows[0], model), true);
  assert.equal(comparisonAuthorMatches(result.rows[1], model), false);
  const changed = workspaceComparisonAuthorContext({ ...result, rows: [{ relative_path: 'other' }] });
  assert.notEqual(cache.get(changed), model);
  assert.equal(cache.get(workspaceComparisonAuthorContext(result)), model);
  cache.select(model, '');
  assert.equal(comparisonAuthorMatches(result.rows[1], model), true);
  assert.equal(workspaceComparisonAuthorContext({ ...result, local_revision: null }), null);
});

test('author metadata is opt-in, cached and does not change comparison rows', async () => {
  let calls = 0;
  const cache = createComparisonAuthorCache(async (command, args) => {
    calls++; assert.equal(command, 'comparison_authors'); assert.deepEqual(args, context); return authors;
  });
  const model = cache.get(context);
  assert.equal(calls, 0);
  await Promise.all([cache.load(model), cache.load(model)]);
  assert.equal(calls, 1);
  const rows = [{ relative_path:'old', local:{relative_path:'old'} }, { relative_path:'new', remote:{relative_path:'new'} },
    { relative_path:'modified', local:{relative_path:'modified'}, remote:{relative_path:'modified'} }, { relative_path:'other' }];
  const before = JSON.stringify(rows);
  cache.select(model, authors[0].key);
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 3);
  assert.equal(comparisonAuthorMatches({ relative_path:'renamed', local:{relative_path:'old'} }, model), true);
  cache.select(model, '');
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 4);
  await cache.load(model);
  assert.equal(calls, 1);
  assert.equal(JSON.stringify(rows), before);
});

test('late author results never enter another repository/revision/submodule context', async () => {
  let finish;
  const cache = createComparisonAuthorCache(() => new Promise(resolve => { finish = resolve; }));
  const old = cache.get(context);
  const request = cache.load(old);
  await Promise.resolve();
  const next = cache.get({ ...context, rightRevision:'c' });
  const sub = cache.get({ ...context, submodulePath:'modules/a' });
  const external = cache.get({ ...context, owner:'org', repositoryName:'a' });
  finish(authors); await request;
  assert.equal(next.authors, null); assert.equal(sub.authors, null); assert.equal(external.authors, null);
  assert.equal(next.paths, null); assert.equal(old.authors, authors);
});

test('failed history reports an error, keeps full list and allows retry; cache is bounded', async () => {
  let calls = 0;
  const cache = createComparisonAuthorCache(async () => { if (++calls === 1) throw new Error('incomplete history'); return authors; }, 2);
  const model = cache.get(context);
  await cache.load(model);
  assert.match(model.error, /incomplete history/);
  assert.equal(comparisonAuthorMatches({relative_path:'anything'}, model), true);
  await cache.load(model);
  assert.equal(model.error, ''); assert.equal(model.authors, authors);
  cache.get({ ...context, rightRevision:'c' }); cache.get({ ...context, rightRevision:'d' });
  assert.notEqual(cache.get(context), model);
});
