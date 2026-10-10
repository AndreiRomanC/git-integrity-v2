// Metadata cache keyed by immutable snapshot IDs and repository context.
// It never replaces rows or supplies file contents to the existing diff UI.
function createComparisonAuthorCache(invoke, capacity = 4) {
  const entries = new Map();
  return {
    get(context) {
      const key = JSON.stringify(context);
      if (!entries.has(key)) {
        entries.set(key, { context, authors: null, selected: '', paths: null, error: '', pending: null });
        if (entries.size > capacity) entries.delete(entries.keys().next().value);
      }
      return entries.get(key);
    },
    async load(model) {
      if (model.pending) return model.pending;
      if (model.authors) return model.authors;
      model.error = '';
      model.pending = Promise.resolve().then(() => invoke('comparison_authors', model.context))
        .then(authors => { model.authors = authors; return authors; })
        .catch(error => { model.error = String(error); return null; })
        .finally(() => { model.pending = null; });
      return model.pending;
    },
    select(model, key) {
      const author = model.authors?.find(author => author.key === key);
      model.selected = author?.key || '';
      model.paths = author ? new Set(author.paths) : null;
    },
  };
}
function comparisonAuthorMatches(row, model) {
  return !model?.paths || [row.relative_path, row.local?.relative_path, row.remote?.relative_path]
    .some(path => path && model.paths.has(path));
}
if (typeof module !== 'undefined' && module.exports) module.exports = { createComparisonAuthorCache, comparisonAuthorMatches };
