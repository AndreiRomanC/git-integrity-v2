// Pure execution-context model for the Terminal panel.
//
// The UI has several repository-like locations at once: the parent repository,
// the folder currently being browsed, a selected submodule, and (when open) a
// submodule's own Branch Map.  A scope choice therefore cannot be stored as the
// generic word "folder" or "submodule": those words can silently point at a
// different path after navigation.  This module gives every target an identity
// based on its concrete path and binds an explicit choice to one UI context.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.ConsoleContextModel = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  function trimSeparators(value) { return String(value || '').replace(/[\\/]+$/, ''); }
  function normalizeRelative(value) { return String(value || '').replace(/^[\\/]+|[\\/]+$/g, '').replace(/\\/g, '/'); }
  function nativeJoin(base, relative) {
    const root = trimSeparators(base);
    const part = normalizeRelative(relative);
    if (!part) return root;
    const separator = root.includes('\\') ? '\\' : '/';
    return `${root}${separator}${part.replace(/\//g, separator)}`;
  }
  function pathIdentity(value) {
    // Windows paths are case-insensitive.  Lower-casing only for identity does
    // not alter the path passed to the backend.
    const normalized = trimSeparators(value).replace(/\\/g, '/');
    return /^[a-z]:\//i.test(normalized) ? normalized.toLowerCase() : normalized;
  }
  function scope(kind, path, label, displayPath) {
    return { key: `${kind}:${pathIdentity(path)}`, kind, path, label, displayPath: displayPath || path };
  }

  function buildConsoleScopeModel(context) {
    const repository = context?.repository;
    if (!repository?.path) return { scopes: [], defaultKey: '', contextKey: 'no-repository' };

    const rootScope = scope('root', repository.path, 'Repository root', repository.name || repository.path);
    let defaultScope = rootScope;
    const alternatives = [];
    const view = context.view || 'explorer';

    // A Submodule Branch Map is a repository context in its own right.  Its
    // Terminal defaults to that submodule, never to the parent repository.
    if (view === 'graph' && context.submoduleGraph?.path) {
      defaultScope = scope(
        'submodule',
        context.submoduleGraph.path,
        'Submodule repository',
        context.submoduleGraph.relativePath || context.submoduleGraph.name || context.submoduleGraph.path,
      );
      alternatives.push(rootScope);
    } else {
      const relativeFolder = view === 'commander'
        ? normalizeRelative(context.commanderPath)
        : view === 'explorer'
          ? normalizeRelative(context.currentPath)
          : '';
      if (relativeFolder) {
        defaultScope = scope('folder', nativeJoin(repository.path, relativeFolder), 'Current folder', relativeFolder);
        alternatives.push(rootScope);
      }

      // Selecting a row must not silently move the Terminal.  The selected
      // submodule remains available as an explicit choice, while "where I am"
      // (the current folder) stays the default.
      if (view === 'explorer' && context.selectedSubmodule?.relativePath) {
        const relativePath = normalizeRelative(context.selectedSubmodule.relativePath);
        const selected = scope('submodule', nativeJoin(repository.path, relativePath), 'Selected submodule', relativePath);
        if (selected.key !== defaultScope.key) alternatives.push(selected);
      }
    }

    const scopes = [defaultScope, ...alternatives.filter((candidate, index, list) =>
      candidate.key !== defaultScope.key && list.findIndex(item => item.key === candidate.key) === index)];
    const contextKey = [pathIdentity(repository.path), view, defaultScope.key, ...scopes.map(item => item.key)].join('|');
    return { scopes, defaultKey: defaultScope.key, contextKey };
  }

  function resolveConsoleScope(model, override) {
    if (!model?.scopes?.length) return { key: '', kind: 'none', path: '', label: 'No repository open', displayPath: '' };
    if (override?.contextKey === model.contextKey) {
      const exact = model.scopes.find(item => item.key === override.targetKey);
      if (exact) return exact;
    }
    return model.scopes.find(item => item.key === model.defaultKey) || model.scopes[0];
  }

  function consoleScopeOverrideFor(model, targetKey) {
    if (!model?.scopes?.some(item => item.key === targetKey)) return null;
    return { contextKey: model.contextKey, targetKey };
  }

  return { nativeJoin, buildConsoleScopeModel, resolveConsoleScope, consoleScopeOverrideFor };
});
