(function (global) {
  const WORKTREE = 'Working tree'; // Not a valid Git ref; deliberately distinct from HEAD.
  const clean = value => String(value || '').replace(/\\/g, '/').replace(/^\/+|\/+$/g, '');
  function origin(repository, path = '', submodulePath = '') {
    const sub = clean(submodulePath);
    return {
      owner: repository.path, submodulePath: sub,
      repositoryPath: sub ? `${repository.path.replace(/[\\/]+$/, '')}/${sub}` : repository.path,
      originPath: clean(path), name: sub ? sub.split('/').pop() : repository.name,
      repository: sub ? null : repository,
    };
  }
  function projectPath(context, innerPath) {
    return [context?.submodulePath, clean(innerPath)].filter(Boolean).join('/');
  }
  const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  function selectCommitPair(selected, id, extend = false) {
    if (!id) return selected;
    if (!extend) return [id];
    return selected.includes(id) ? selected.filter(value => value !== id) : [...selected, id].slice(-2);
  }

  // One active folder, bounded history pages, no polling or background refresh.
  // Uses the same pinned history query as Restore, but has no restore actions.
  function createHistory({ root, invoke, onChange, onContextMenu, onCompare }) {
    // The Explorer dialog shares paging and selection with Folder History,
    // but never runs a comparison until the user confirms a complete pair.
    const selectionOnly = typeof onCompare === 'function';
    let model = null;
    const left = root.querySelector('[data-history-left]');
    const right = root.querySelector('[data-history-right]');
    const rows = root.querySelector('[data-history-rows]');
    const note = root.querySelector('[data-history-note]');
    const search = root.querySelector('[data-history-search]');
    const more = root.querySelector('[data-history-more]');
    const refresh = root.querySelector('[data-history-refresh]');
    const selectedNote = root.querySelector('[data-history-selected-note]');
    const compareSelected = root.querySelector('[data-history-compare-selected]');
    const clearSelected = root.querySelector('[data-history-clear-selected]');
    function render() {
      if (!model) return;
      if (!selectionOnly) {
        left.value = model.left; right.value = model.right;
        root.querySelector('[data-history-options]').innerHTML = '<option value="HEAD">Current committed snapshot</option>' + model.commits.map(c => `<option value="${escape(c.id)}">${escape(c.date)} · ${escape(c.subject)}</option>`).join('');
      }
      const query = search.value.trim().toLowerCase();
      const visible = model.commits.filter(c => `${c.id} ${c.subject} ${c.author} ${c.date}`.toLowerCase().includes(query));
      rows.innerHTML = visible.map(c => {
        const position = model.selected.indexOf(c.id);
        return `<div class="folder-history-row ${position >= 0 ? 'compare-selected' : ''}" data-history-row="${escape(c.id)}"><button type="button" class="folder-history-commit" data-history-select="${escape(c.id)}" aria-pressed="${position >= 0}" title="Click to select · Ctrl/Cmd-click a second commit${onContextMenu ? ' · right-click to compare' : ''}"><strong>${escape(c.subject || 'No message')}</strong><small>${position >= 0 ? `<b>${position === 0 ? 'LEFT' : 'RIGHT'}</b> · ` : ''}<code>${escape(c.id.slice(0, 8))}</code> · ${escape(c.author)} · ${escape(c.date)}</small></button>${selectionOnly ? '' : `<button type="button" data-history-pick="${escape(c.id)}" data-side="left" title="Use as left snapshot">Set left</button><button type="button" data-history-pick="${escape(c.id)}" data-side="right" title="Use as right snapshot">Set right</button>`}</div>`;
      }).join('') || `<p class="empty-state">${model.loading ? 'Reading folder history…' : model.error ? 'Could not read history. Refresh to retry.' : model.more ? 'No matching commits in this page. Load older to continue.' : 'No matching commits in the loaded history.'}</p>`;
      selectedNote.textContent = model.selected.length ? `${model.selected.map(id => id.slice(0, 8)).join(' → ')} · ${model.selected.length}/2 selected` : `Ctrl/Cmd-click two commits, then Compare selected${onContextMenu ? ' or right-click' : ''}.`;
      compareSelected.disabled = model.loading || model.selected.length !== 2;
      clearSelected.hidden = !model.selected.length;
      note.textContent = model.loading ? 'Reading a page of folder history…' : model.error
        ? `History unavailable: ${model.error}. Refresh to retry; selected revisions are unchanged.`
        : `${model.commits.length} commits loaded · ${model.offset} repository commits checked · ${model.more ? 'Older history available' : 'End of history'}${model.head ? ` · history rooted at ${model.head.slice(0, 8)}` : ''}`;
      more.hidden = !model.more; more.disabled = refresh.disabled = !!model.loading;
    }
    async function load(append = false) {
      const mine = model;
      if (!mine || mine.loading || (append && !mine.more)) return;
      mine.loading = true; mine.error = ''; render();
      try {
        if (!invoke) throw new Error('History is available in the desktop application.');
        const page = await invoke('restore_path_history', { repositoryPath: mine.repositoryPath, relativePath: mine.path, headId: append ? mine.head : null, offset: append ? mine.offset : 0 });
        if (model !== mine) return;
        mine.commits = append ? [...mine.commits, ...page.commits] : page.commits;
        mine.selected = mine.selected.filter(id => mine.commits.some(c => c.id === id));
        mine.head = page.head_id; mine.offset = page.next_offset; mine.more = page.has_more;
      } catch (error) { if (model === mine) mine.error = String(error); }
      finally { if (model === mine) { mine.loading = false; render(); } }
    }
    function changed() {
      if (!model) return;
      model.left = left.value.trim(); model.right = right.value.trim();
      onChange();
    }
    if (!selectionOnly) {
      left.addEventListener('change', changed); right.addEventListener('change', changed);
      [left, right].forEach(input => input.addEventListener('keydown', event => { if (event.key === 'Enter') { event.preventDefault(); changed(); } }));
      root.querySelector('[data-history-compare]').addEventListener('click', changed);
      root.querySelector('[data-history-worktree]').addEventListener('click', () => { left.value = WORKTREE; changed(); });
      root.querySelector('[data-history-swap]').addEventListener('click', () => {
        // Keep the local side explicit and on the left, as in the existing engine.
        if (left.value === WORKTREE) return;
        [left.value, right.value] = [right.value, left.value]; changed();
      });
    }
    function select(event, button) {
      const id = button?.dataset.historySelect;
      if (!model?.commits.some(c => c.id === id)) return;
      model.selected = selectCommitPair(model.selected, id, event.ctrlKey || event.metaKey);
      render();
      rows.querySelector?.(`[data-history-select="${id}"]`)?.focus();
    }
    function comparePair() {
      if (!model || model.loading || model.selected.length !== 2 || !model.selected.every(id => model.commits.some(c => c.id === id))) return;
      if (selectionOnly) {
        onCompare({ context: model.context, path: model.path, left: model.selected[0], right: model.selected[1] });
        return;
      }
      [left.value, right.value] = model.selected;
      root.querySelector('.folder-history-picker').open = false;
      changed();
    }
    compareSelected.addEventListener('click', comparePair);
    clearSelected.addEventListener('click', () => { if (model) { model.selected = []; render(); } });
    rows.addEventListener('click', event => {
      const button = event.target.closest('[data-history-pick]');
      if (button && !selectionOnly) {
        if (!model?.commits.some(c => c.id === button.dataset.historyPick)) return;
        (button.dataset.side === 'left' ? left : right).value = button.dataset.historyPick; changed();
      } else select(event, event.target.closest('[data-history-select]'));
    });
    rows.addEventListener('keydown', event => {
      const button = event.target.closest('[data-history-select]');
      if (!button || !['Enter', ' '].includes(event.key)) return;
      event.preventDefault(); select(event, button);
    });
    rows.addEventListener('contextmenu', event => {
      const id = event.target.closest('[data-history-row]')?.dataset.historyRow;
      if (!onContextMenu || !model?.commits.some(c => c.id === id)) return;
      if (!model.selected.includes(id)) { model.selected = [id]; render(); }
      const mine = model, selection = mine.selected.join('|');
      const current = () => model === mine && model.selected.join('|') === selection;
      onContextMenu(event, [{ id: 'compare-folder-history', label: 'Compare selected commits', kind: 'compare',
        detail: model.selected.length === 2 ? model.selected.map(id => id.slice(0, 8)).join(' → ') : 'Ctrl/Cmd-click a second commit first. Read-only.',
        disabled: model.selected.length !== 2, run: () => { if (current()) comparePair(); } },
      { id: 'clear-folder-history', label: 'Clear selection', run: () => { if (current()) { model.selected = []; render(); } } }]);
    });
    search.addEventListener('input', render);
    more.addEventListener('click', () => load(true)); refresh.addEventListener('click', () => load());
    return {
      activate(context, path) {
        if (model?.context === context && model.path === path) return;
        model = { context, repositoryPath: context.repositoryPath, path, left: WORKTREE, right: 'HEAD', commits: [], selected: [], offset: 0, head: null, more: false, loading: false, error: '' };
        search.value = ''; render(); load();
      },
      selection() { return model ? { left: model.left, right: model.right } : { left: WORKTREE, right: 'HEAD' }; },
      deactivate() { model = null; },
      // Kept available to explicit refresh actions and focused unit tests.
      load,
    };
  }
  const api = { WORKTREE, origin, projectPath, createHistory, selectCommitPair };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  global.ComparisonNavigation = api;
})(typeof window !== 'undefined' ? window : globalThis);
