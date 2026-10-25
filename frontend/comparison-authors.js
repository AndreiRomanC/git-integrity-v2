// Metadata cache keyed by immutable snapshot IDs and repository context.
// It never replaces rows or supplies file contents to the existing diff UI.
function createComparisonAuthorCache(invoke, capacity = 4) {
  const entries = new Map();
  const preferences = new Map();
  const scope = context => JSON.stringify([context.repositoryPath, context.submodulePath || '', context.owner || '', context.repositoryName || '']);
  function apply(model) {
    const author = model.authors?.find(author => author.key === model.selected);
    model.strictResult = model.strict.get(model.selected) || null;
    const base = new Set(author?.paths || []);
    model.paths = model.selected ? (model.directOnly ? new Set((model.strictResult?.result?.paths || []).filter(path => base.has(path))) : base) : null;
  }
  return {
    get(context) {
      const key = JSON.stringify(context);
      if (!entries.has(key)) {
        entries.set(key, { context, authors: null, selected: '', paths: null, directOnly: false, error: '', pending: null, strict: new Map(), strictResult: null });
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
      const { scopePaths, ...context } = model.context;
      const request = scopePaths ? {...context, comparisonPaths:scopePaths} : context;
      model.pending = Promise.resolve().then(() => invoke('comparison_authors', request))
        .then(authors => { model.authors = authors; apply(model); return authors; })
        .catch(error => { model.error = String(error); return null; })
        .finally(() => { model.pending = null; });
      return model.pending;
    },
    select(model, key) {
      model.selected = key || '';
      preferences.set(scope(model.context), { selected: model.selected, directOnly: model.directOnly });
      apply(model);
    },
    setDirectOnly(model, enabled) {
      model.directOnly = Boolean(enabled);
      this.select(model, model.selected);
    },
    async loadStrict(model, retry = false) {
      if (!model.directOnly || !model.selected || !model.authors) return null;
      const key = model.selected;
      let entry = model.strict.get(key);
      if (!entry) { entry = {result:null,error:'',pending:null}; model.strict.set(key,entry); }
      if (entry.pending) return entry.pending;
      if (entry.result || (entry.error && !retry)) return entry.result;
      entry.error = '';
      const candidatePaths = [...(model.authors.find(author => author.key === key)?.paths || [])];
      const { scopePaths, ...context } = model.context;
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
  return !model?.paths || comparisonRowPaths(row)
    .some(path => path && model.paths.has(path));
}
function comparisonRowPaths(row) {
  return [...new Set([row.relative_path, row.local?.relative_path, row.remote?.relative_path].filter(Boolean))];
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
  if (model?.selected && model.directOnly) {
    if (model.strictResult?.error) return 'Strict analysis could not finish. No results were applied; see the error above.';
    if (!model.strictResult?.result || model.strictResult?.pending) return 'Analyzing actual contributions…';
  }
  return 'No changed files match this filter';
}
function comparisonAuthorDiagnostic(model, totalRows, shownRows, otherFilterActive) {
  if (!model?.selected) return '';
  const strict = model.directOnly;
  const result = model.strictResult;
  const phase = strict ? (result?.error ? 'error' : result?.pending ? 'pending' : result?.result ? 'complete' : 'not-started')
    : model.error ? 'error' : model.pending ? 'pending' : model.authors ? 'complete' : 'not-started';
  // Counts and immutable revisions only; never log names, emails or search text.
  return `comparison_author_display: mode=${strict ? 'strict' : 'normal'} phase=${phase} left=${model.context.leftRevision} right=${model.context.rightRevision} author_index=${model.authors?.findIndex(author => author.key === model.selected) ?? -1} total_rows=${totalRows} matched_paths=${model.paths?.size ?? 0} shown_rows=${shownRows} inconclusive=${Object.keys(result?.result?.ambiguous_paths || {}).length} other_filter=${Boolean(otherFilterActive)}`;
}
function comparisonAuthorEvidence(row, model) {
  const author = model?.authors?.find(item => item.key === model.selected);
  if (!author) return [];
  const seen = new Set();
  return [row.relative_path, row.local?.relative_path, row.remote?.relative_path].filter(Boolean)
    .flatMap(path => ((model.directOnly ? model.strictResult?.result?.evidence : author.evidence)?.[path] || []).map(touch => ({...touch, path:touch.witness_path || path})))
    .filter(touch => {
      const key = `${touch.path}:${touch.commit}`;
      if (seen.has(key)) return false;
      seen.add(key); return true;
    });
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
if (typeof module !== 'undefined' && module.exports) module.exports = { createComparisonAuthorCache, comparisonAuthorMatches, comparisonAuthorPartition, comparisonRowPaths, scopedComparisonAuthorContext, comparisonAuthorEmptyMessage, comparisonAuthorDiagnostic, comparisonAuthorEvidence, workspaceComparisonAuthorContext };
