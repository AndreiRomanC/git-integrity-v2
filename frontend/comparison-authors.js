// Metadata cache keyed by immutable snapshot IDs and repository context.
// It never replaces rows or supplies file contents to the existing diff UI.
function createComparisonAuthorCache(invoke, capacity = 4) {
  const entries = new Map();
  const preferences = new Map();
  const scope = context => JSON.stringify([context.repositoryPath, context.submodulePath || '', context.owner || '', context.repositoryName || '']);
  function remember(model) {
    preferences.set(scope(model.context), { mode:model.mode, selected:model.selected, directOnly:model.directOnly });
    apply(model);
  }
  function apply(model) {
    const author = (model.mode === 'introduced' ? model.introductions : model.authors)?.find(author => author.key === model.selected);
    model.strictResult = model.strict.get(model.selected) || null;
    const base = new Set(author?.paths || []);
    model.paths = model.mode === 'all' ? null : model.mode === 'shared'
      ? new Set([...(model.contributorsByPath || [])].filter(([, authors]) => authors.size >= 2).map(([path]) => path))
      : comparisonAuthorStrict(model) ? new Set((model.strictResult?.result?.paths || []).filter(path => base.has(path))) : base;
  }
  return {
    get(context) {
      const key = JSON.stringify(context);
      if (!entries.has(key)) {
        entries.set(key, { context, authors: null, contributorsByPath:null, introductions:null, introductionError:'', introductionPending:null, mode:'all', selected: '', paths: null, directOnly: false, error: '', pending: null, strict: new Map(), strictResult: null });
        if (entries.size > capacity) entries.delete(entries.keys().next().value);
      }
      const model = entries.get(key);
      Object.assign(model, preferences.get(scope(context)) || {});
      apply(model);
      return model;
    },
    async load(model) {
      if (model.pending) return model.pending;
      if (model.authors) return model.authors;
      model.error = '';
      // Reuse the metadata endpoint's exact-path support for committed folder
      // comparisons too. Keep this separate from the workspace marker: strict
      // attribution still receives two committed revisions, not a worktree.
      const { scopePaths, parentComparison, ...context } = model.context;
      const request = scopePaths ? {...context, comparisonPaths:scopePaths} : context;
      model.pending = Promise.resolve().then(() => invoke('comparison_authors', request))
        .then(authors => { model.authors = authors; model.contributorsByPath = comparisonContributorIndex(authors); apply(model); return authors; })
        .catch(error => { model.error = String(error); return null; })
        .finally(() => { model.pending = null; });
      return model.pending;
    },
    select(model, key, mode = 'touched') {
      model.selected = key || '';
      model.mode = model.selected ? mode : 'all';
      remember(model);
    },
    setMode(model, mode) {
      if (!['all','touched','shared','introduced'].includes(mode)) throw new Error('Unknown contributor filter');
      model.mode = mode;
      remember(model);
    },
    setDirectOnly(model, enabled) {
      model.directOnly = Boolean(enabled);
      remember(model);
    },
    async loadIntroductions(model, retry = false) {
      if (model.introductionPending) return model.introductionPending;
      if (model.introductions || (model.introductionError && !retry)) return model.introductions;
      model.introductionError = '';
      const { scopePaths, ...context } = model.context;
      const request = scopePaths ? {...context, comparisonPaths:scopePaths} : context;
      model.introductionPending = Promise.resolve().then(() => invoke('comparison_introductions', request))
        .then(authors => { model.introductions = authors; return authors; })
        .catch(error => { model.introductionError = String(error); return null; })
        .finally(() => { model.introductionPending = null; apply(model); });
      apply(model);
      return model.introductionPending;
    },
    async loadStrict(model, retry = false) {
      if (!comparisonAuthorStrict(model) || !model.authors) return null;
      const key = model.selected;
      let entry = model.strict.get(key);
      if (!entry) { entry = {result:null,error:'',pending:null}; model.strict.set(key,entry); }
      if (entry.pending) return entry.pending;
      if (entry.result || (entry.error && !retry)) return entry.result;
      entry.error = '';
      const candidatePaths = [...(model.authors.find(author => author.key === key)?.paths || [])];
      const { scopePaths, parentComparison, ...context } = model.context;
      entry.pending = Promise.resolve().then(() => invoke('comparison_author_contributions', {...context, authorKey:key, candidatePaths}))
        .then(result => { entry.result = result; return result; })
        .catch(error => { entry.error = String(error); return null; })
        .finally(() => { entry.pending = null; apply(model); });
      apply(model);
      return entry.pending;
    },
  };
}
function comparisonAuthorMatches(row, model) {
  if (comparisonAuthorMode(model) === 'shared') return comparisonAuthorContributors(row, model).length >= 2;
  return !model?.paths || comparisonRowPaths(row)
    .some(path => path && model.paths.has(path));
}
function comparisonRowPaths(row) {
  return [...new Set([row.relative_path, row.local?.relative_path, row.remote?.relative_path].filter(Boolean))];
}
// The directory endpoint supplies only immediate children. Load its leaf list
// on demand, pinned to the displayed revisions, and keep it on THAT snapshot.
// A late completion never installs a different directory or comparison.
async function loadComparisonLeafScope(scope, load, retry = false) {
  if (scope.pending) return scope.pending;
  if (scope.rows || (scope.error && !retry)) return scope.rows;
  scope.requested = true;
  scope.error = '';
  scope.pending = Promise.resolve().then(load)
    .then(result => { scope.rows = result.rows || []; return scope.rows; })
    .catch(error => { scope.error = String(error); return null; })
    .finally(() => { scope.pending = null; });
  return scope.pending;
}
// Filter leaves first, then project them onto the current directory. Never
// union unrelated children's authors to call a folder a "Shared file".
function comparisonViewRows(directoryRows, leafRows, model, {flat = false, query = '', filter = 'all'} = {}) {
  const visible = row => (filter === 'all' || row.status === filter) && (!query ||
    [row.name, ...comparisonRowPaths(row)].filter(Boolean).join(' ').toLowerCase().includes(query));
  const candidates = (leafRows || []).filter(visible);
  const matching = candidates.filter(row => comparisonAuthorMatches(row, model));
  const descendants = new Map();
  for (const row of matching) {
    const parents = new Set();
    for (const path of comparisonRowPaths(row)) {
      const parts = path.split('/'); parts.pop();
      while (parts.length) { parents.add(parts.join('/')); parts.pop(); }
    }
    for (const path of parents) descendants.set(path, (descendants.get(path) || 0) + 1);
  }
  const exact = new Set(matching.flatMap(comparisonRowPaths));
  const filtered = comparisonAuthorMode(model) !== 'all';
  const rows = flat ? matching : !filtered && filter === 'all' && !query ? directoryRows
    : leafRows === null ? (filtered ? [] : directoryRows.filter(visible))
    : directoryRows.filter(row => comparisonRowPaths(row).some(path => exact.has(path) ||
      ((row.local?.kind === 'folder' || row.remote?.kind === 'folder' || row.local?.kind === 'submodule' || row.remote?.kind === 'submodule') && descendants.has(path))));
  return {rows, candidates, matching, descendants};
}
function comparisonAuthorMode(model) {
  return model?.mode || (model?.selected ? 'touched' : 'all');
}
function comparisonAuthorStrict(model) {
  return comparisonAuthorMode(model) === 'touched' && Boolean(model?.selected && model.directOnly);
}
// One local index per metadata response. Shared mode and expanded explanations
// reuse the existing history walk; neither starts another Git request.
function comparisonContributorIndex(authors) {
  const index = new Map();
  for (const author of authors) for (const path of author.paths || []) {
    if (!index.has(path)) index.set(path, new Map());
    index.get(path).set(author.key, author);
  }
  return index;
}
function comparisonAuthorContributors(row, model) {
  const index = model?.contributorsByPath || comparisonContributorIndex(model?.authors || []);
  const contributors = new Map();
  for (const path of comparisonRowPaths(row)) for (const [key, author] of index.get(path) || []) contributors.set(key, author);
  return [...contributors.values()];
}
function comparisonContributorEvidence(row, model) {
  return comparisonAuthorContributors(row, model).flatMap(author =>
    comparisonAuthorEvidence(row, {...model, mode:'touched', selected:author.key, directOnly:false})
      .map(touch => ({...touch, authorKey:author.key, authorName:author.name, authorEmail:author.email})));
}
// Scope to the actual loaded comparison, before text/state/author filtering.
// No prefix guesses, repository scans, or additional requests while typing.
// Stable ordering lets the same folder reuse its own metadata, while another
// folder at the same SHAs gets a separate cache entry (including strict results).
function scopedComparisonAuthorContext(context, rows) {
  return context ? {...context, scopePaths:[...new Set(rows.flatMap(comparisonRowPaths))].sort()} : null;
}
// Counts are rows in the SAME normal-author population, after the view's path,
// search and state filters. Aliases/renames count once, never once per path.
function comparisonAuthorPartition(rows, model) {
  const author = model?.authors?.find(author => author.key === model.selected);
  const base = new Set(author?.paths || []);
  const result = model?.strictResult?.result;
  const own = new Set(result?.paths || []);
  const none = new Set(result?.no_contribution_paths || []);
  const partition = {total:0, shown:0, excluded:0, content:[], structural:new Map(), inconclusive:[], noContribution:[]};
  for (const row of rows) {
    const paths = comparisonRowPaths(row).filter(path => base.has(path));
    if (!paths.length) continue;
    partition.total++;
    const included = paths.some(path => own.has(path));
    const root = paths.map(path => result?.structural_paths?.[path]).find(Boolean);
    if (included && !root) { partition.content.push(row); partition.shown++; }
    else if (included && root) {
      if (!partition.structural.has(root)) partition.structural.set(root, []);
      partition.structural.get(root).push(row); partition.shown++;
    } else {
      partition.excluded++;
      const reason = paths.map(path => result?.ambiguous_paths?.[path]).find(Boolean);
      if (reason || !paths.some(path => none.has(path))) partition.inconclusive.push({row, reason:reason || 'This item has no analysis result. Retry with the updated application.'});
      else partition.noContribution.push({row, reason:'No surviving contribution from this author was found.'});
    }
  }
  return partition;
}
function comparisonAuthorEmptyMessage(model) {
  if (comparisonAuthorMode(model) === 'introduced') {
    if (model.introductionError) return 'Introduction history is incomplete or unavailable. No filtered results were applied; retry above.';
    if (!model.introductions) return 'Loading introduction history…';
    if (!model.selected) return 'Choose an author under Introduced by author.';
    return 'No compared items match this introducer and the current search/change-type filters.';
  }
  if (model?.error && comparisonAuthorMode(model) !== 'all') return 'Contributor history is unavailable. No filtered results were applied; retry above.';
  if (model?.authors === null && comparisonAuthorMode(model) !== 'all') return 'Loading contributor history…';
  if (comparisonAuthorMode(model) === 'shared') {
    return 'No compared items match Shared files and the current search/change-type filters.';
  }
  if (comparisonAuthorStrict(model)) {
    if (model.strictResult?.error) return 'Strict analysis could not finish. No results were applied; see the error above.';
    if (!model.strictResult?.result || model.strictResult?.pending) return 'Analyzing actual contributions…';
  }
  return 'No changed files match this filter';
}
function comparisonAuthorDiagnostic(model, totalRows, shownRows, otherFilterActive) {
  const mode = comparisonAuthorMode(model);
  if (mode === 'all') return '';
  const strict = comparisonAuthorStrict(model);
  const result = model.strictResult;
  const phase = mode === 'introduced' ? (model.introductionError ? 'error' : model.introductionPending ? 'pending' : model.introductions ? 'complete' : 'not-started')
    : strict ? (result?.error ? 'error' : result?.pending ? 'pending' : result?.result ? 'complete' : 'not-started')
    : model.error ? 'error' : model.pending ? 'pending' : model.authors ? 'complete' : 'not-started';
  // Counts and immutable revisions only; never log names, emails or search text.
  return `comparison_author_display: mode=${strict ? 'strict' : mode === 'shared' || mode === 'introduced' ? mode : 'normal'} phase=${phase} left=${model.context.leftRevision} right=${model.context.rightRevision} author_index=${mode === 'shared' ? -1 : (mode === 'introduced' ? model.introductions : model.authors)?.findIndex(author => author.key === model.selected) ?? -1} total_rows=${totalRows} matched_paths=${model.paths?.size ?? 0} shown_rows=${shownRows} inconclusive=${strict ? Object.keys(result?.result?.ambiguous_paths || {}).length : 0} other_filter=${Boolean(otherFilterActive)}`;
}
function comparisonAuthorEvidence(row, model) {
  const author = (comparisonAuthorMode(model) === 'introduced' ? model?.introductions : model?.authors)?.find(item => item.key === model.selected);
  if (!author) return [];
  const seen = new Set();
  return [row.relative_path, row.local?.relative_path, row.remote?.relative_path].filter(Boolean)
    .flatMap(path => ((comparisonAuthorStrict(model) ? model.strictResult?.result?.evidence : author.evidence)?.[path] || []).map(touch => ({...touch, path:touch.witness_path || path})))
    .filter(touch => {
      const key = `${touch.parent_repository || ''}:${touch.path}:${touch.commit}:${touch.kind || ''}`;
      if (seen.has(key)) return false;
      seen.add(key); return true;
    });
}
// Keep parent attribution only for the exact gitlink pair used to enter the
// child comparison. Changing refs drops it; reversing swaps BOTH pairs.
function comparisonParentContext(source, left, right) {
  if (!source) return null;
  const { childLeft, childRight, ...parent } = source;
  if (left === childLeft && right === childRight) return parent;
  if (left === childRight && right === childLeft) return {...parent, leftRevision:parent.rightRevision, rightRevision:parent.leftRevision};
  return null;
}
function workspaceComparisonAuthorContext(result) {
  if (!result?.repository_path || !result.local_revision || !result.remote_revision) return null;
  return {
    repositoryPath: result.repository_path,
    leftRevision: result.local_revision,
    rightRevision: result.remote_revision,
    comparisonPaths: [...new Set((result.rows || []).flatMap(row =>
      [row.relative_path, row.local?.relative_path, row.remote?.relative_path].filter(Boolean)))].sort(),
  };
}
if (typeof module !== 'undefined' && module.exports) module.exports = { createComparisonAuthorCache, loadComparisonLeafScope, comparisonViewRows, comparisonAuthorMatches, comparisonAuthorPartition, comparisonRowPaths, scopedComparisonAuthorContext, comparisonAuthorMode, comparisonAuthorStrict, comparisonAuthorContributors, comparisonContributorEvidence, comparisonAuthorEmptyMessage, comparisonAuthorDiagnostic, comparisonAuthorEvidence, comparisonParentContext, workspaceComparisonAuthorContext };
