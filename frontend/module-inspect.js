// Remote presentation only. The caller owns Browse, comparison and the Add form.
function createModuleInspector(dialog, { readHistory, readDetails, download, useRevision, compare }) {
  const find = selector => dialog.querySelector(selector);
  const rows = find('.version-list'), search = find('.version-search'), more = find('#inspectMore');
  const graph = find('[data-version-map]'), scope = find('#inspectScope'), status = find('#inspectStatus');
  const use = find('#inspectUse'), compareButton = find('#inspectCompare');
  const picker = find('.version-history-picker'), branchSelect = picker.querySelector('select');
  const details = find('#inspectDetails');
  const tabs = Array.from(dialog.querySelectorAll('[data-version-filter]'));
  const view = { branch: 'branch', tag: 'tag', commit: 'history', complete: 'graph' };
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
    s.tips = indexSubmoduleRefTips(refs);
    s.defaultRef = refs.find(ref => ref.kind === 'branch' && ref.name === s.source.repo.default_branch);
    s.anchor ??= s.defaultRef || refs.find(ref => ref.kind === 'branch') || refs[0];
    find('#inspectDefaultBranch').textContent = s.defaultRef ? `Default · ${s.defaultRef.name} @ ${s.defaultRef.revision.slice(0, 8)}` : s.source.repo.full_name;
    render(s);
  }

  function updateSelection(s) {
    for (const row of rows.querySelectorAll('[data-revision]')) {
      const selected = s.selected.some(item => item.revision === row.dataset.revision
        && (isHistory(s) || (item.kind === row.dataset.versionKind && item.name === row.dataset.name)));
      row.classList.toggle('version-compare-selected', selected);
      row.setAttribute('aria-label', `${row.dataset.name} · ${row.dataset.revision}${selected ? ' · selected' : ''}`);
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
    graph.disabled = !s.anchor || !ready;
    for (const button of tabs) {
      const tab = view[button.dataset.versionFilter];
      button.classList.toggle('active', tab === s.tab);
      button.setAttribute('aria-pressed', String(tab === s.tab));
      button.disabled = (tab === 'history' || tab === 'graph') && graph.disabled;
    }
    picker.hidden = s.tab !== 'history';
    if (!picker.hidden) {
      s.choices = historyBranchOptions(s.source.versions(), s.anchor);
      showHistoryBranchPicker(picker, s.choices, s.anchor);
    }
    details.disabled = !ready || !s.tips.size || s.loadingDetails || s.detailsLoaded;
    details.textContent = s.loadingDetails ? 'Loading details…' : s.detailsLoaded ? 'Commit details loaded' : 'Load commit details';
    search.placeholder = history ? 'Filter loaded commits by message, author or SHA…' : `Filter ${s.tab === 'branch' ? 'branches' : 'tags'} by name or SHA…`;
    const all = history ? page?.commits || [] : s.source.versions().filter(ref => ref.kind === s.tab);
    s.visible = all.filter(ref => matchesSubmoduleVersion({ ...ref, ...s.metadata.get(ref.revision) }, query));
    scope.textContent = history
      ? `${s.tab === 'graph' ? 'All branches and tags' : 'From ' + (s.anchor ? label(s.anchor) : 'default branch')} · ${all.length} commits loaded`
      : `${s.visible.length} ${s.tab === 'tag' ? 'tags' : 'branches'}`;
    const graphVisible = s.tab === 'graph' && !query && s.visible.length > 0;
    rows.classList.toggle('inspect-graph', graphVisible);
    const model = graphVisible ? buildGraphModel(s.visible, s.defaultRef?.revision) : null;
    const width = model ? model.reduce((n, row) => Math.max(n, row.before.length, row.after.length, row.lane + 1), 1) * LANE_WIDTH : 0;
    rows.style.setProperty('--inspect-lanes', width + 'px');
    rows.innerHTML = (model ? '<svg class="graph-overlay" aria-hidden="true"></svg>' : '') + s.visible.map(item => {
      const tips = s.tips.get(item.revision) || [];
      const related = item.kind === 'commit' ? tips : relatedSubmoduleVersionRefs(item, tips);
      return submoduleVersionRowHtml({ ...item, ...s.metadata.get(item.revision),
        cross_selected: s.cross?.kind === item.kind && s.cross.name === item.name,
        default_branch: item === s.defaultRef, related_refs: related }, true);
    }).join('');
    s.drawGraph = model && (() => drawGraphOverlay(model, width, { container: rows, rowSelector: '[data-revision]', truncated: page.more }));
    s.drawGraph?.();
    let message = '';
    if (!ready) message = s.source.loading() ? 'Reading branches and tags…' : 'Could not load refs. Return to Browse to retry.';
    else if (s.detailsError) message = s.detailsError;
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
      for (const commit of result.commits) s.metadata.set(commit.id, commit);
      page.commits.push(...result.commits.map(commit => ({ ...commit, kind: 'commit', revision: commit.id, name: commit.id.slice(0, 12) })));
      page.more = result.has_more;
    } catch (error) { page.error = String(error); }
    finally {
      page.loading = false;
      if (live(s) && isHistory(s) && key === historyKey(s)) render(s);
    }
  }

  function setTab(tab, cross = null) {
    const s = session;
    if (!live(s)) return;
    s.tab = tab; s.cross = cross; search.value = ''; rows.parentElement.scrollTop = 0;
    render(s);
    if (isHistory(s)) loadPage(s);
  }

  function select(event) {
    const s = session;
    if (!live(s)) return;
    const link = event.target.closest('[data-related-version]');
    if (link) {
      revealSubmoduleVersion(rows, link, (tab, target) => {
        const ref = s.source.versions().find(ref => ref.kind === target.kind && ref.name === target.name);
        if (ref) {
          s.anchor = ref;
          if (s.selected.length < 2) s.selected = [ref];
        }
        setTab(tab, target);
      });
      return;
    }
    if (event.target.closest('summary')) return;
    const copy = event.target.closest('[data-copy-sha]');
    if (copy) { navigator.clipboard?.writeText(copy.dataset.copySha).catch(() => {}); return; }
    const row = event.target.closest('[data-revision]');
    if (!row) return;
    const item = s.visible.find(ref => ref.revision === row.dataset.revision && ref.kind === row.dataset.versionKind && ref.name === row.dataset.name);
    if (!item) return;
    const exportButton = event.target.closest('[data-download-version]');
    if (exportButton) { download(item, exportButton); return; }
    const ids = selectSubmoduleCompareRevision(s.selected.map(ref => ref.revision), item.revision, event.ctrlKey || event.metaKey);
    s.selected = ids.map(id => id === item.revision ? item : s.selected.find(ref => ref.revision === id));
    if (!isHistory(s) && !event.ctrlKey && !event.metaKey) s.anchor = item;
    updateSelection(s);
  }
  rows.addEventListener('toggle', () => session?.drawGraph?.(), true);
  rows.addEventListener('click', select);
  rows.addEventListener('keydown', event => {
    if (event.target.closest('button, summary')) return; // Native activation dispatches one click.
    if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); select(event); }
  });
  tabs.forEach(button => button.addEventListener('click', () => setTab(view[button.dataset.versionFilter])));
  graph.addEventListener('click', () => setTab('graph'));
  branchSelect.addEventListener('change', () => {
    const s = session;
    if (!live(s) || !s.choices[branchSelect.selectedIndex]) return;
    s.anchor = s.choices[branchSelect.selectedIndex];
    setTab('history');
  });
  details.addEventListener('click', async () => {
    const s = session;
    if (!live(s) || details.disabled) return;
    s.loadingDetails = true; s.detailsError = ''; render(s);
    try {
      const revisions = [...s.tips.keys()].filter(id => !s.metadata.has(id));
      const result = revisions.length ? await readDetails({ repositoryPath: s.source.repositoryPath, owner: s.source.repo.owner,
        repositoryName: s.source.repo.name, revisions }) : [];
      if (!live(s)) return;
      for (const commit of result) s.metadata.set(commit.id, commit);
      s.detailsLoaded = true;
    } catch (error) { s.detailsError = String(error); }
    finally { s.loadingDetails = false; if (live(s)) render(s); }
  });
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
    session = { source, anchor: null, tab: 'branch',
      pages: new Map(), metadata: new Map(), selected: [], visible: [] };
    find('#inspectTitle').textContent = source.repo.full_name;
    search.value = ''; dialog.showModal(); rows.parentElement.scrollTop = 0; refreshRefs(); search.focus();
  } };
}
