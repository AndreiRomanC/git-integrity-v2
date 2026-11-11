// Remote presentation only. The caller owns Browse, comparison and the Add form.
function createModuleInspector(dialog, { readHistory, useRevision, compare }) {
  const find = selector => dialog.querySelector(selector);
  const rows = find('#inspectRows'), search = find('#inspectSearch'), more = find('#inspectMore');
  const graph = find('#inspectGraph'), scope = find('#inspectScope'), status = find('#inspectStatus');
  const use = find('#inspectUse'), compareButton = find('#inspectCompare');
  const tabs = Array.from(dialog.querySelectorAll('[data-inspect-tab]'));
  let session = null;
  const live = s => s && session === s && dialog.open && s.source.valid();
  const isHistory = s => s.tab === 'history' || s.tab === 'graph';
  const historyKey = s => s.tab === 'graph' ? 'graph' : s.anchor?.revision;
  const label = item => item.kind === 'commit' ? item.revision.slice(0, 12) : item.name;

  function close() {
    session = null;
    if (dialog.open) dialog.close();
  }

  function refreshRefs() {
    const s = session;
    if (!s) return;
    if (!live(s)) { close(); return; }
    const refs = s.source.versions();
    s.defaultRef = refs.find(ref => ref.kind === 'branch' && ref.name === s.source.repo.default_branch);
    s.anchor ??= s.defaultRef || refs.find(ref => ref.kind === 'branch') || refs[0];
    render(s);
  }

  function updateSelection(s) {
    for (const row of rows.querySelectorAll('[data-revision]')) {
      const selected = s.selected.some(item => item.revision === row.dataset.revision
        && (isHistory(s) || (item.kind === row.dataset.versionKind && item.name === row.dataset.name)));
      row.classList.toggle('version-compare-selected', selected);
      row.setAttribute('aria-pressed', String(selected));
    }
    use.disabled = s.selected.length !== 1;
    compareButton.disabled = s.selected.length !== 2;
    status.textContent = s.selected.length
      ? s.selected.map(item => item.kind === 'commit' ? label(item) : `${item.name} @ ${item.revision.slice(0, 8)}`).join(' → ')
      : 'Ctrl/Cmd-click two revisions to compare.';
    compareButton.title = 'Ctrl/Cmd-click two different commits to compare them.';
    status.title = status.textContent;
  }

  function render(s) {
    const history = isHistory(s), page = history && s.pages.get(historyKey(s));
    const ready = s.source.ready();
    const query = search.value.trim();
    graph.hidden = !history;
    graph.textContent = s.tab === 'graph' ? 'Back to revision history' : 'Open full graph';
    graph.disabled = !s.anchor || !ready;
    for (const button of tabs) {
      const active = button.dataset.inspectTab === (history ? 'history' : s.tab);
      button.classList.toggle('active', active);
      button.setAttribute('aria-pressed', String(active));
      button.disabled = button.dataset.inspectTab === 'history' && (!s.anchor || !ready);
    }
    search.placeholder = history ? 'Filter loaded commits by message, author or SHA…' : `Filter ${s.tab === 'branch' ? 'branches' : 'tags'} by name or SHA…`;
    const all = history ? page?.commits || [] : s.source.versions().filter(ref => ref.kind === s.tab);
    s.visible = all.filter(ref => matchesSubmoduleVersion(ref, query));
    scope.textContent = history
      ? `${s.tab === 'graph' ? 'All branches and tags' : 'From ' + (s.anchor ? label(s.anchor) : 'default branch')} · ${all.length} commits loaded`
      : `${s.visible.length} ${s.tab === 'tag' ? 'tags' : 'branches'}`;
    const graphVisible = s.tab === 'graph' && !query && s.visible.length > 0;
    rows.classList.toggle('inspect-graph', graphVisible);
    const model = graphVisible ? buildGraphModel(s.visible, s.defaultRef?.revision) : null;
    const width = model ? model.reduce((n, row) => Math.max(n, row.before.length, row.after.length, row.lane + 1), 1) * LANE_WIDTH : 0;
    rows.style.setProperty('--inspect-lanes', width + 'px');
    const refNames = new Map();
    if (history) for (const ref of s.source.versions()) {
      if (ref.kind === 'commit') continue;
      if (!refNames.has(ref.revision)) refNames.set(ref.revision, []);
      refNames.get(ref.revision).push(ref.name);
    }
    rows.innerHTML = (model ? '<svg class="graph-overlay" aria-hidden="true"></svg>' : '') + s.visible.map(item => {
      const names = refNames.get(item.revision);
      const badge = names ? names.join(' · ') : item === s.defaultRef ? 'Default branch' : '';
      const context = badge ? `<small class="inspect-ref-label" title="${escapeSubmoduleVersionHtml(badge)}">${escapeSubmoduleVersionHtml(badge)}</small>` : '';
      return submoduleVersionRow(item, item.kind.toUpperCase(), context);
    }).join('');
    for (const row of rows.querySelectorAll('[data-revision]')) row.setAttribute('role', 'button');
    if (model) drawGraphOverlay(model, width, { container: rows, rowSelector: '[data-revision]', truncated: page.more });
    let message = '';
    if (!ready) message = s.source.loading() ? 'Reading branches and tags…' : 'Could not load refs. Return to Browse to retry.';
    else if (page?.error) message = page.error;
    else if (page?.loading) message = 'Loading repository history…';
    else if (!s.visible.length) message = query ? 'No matches in the loaded results.' : 'No versions found.';
    if (message) {
      const note = document.createElement('div');
      note.className = 'version-loading'; note.setAttribute('role', 'status'); note.textContent = message;
      rows.append(note);
    }
    more.hidden = !page || page.loading || (!page.more && !page.error);
    more.textContent = page?.error ? 'Retry history' : 'Load older commits';
    updateSelection(s);
  }

  async function loadPage(s, next = false) {
    if (!live(s) || !isHistory(s) || !s.anchor) return;
    const key = historyKey(s);
    let page = s.pages.get(key);
    if (page && (page.loading || (!page.error && (!next || !page.more)))) return;
    if (!page) { page = { commits: [], more: false }; s.pages.set(key, page); }
    const revisions = s.tab === 'graph'
      ? [...new Set(s.source.versions().filter(ref => ref.kind === 'branch' || ref.kind === 'tag').map(ref => ref.revision))]
      : [s.anchor.revision];
    page.loading = true; page.error = '';
    render(s);
    try {
      const result = await readHistory({ repositoryPath: s.source.repositoryPath, owner: s.source.repo.owner,
        repositoryName: s.source.repo.name, revisions, offset: page.commits.length });
      if (!live(s)) return;
      page.commits.push(...result.commits.map(commit => ({ ...commit, kind: 'commit', revision: commit.id, name: commit.id.slice(0, 12) })));
      page.more = result.has_more;
    } catch (error) { page.error = String(error); }
    finally {
      page.loading = false;
      if (live(s) && isHistory(s) && key === historyKey(s)) render(s);
    }
  }

  function setTab(tab) {
    const s = session;
    if (!live(s)) return;
    s.tab = tab; search.value = ''; rows.parentElement.scrollTop = 0;
    render(s);
    if (isHistory(s)) loadPage(s);
  }

  function select(event) {
    const s = session;
    if (!live(s)) return;
    const copy = event.target.closest('[data-copy-sha]');
    if (copy) { navigator.clipboard?.writeText(copy.dataset.copySha).catch(() => {}); return; }
    const row = event.target.closest('[data-revision]');
    if (!row) return;
    const item = s.visible.find(ref => ref.revision === row.dataset.revision && ref.kind === row.dataset.versionKind && ref.name === row.dataset.name);
    if (!item) return;
    const ids = selectSubmoduleCompareRevision(s.selected.map(ref => ref.revision), item.revision, event.ctrlKey || event.metaKey);
    s.selected = ids.map(id => id === item.revision ? item : s.selected.find(ref => ref.revision === id));
    if (!isHistory(s)) s.anchor = item;
    updateSelection(s);
  }
  rows.addEventListener('click', select);
  rows.addEventListener('keydown', event => {
    if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); select(event); }
  });
  tabs.forEach(button => button.addEventListener('click', () => setTab(button.dataset.inspectTab)));
  graph.addEventListener('click', () => setTab(session?.tab === 'graph' ? 'history' : 'graph'));
  search.addEventListener('input', () => { if (live(session)) render(session); });
  more.addEventListener('click', () => loadPage(session, true));
  dialog.querySelectorAll('[data-inspect-back]').forEach(button => button.addEventListener('click', close));
  dialog.addEventListener('close', () => { if (!dialog.open) session = null; });
  use.addEventListener('click', () => {
    if (!live(session) || session.selected.length !== 1) return;
    const [ref] = session.selected; close(); useRevision(ref);
  });
  compareButton.addEventListener('click', () => {
    if (!live(session) || session.selected.length !== 2) return;
    const pair = session.selected; close(); compare(pair);
  });
  return { close, refreshRefs, open(source) {
    const initial = /^[0-9a-f]{40}$/i.test(source.initialRef?.revision || '') ? source.initialRef : null;
    session = { source, anchor: initial, tab: initial?.kind === 'tag' ? 'tag' : 'branch',
      pages: new Map(), selected: initial ? [initial] : [], visible: [] };
    find('#inspectTitle').textContent = source.repo.full_name;
    search.value = ''; dialog.showModal(); rows.parentElement.scrollTop = 0; refreshRefs(); search.focus();
  } };
}
