const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');

function harness() {
  const calls = [];
  const refs = new Proxy({}, { get(target, name) {
    return target[name] ||= { value: '', open: true, innerHTML: '', textContent: '', disabled: false, querySelectorAll: () => [] };
  } });
  const context = { refs, state: { repository: { path: '/project' } },
    submoduleBrowserState: { repositories: [], selectedRepository: null, allRefs: [], refs: [], selectedRefs: [] },
    submoduleRefRequest: 0, finishSubmoduleRefRequest: null,
    submoduleBrowserRefCache: new Map(), SUBMODULE_REPOSITORY_CACHE_TTL_MS: 300000,
    esc: String, revisionDisplay: String, performance, jsPerfLog() {},
    beginButtonOperation(button) { button.disabled = true; return () => { button.disabled = false; }; },
    invoke(command, args) { return new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })); },
  };
  vm.createContext(context);
  vm.runInContext(app.slice(app.indexOf('function submoduleBrowserRefLabel('), app.indexOf('function applySubmoduleBrowserSelection(')), context);
  return { context, refs, calls };
}
const repo = name => ({ name, owner: 'eng', full_name: `eng/${name}` });
const ref = (name, kind = 'branch') => ({ name, kind, revision: 'a'.repeat(40) });
const complete = () => [...Array.from({ length: 32 }, (_, i) => ref(`branch-${i}`)), ...Array.from({ length: 88 }, (_, i) => ref(`tag-${i}`, 'tag'))];

test('32 branches and 88 tags sharing a SHA are all rendered and filtered locally', async () => {
  const h = harness();
  const pending = h.context.selectSubmoduleRepository(repo('large'));
  assert.equal(h.calls[0].args.query, '');
  assert.equal(h.calls[0].args.limit, null);
  h.calls[0].resolve(complete());
  await pending;
  assert.equal(h.context.submoduleBrowserState.refs.length, 120);
  assert.equal((h.refs.submoduleRefResults.innerHTML.match(/data-submodule-ref-index=/g) || []).length, 120);
  assert.match(h.refs.submoduleBrowserStatus.textContent, /32 branches · 88 tags/);
  h.refs.submoduleRefSearch.value = 'tag-87';
  await h.context.searchSubmoduleRefs();
  assert.equal(h.context.submoduleBrowserState.refs.length, 1);
  h.refs.submoduleRefSearch.value = '';
  await h.context.searchSubmoduleRefs();
  assert.equal(h.context.submoduleBrowserState.refs.length, 120);
  assert.equal(h.calls.length, 1, 'typing and clearing the search do not access the server');
});

test('search before the list loads must request the complete advertisement, not cache a partial query', async () => {
  const h = harness();
  h.context.submoduleBrowserState.selectedRepository = repo('large');
  h.refs.submoduleRefSearch.value = 'tag-87';
  const pending = h.context.searchSubmoduleRefs();
  assert.equal(h.calls[0].args.query, '');
  h.calls[0].resolve(complete());
  await pending;
  assert.equal(h.context.submoduleBrowserState.refs.length, 1);
  assert.equal(h.context.cachedSubmoduleRefs(repo('large')).length, 120);
});

test('late responses and errors cannot replace the newly selected repository or unlock its request', async () => {
  for (const fail of [false, true]) {
    const h = harness();
    const old = h.context.selectSubmoduleRepository(repo('old'));
    const fresh = h.context.selectSubmoduleRepository(repo('fresh'));
    if (fail) h.calls[0].reject(new Error('old failure'));
    else h.calls[0].resolve([ref('wrong')]);
    await old;
    assert.equal(h.refs.runSubmoduleRefSearch.disabled, true);
    assert.equal(h.context.submoduleBrowserState.allRefs.length, 0);
    h.calls[1].resolve(complete());
    await fresh;
    assert.equal(h.context.submoduleBrowserState.refs.length, 120);
    assert.equal(h.context.cachedSubmoduleRefs(repo('old')), null);
    assert.equal(h.refs.runSubmoduleRefSearch.disabled, false);
  }
});

test('latest request wins even after selecting A then B then A', async () => {
  const h = harness();
  const a = repo('A');
  const old = h.context.selectSubmoduleRepository(a);
  const b = h.context.selectSubmoduleRepository(repo('B'));
  const latest = h.context.selectSubmoduleRepository(a);
  h.calls[2].resolve(complete()); await latest;
  h.calls[0].resolve([ref('old-A')]); await old;
  h.calls[1].resolve([ref('old-B')]); await b;
  assert.equal(h.context.submoduleBrowserState.refs.length, 120);
});

test('typing during load applies the newest filter after the full result arrives', async () => {
  const h = harness();
  const pending = h.context.selectSubmoduleRepository(repo('large'));
  h.refs.submoduleRefSearch.value = 'tag-87';
  h.context.filterLoadedSubmoduleRefs();
  assert.match(h.refs.submoduleRefResults.innerHTML, /Reading branches and tags/);
  h.calls[0].resolve(complete()); await pending;
  assert.equal(h.context.submoduleBrowserState.refs[0].name, 'tag-87');
  assert.equal(h.context.submoduleBrowserState.allRefs.length, 120);
});

test('SHA lookup never poisons complete cache; explicit refresh reloads all refs even with a filter', async () => {
  const h = harness();
  const repository = repo('large');
  const initial = h.context.selectSubmoduleRepository(repository);
  h.calls[0].resolve(complete()); await initial;
  h.refs.submoduleRefSearch.value = 'abcdef123';
  const sha = h.context.searchSubmoduleRefs();
  h.calls[1].resolve([{ name: 'abcdef123', revision: 'abcdef123', kind: 'commit' }]); await sha;
  assert.equal(h.context.cachedSubmoduleRefs(repository).length, 120);
  const refresh = h.context.searchSubmoduleRefs('', { forceRemote: true });
  assert.equal(h.calls[2].args.query, '');
  assert.equal(h.calls[2].args.limit, null);
  h.calls[2].resolve([...complete(), ref('new-branch')]); await refresh;
  assert.equal(h.context.cachedSubmoduleRefs(repository).length, 121);
  h.refs.submoduleRefSearch.value = '';
  h.context.filterLoadedSubmoduleRefs();
  assert.equal(h.context.submoduleBrowserState.refs.length, 121);
});

test('cache expires, is project-scoped, and empty complete repositories are cached', async () => {
  const h = harness();
  const repository = repo('empty');
  const initial = h.context.selectSubmoduleRepository(repository);
  h.calls[0].resolve([]); await initial;
  await h.context.selectSubmoduleRepository(repository);
  assert.equal(h.calls.length, 1);
  assert.match(h.refs.submoduleBrowserStatus.textContent, /0 branches · 0 tags/);
  h.context.state.repository.path = '/another';
  assert.equal(h.context.cachedSubmoduleRefs(repository), null);
  h.context.state.repository.path = '/project';
  const cached = h.context.submoduleBrowserRefCache.get(h.context.submoduleRepoCacheKey(repository));
  cached.savedAt -= 300001;
  assert.equal(h.context.cachedSubmoduleRefs(repository), null);
});

test('closed dialog and changed project reject outstanding ref responses', async () => {
  for (const close of [true, false]) {
    const h = harness();
    const initial = h.context.selectSubmoduleRepository(repo('large'));
    if (close) h.refs.submoduleBrowserDialog.open = false;
    else h.context.state.repository.path = '/another';
    h.calls[0].resolve(complete()); await initial;
    assert.equal(h.context.submoduleBrowserState.allRefs.length, 0);
    assert.equal(h.context.submoduleBrowserRefCache.size, 0);
  }
});
