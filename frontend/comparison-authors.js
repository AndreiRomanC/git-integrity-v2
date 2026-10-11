// Metadata cache keyed by immutable snapshot IDs and repository context.
// It never replaces rows or supplies file contents to the existing diff UI.
function createComparisonAuthorCache(invoke, capacity = 4) {
  const entries = new Map();
  const preferences = new Map();
  const scope = context => JSON.stringify([context.repositoryPath, context.submodulePath || '', context.owner || '', context.repositoryName || '']);
  function apply(model) {
    const author = model.authors?.find(author => author.key === model.selected);
    model.strictResult = model.strict.get(model.selected) || null;
    model.paths = model.selected ? new Set(model.directOnly ? model.strictResult?.result?.paths || [] : author?.paths || []) : null;
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
      model.pending = Promise.resolve().then(() => invoke('comparison_authors', model.context))
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
      if (!model.directOnly || !model.selected) return null;
      const key = model.selected;
      let entry = model.strict.get(key);
      if (!entry) { entry = {result:null,error:'',pending:null}; model.strict.set(key,entry); }
      if (entry.pending) return entry.pending;
      if (entry.result || (entry.error && !retry)) return entry.result;
      entry.error = '';
      entry.pending = Promise.resolve().then(() => invoke('comparison_author_contributions', {...model.context, authorKey:key}))
        .then(result => { entry.result = result; return result; })
        .catch(error => { entry.error = String(error); return null; })
        .finally(() => { entry.pending = null; apply(model); });
      apply(model);
      return entry.pending;
    },
  };
}
function comparisonAuthorMatches(row, model) {
  return !model?.paths || [row.relative_path, row.local?.relative_path, row.remote?.relative_path]
    .some(path => path && model.paths.has(path));
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
if (typeof module !== 'undefined' && module.exports) module.exports = { createComparisonAuthorCache, comparisonAuthorMatches, comparisonAuthorEvidence, workspaceComparisonAuthorContext };
