'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const {
  nativeJoin,
  buildConsoleScopeModel,
  resolveConsoleScope,
  consoleScopeOverrideFor,
} = require('../frontend/console-context.js');

const repository = { name: 'project', path: '/work/project' };

function model(overrides = {}) {
  return buildConsoleScopeModel({ repository, view: 'explorer', currentPath: '', commanderPath: '', ...overrides });
}

test('nativeJoin preserves the host path style', () => {
  assert.equal(nativeJoin('/work/project/', '/src/core'), '/work/project/src/core');
  assert.equal(nativeJoin('D:\\work\\project\\', 'src/core'), 'D:\\work\\project\\src\\core');
});

test('an Explorer folder is the default and uses its exact absolute path', () => {
  const result = model({ currentPath: 'src/core' });
  assert.equal(resolveConsoleScope(result, null).path, '/work/project/src/core');
  assert.equal(resolveConsoleScope(result, null).label, 'Current folder');
});

test('selecting a submodule does not silently replace the current folder', () => {
  const result = model({ currentPath: 'src', selectedSubmodule: { relativePath: 'modules/ui' } });
  assert.equal(resolveConsoleScope(result, null).path, '/work/project/src');
  assert.deepEqual(result.scopes.map(item => item.path), ['/work/project/src', '/work/project', '/work/project/modules/ui']);
});

test('a selected submodule remains an explicit selectable target', () => {
  const result = model({ selectedSubmodule: { relativePath: 'modules/ui' } });
  const override = consoleScopeOverrideFor(result, result.scopes.find(item => item.kind === 'submodule').key);
  assert.equal(resolveConsoleScope(result, override).path, '/work/project/modules/ui');
});

test('an explicit root choice is discarded after navigating to another folder', () => {
  const first = model({ currentPath: 'src/one' });
  const rootOverride = consoleScopeOverrideFor(first, first.scopes.find(item => item.kind === 'root').key);
  assert.equal(resolveConsoleScope(first, rootOverride).path, '/work/project');

  const afterNavigation = model({ currentPath: 'src/two' });
  assert.equal(resolveConsoleScope(afterNavigation, rootOverride).path, '/work/project/src/two');
});

test('a submodule choice cannot leak to another selected submodule', () => {
  const first = model({ selectedSubmodule: { relativePath: 'modules/one' } });
  const submoduleOverride = consoleScopeOverrideFor(first, first.scopes.find(item => item.kind === 'submodule').key);
  assert.equal(resolveConsoleScope(first, submoduleOverride).path, '/work/project/modules/one');

  const second = model({ selectedSubmodule: { relativePath: 'modules/two' } });
  assert.equal(resolveConsoleScope(second, submoduleOverride).path, '/work/project');
});

test('a Submodule Branch Map defaults to that submodule repository, not its parent', () => {
  const result = model({
    view: 'graph',
    currentPath: 'unrelated/old/path',
    submoduleGraph: { name: 'ui', relativePath: 'modules/ui', path: '/work/project/modules/ui' },
  });
  assert.equal(resolveConsoleScope(result, null).path, '/work/project/modules/ui');
  assert.deepEqual(result.scopes.map(item => item.path), ['/work/project/modules/ui', '/work/project']);
});

test('Commander uses the directory currently displayed there', () => {
  const result = model({ view: 'commander', currentPath: 'old/explorer/path', commanderPath: 'src/remote-check' });
  assert.equal(resolveConsoleScope(result, null).path, '/work/project/src/remote-check');
});

test('a normal Branch Map and Remotes view default to repository root, not stale Explorer state', () => {
  assert.equal(resolveConsoleScope(model({ view: 'graph', currentPath: 'old/path' }), null).path, '/work/project');
  assert.equal(resolveConsoleScope(model({ view: 'remotes', currentPath: 'old/path' }), null).path, '/work/project');
});
