const invoke = window.__TAURI__?.core?.invoke;
const $ = selector => document.querySelector(selector);
// Lane colors only — never branch identity, never commit state. Deliberately no
// red: that's reserved elsewhere in the app for danger/conflict/delete states,
// so a lane must never look like an error.
const palette = ['#58a6ff', '#39c5cf', '#48cc7e', '#f0b65a', '#b294ff', '#e17fd5'];
const directoryCache = new Map();
let submoduleMenuData = null;
let versionFilter = 'branch';
const recentRepos = JSON.parse(localStorage.getItem('recentRepos') || '[]');

function addRecentRepo(path, name) {
  const existing = recentRepos.findIndex(r => r.path === path);
  if (existing >= 0) recentRepos.splice(existing, 1);
  recentRepos.unshift({ path, name, date: new Date().toISOString() });
  if (recentRepos.length > 10) recentRepos.pop();
  localStorage.setItem('recentRepos', JSON.stringify(recentRepos));
  renderRecentRepos();
}

const state = { repository: null, branches: [], commits: [], allCommits: [], changes: [], selectedCommit: null, view: 'explorer', currentPath: '', entries: [], selectedEntry: null, historyScope: '', commanderPath: '', commanderRows: [], remoteRef: '', remotes: [], graphContext: null, editingPath: '', editorOriginal: '', publish: null, changesScope: 'global', commanderFocus: '', comparingRow: null, hasStash: false, stashes: [], editingConflict: null, mergeTarget: null,
  consoleMode: 'commands', consoleTranscript: [], consoleCmdHistory: [], consoleScopeOverride: null, graphPrimaryBranch: null, publishUpto: null };
const previewData = {
  repository: { name: 'vehicle-control', path: '/projects/vehicle-control', current_branch: 'feature/diagnostics' },
  branches: [
    { name: 'feature/diagnostics', current: true, remote: false },
    { name: 'main', current: false, remote: false },
    { name: 'release/2.4', current: false, remote: false },
    { name: 'origin/main', current: false, remote: true }
  ],
  commits: [
    { id: 'a39f21d', parents: ['9d2ac84'], subject: 'P:1842 Validate diagnostic event configuration', author: 'Andrei Pop', date: 'Today, 14:32', refs: ['HEAD', 'feature/diagnostics'], lane: 0 },
    { id: '9d2ac84', parents: ['eb81910'], subject: 'Add DEM configuration parser', author: 'Andrei Pop', date: 'Today, 11:08', refs: [], lane: 0 },
    { id: 'eb81910', parents: ['73da102', '24f5a88'], subject: 'Merge release/2.4 into main', author: 'Maria Ionescu', date: 'Yesterday', refs: ['main'], lane: 0 },
    { id: '24f5a88', parents: ['51bca30'], subject: 'Prepare release configuration', author: 'Victor Ene', date: 'Aug 12', refs: ['release/2.4'], lane: 1 },
    { id: '73da102', parents: ['51bca30'], subject: 'Refactor service dispatcher', author: 'Maria Ionescu', date: 'Aug 12', refs: [], lane: 0 },
    { id: '51bca30', parents: [], subject: 'Initial project structure', author: 'Maria Ionescu', date: 'Aug 09', refs: [], lane: 0 }
  ],
  changes: [
    { status: 'M', path: 'src/dem/configuration.c', staged: false },
    { status: 'A', path: 'tests/dem_configuration_test.c', staged: true }
  ],
  entries: [
    { name: 'src', relative_path: 'src', kind: 'folder', status: '•', tracked: true, size: 0, modified: 1786638900 },
    { name: 'diagnostics-core', relative_path: 'diagnostics-core', kind: 'submodule', status: '', tracked: true, size: 0, modified: 1786552500 },
    { name: 'tests', relative_path: 'tests', kind: 'folder', status: '•', tracked: true, size: 0, modified: 1786466100 },
    { name: 'Cargo.toml', relative_path: 'Cargo.toml', kind: 'file', status: 'M', tracked: true, size: 1834, modified: 1786638900 },
    { name: 'README.md', relative_path: 'README.md', kind: 'file', status: '', tracked: true, size: 5240, modified: 1786380000 },
    { name: 'vehicle-control.code-workspace', relative_path: 'vehicle-control.code-workspace', kind: 'file', status: '??', tracked: false, size: 386, modified: 1786638000 }
  ]
};
const refs = {
  repoName: $('#repoName'), repoPath: $('#repoPath'), branches: $('#branches'), graph: $('#graph'),
  graphView: $('#graphView'), emptyState: $('#emptyState'), laneLegend: $('#laneLegend'),
  details: $('#detailsPanel'), currentBranch: $('#currentBranch'), statusText: $('#statusText'),
  statusDot: $('#statusDot'), search: $('#search'), graphSubtitle: $('#graphSubtitle'),
  changeBadge: $('#changeBadge'), workspaceSubtitle: $('#workspaceSubtitle'), changes: $('#changes'),
  changesDrawer: $('#changesDrawer'), changesSummary: $('#changesSummary'), commitMessage: $('#commitMessage'),
  defaultCommitMessage: $('#defaultCommitMessage'), defaultCommitPolarionLink: $('#defaultCommitPolarionLink'),
  commitButton: $('#commitButton'), selectionText: $('#selectionText'), browserDialog: $('#browserDialog'),
  browserNotice: $('#browserNotice'), explorerView: $('#explorerView'), fileList: $('#fileList'),
  breadcrumbs: $('#breadcrumbs'), viewTitle: $('#viewTitle'), goUp: $('#goUp'), reloadFolder: $('#reloadFolder'),
  submoduleMenu: $('#submoduleMenu'), submoduleVersions: $('#submoduleVersions'), submoduleMenuName: $('#submoduleMenuName'), currentSubmoduleVersion: $('#currentSubmoduleVersion'),
  commitScope: $('#commitScope'), showPathHistory: $('#showPathHistory'), commitScopeDialog: $('#commitScopeDialog'), commitScopeName: $('#commitScopeName'), scopeCommitMessage: $('#scopeCommitMessage'), confirmScopeCommit: $('#confirmScopeCommit'),
  commanderView: $('#commanderView'), commanderRows: $('#commanderRows'), commanderBreadcrumbs: $('#commanderBreadcrumbs'), remoteRef: $('#remoteRef'), compareDialog: $('#compareDialog'), compareTitle: $('#compareTitle'), compareSubtitle: $('#compareSubtitle'), localCompare: $('#localCompare'), remoteCompare: $('#remoteCompare'),
  remotesView: $('#remotesView'), remoteCards: $('#remoteCards'), editorDialog: $('#editorDialog'), editorTitle: $('#editorTitle'), editorPath: $('#editorPath'), editorContent: $('#editorContent'), locationRepository: $('#locationRepository'), locationBranch: $('#locationBranch'), locationPath: $('#locationPath'), leaveSubmoduleGraph: $('#leaveSubmoduleGraph'), publishDialog: $('#publishDialog'), publishBranch: $('#publishBranch'), publishRemote: $('#publishRemote'), publishCommits: $('#publishCommits'), publishSummary: $('#publishSummary'), publishDestination: $('#publishDestination'), publishBadge: $('#publishBadge'), publishSubtitle: $('#publishSubtitle'), cloneDialog: $('#cloneDialog'), cloneUrl: $('#cloneUrl'), cloneParent: $('#cloneParent'), cloneName: $('#cloneName'), confirmClone: $('#confirmClone'), submoduleDialog: $('#submoduleDialog'), submoduleUrl: $('#submoduleUrl'), submoduleParent: $('#submoduleParent'), submoduleName: $('#submoduleName'), submoduleUsername: $('#submoduleUsername'), submoduleToken: $('#submoduleToken'), submoduleAddStatus: $('#submoduleAddStatus'), confirmAddSubmodule: $('#confirmAddSubmodule'), operationToast: $('#operationToast'), drawerScopeTitle: $('#drawerScopeTitle'),
  mergeBranchDialog: $('#mergeBranchDialog'), mergeBranchSubtitle: $('#mergeBranchSubtitle'), mergeBranchCurrent: $('#mergeBranchCurrent'), mergeBranchSource: $('#mergeBranchSource'), mergeBranchStatus: $('#mergeBranchStatus'), confirmMergeBranch: $('#confirmMergeBranch'),
  stashesDialog: $('#stashesDialog'), stashesList: $('#stashesList'),
  newBranchDialog: $('#newBranchDialog'), newBranchFrom: $('#newBranchFrom'), newBranchOriginStatus: $('#newBranchOriginStatus'), newBranchName: $('#newBranchName'), newBranchStatus: $('#newBranchStatus'), confirmNewBranch: $('#confirmNewBranch'),
  conflictsDialog: $('#conflictsDialog'), conflictsTitle: $('#conflictsTitle'), conflictsSubtitle: $('#conflictsSubtitle'), conflictsList: $('#conflictsList'), conflictsCommitMessage: $('#conflictsCommitMessage'), conflictsCommitMessageLabel: $('#conflictsCommitMessageLabel'), conflictsLocalNote: $('#conflictsLocalNote'), conflictsStatus: $('#conflictsStatus'), confirmCompleteMerge: $('#confirmCompleteMerge'), abortMergeButton: $('#abortMerge'),
  mergeConflictsBanner: $('#mergeConflictsBanner'), mergeConflictsSubtitle: $('#mergeConflictsSubtitle')
};

function esc(value = '') { return String(value).replace(/[&<>'"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;',"'":'&#39;','"':'&quot;'}[c])); }
function commitSubjectHtml(subject = '') {
  // Commit messages carry Polarion work item IDs like "P:OMBMS-21610" (project
  // code, dash, number) — e.g. "P:OMBMS-21610 - 0ADOBD - DEM/FIM ...". The
  // Polarion URL needs the project code on its own (for the /project/ segment)
  // plus the full ID again for the workitem id. Project codes can start with
  // a digit too (e.g. "P:0ADOPM-1803"), so the first character must allow
  // digits, not just letters — a letter-only first character was silently
  // failing to recognize those as links.
  let cursor = 0; const parts = [];
  for (const match of subject.matchAll(/P:([A-Za-z0-9][A-Za-z0-9_]*-\d+)/g)) {
    parts.push(esc(subject.slice(cursor, match.index)));
    const workitemId = match[1]; const project = workitemId.split('-')[0];
    const url = `https://polarion.vitesco.io/polarion/#/project/${project}/workitem?id=${workitemId}`;
    parts.push(`<a class="polarion-link" href="${esc(url)}" target="_blank" rel="noreferrer" title="Open Polarion ${workitemId}">${esc(match[0])}</a>`);
    cursor = match.index + match[0].length;
  }
  parts.push(esc(subject.slice(cursor))); return parts.join('');
}
function status(message, kind = '') { refs.statusText.textContent = message; refs.statusDot.className = `status-dot ${kind}`; }
let toastTimer; function showOperationToast(message, kind = '') { clearTimeout(toastTimer); refs.operationToast.textContent = message; refs.operationToast.className = `operation-toast ${kind}`; refs.operationToast.hidden = false; toastTimer = setTimeout(() => { refs.operationToast.hidden = true; }, 7000); }

function handleError(error) {
  const msg = String(error).toLowerCase();
  if (msg.includes('timed out')) {
    // The backend only ever reports this after it has already force-killed
    // a genuinely stuck git process (10 minutes with zero progress) — by
    // this point nothing is still running, so it's safe to automatically
    // bring the repository view back to a clean, known state instead of
    // leaving it looking frozen. This should be rare: everything short of a
    // truly wedged process (a stalled network transfer, a held filesystem
    // lock) finishes on its own well before that backstop ever triggers.
    const friendly = 'That got stuck and was stopped automatically. Reloading the project…';
    status(friendly, 'error'); showOperationToast(friendly, 'error');
    if (state.repository) setTimeout(() => loadRepository(state.repository.path, { keepPath: true }), 300);
    return friendly;
  }
  if (msg.includes('no changes to commit')) { const friendly = 'Nothing to commit — this selection has no uncommitted local changes.'; status(friendly, 'error'); return friendly; }
  const adviceMap = {
    'no upstream': 'Go to Branch Map (Ctrl+Shift+G) → Right-click branch → Set upstream',
    'not a git': 'Open a valid Git repository with File > Open Repository',
    'merge conflict': 'Resolve conflicts manually in the files, then stage them',
    'diverged': 'This needs manual resolution: open a terminal in that folder, run `git fetch` then `git merge origin/<branch>` (or `git rebase origin/<branch>`), resolve any conflicts, commit, then Push again from here.',
    'non-fast-forward': 'Use "Pull submodule" first. If it also refuses (diverged), resolve manually in a terminal: `git fetch`, `git merge origin/<branch>`, fix conflicts, commit, then push.',
    'authentication': 'Check your Git credentials and SSH keys',
    'permission denied': 'Check file permissions and access rights',
    'branch not found': 'Refresh (Ctrl+R) and verify the branch name',
    'remote not found': 'Add a remote in the Remotes view',
  };
  let advice = '';
  for (const [key, val] of Object.entries(adviceMap)) {
    if (msg.includes(key)) { advice = val; break; }
  }
  const fullMsg = advice ? `${error}\n💡 ${advice}` : String(error);
  status(fullMsg, 'error');
  return fullMsg;
}
function clearDetails(message) { refs.details.innerHTML = `<div class="details-empty"><div class="details-node"></div><strong>${esc(message)}</strong><span>Select an item in this view to see only relevant details and actions.</span></div>`; }

// window.confirm() / window.prompt() are unreliable inside Tauri's WKWebView (macOS) —
// they can silently no-op instead of showing anything, which looks like the app is
// broken. These custom dialogs use the same <dialog> element the rest of the app
// already relies on, so they are guaranteed to actually appear.
function customConfirm(message, options = {}) {
  return new Promise(resolve => {
    const dialog = $('#appConfirmDialog');
    $('#appConfirmTitle').textContent = options.title || 'Confirm';
    $('#appConfirmMessage').textContent = message;
    $('#appConfirmIcon').textContent = options.danger ? '!' : '?';
    const okButton = $('#appConfirmOk');
    okButton.textContent = options.okLabel || 'Confirm';
    okButton.className = options.danger ? 'danger' : 'confirm';
    const cleanup = (result) => { dialog.close(); okButton.removeEventListener('click', onOk); cancelButton.removeEventListener('click', onCancel); dialog.removeEventListener('cancel', onCancel); resolve(result); };
    const onOk = () => cleanup(true);
    const onCancel = () => cleanup(false);
    const cancelButton = $('#appConfirmCancel');
    okButton.addEventListener('click', onOk); cancelButton.addEventListener('click', onCancel); dialog.addEventListener('cancel', onCancel);
    dialog.showModal();
  });
}

function customPrompt(message, defaultValue = '', options = {}) {
  return new Promise(resolve => {
    const dialog = $('#appPromptDialog');
    $('#appPromptTitle').textContent = options.title || 'Enter value';
    $('#appPromptMessage').textContent = message;
    const input = $('#appPromptInput'); input.value = defaultValue;
    const okButton = $('#appPromptOk'); const cancelButton = $('#appPromptCancel');
    const cleanup = (result) => { dialog.close(); okButton.removeEventListener('click', onOk); cancelButton.removeEventListener('click', onCancel); input.removeEventListener('keydown', onKeydown); dialog.removeEventListener('cancel', onCancel); resolve(result); };
    const onOk = () => cleanup(input.value);
    const onCancel = () => cleanup(null);
    const onKeydown = (event) => { if (event.key === 'Enter') { event.preventDefault(); onOk(); } };
    okButton.addEventListener('click', onOk); cancelButton.addEventListener('click', onCancel); input.addEventListener('keydown', onKeydown); dialog.addEventListener('cancel', onCancel);
    dialog.showModal(); input.focus(); input.select();
  });
}

function openRepository() {
  if (!invoke) { refs.browserDialog.showModal(); return; }
  status('Choose a repository folder…', 'busy');
  invoke('choose_folder').then(path => path && loadRepository(path)).catch(error => handleError(error));
}

function validateCloneForm() { refs.confirmClone.disabled = !refs.cloneUrl.value.trim() || !refs.cloneParent.value.trim() || !refs.cloneName.value.trim(); }
function openCloneDialog() { refs.cloneUrl.value = ''; refs.cloneParent.value = ''; refs.cloneName.value = ''; refs.cloneName.dataset.edited = ''; validateCloneForm(); refs.cloneDialog.showModal(); refs.cloneUrl.focus(); }
async function chooseCloneParent() { if (!invoke) { refs.cloneParent.value = '/projects'; validateCloneForm(); return; } const path = await invoke('choose_folder'); if (path) { refs.cloneParent.value = path; validateCloneForm(); } }
async function confirmClone(event) {
  event.preventDefault(); const url = refs.cloneUrl.value.trim(); const parentPath = refs.cloneParent.value.trim(); const folderName = refs.cloneName.value.trim(); if (!url || !parentPath || !folderName) return;
  if (!invoke) { refs.cloneDialog.close(); status(`Preview: cloned ${folderName}`); return; }
  refs.confirmClone.disabled = true; refs.confirmClone.textContent = 'Cloning…'; status(`Cloning ${folderName}…`, 'busy');
  try { const path = await invoke('clone_repository', { url, parentPath, folderName }); refs.cloneDialog.close(); await loadRepository(path); status(`Cloned and opened ${folderName}`); }
  catch (error) { status(String(error), 'error'); refs.confirmClone.disabled = false; }
  finally { refs.confirmClone.textContent = 'Clone repository'; }
}

function suggestedRepositoryName(url) { return url.trim().replace(/\/$/, '').split(/[/:]/).pop()?.replace(/\.git$/i, '') || ''; }
function validateSubmoduleForm() { refs.confirmAddSubmodule.disabled = !refs.submoduleUrl.value.trim() || !refs.submoduleName.value.trim(); if (refs.submoduleDialog.open && refs.submoduleName.value.trim()) { refs.submoduleAddStatus.textContent = `Will add at /${state.currentPath ? `${state.currentPath}/` : ''}${refs.submoduleName.value.trim()}`; refs.submoduleAddStatus.className = 'submodule-operation-status'; } }
function openAddSubmoduleDialog() {
  if (!state.repository || state.view !== 'explorer') return;
  refs.submoduleUrl.value = ''; refs.submoduleName.value = ''; refs.submoduleUsername.value = ''; refs.submoduleToken.value = ''; refs.submoduleName.dataset.edited = '';
  refs.submoduleParent.value = state.currentPath ? `/${state.currentPath}` : '/ (repository root)';
  refs.submoduleAddStatus.textContent = `Destination: /${state.currentPath ? `${state.currentPath}/` : ''}…`; refs.submoduleAddStatus.className = 'submodule-operation-status';
  validateSubmoduleForm(); refs.submoduleDialog.showModal(); refs.submoduleUrl.focus();
}
async function confirmAddSubmodule() {
  const url = refs.submoduleUrl.value.trim(), folderName = refs.submoduleName.value.trim(), parentPath = state.currentPath, username = refs.submoduleUsername.value.trim(), accessToken = refs.submoduleToken.value;
  if (!url || !folderName || !state.repository) return;
  if (!invoke) { refs.submoduleDialog.close(); return status(`Preview: added ${folderName} in /${parentPath}`); }
  refs.confirmAddSubmodule.disabled = true; refs.confirmAddSubmodule.textContent = 'Adding…'; refs.submoduleAddStatus.textContent = `Cloning into /${parentPath ? `${parentPath}/` : ''}${folderName}…`; refs.submoduleAddStatus.className = 'submodule-operation-status busy'; status(`Cloning submodule ${folderName}…`, 'busy');
  try {
    const addedPath = await invoke('add_submodule', { repositoryPath: state.repository.path, parentPath, url, folderName, username, accessToken });
    refs.submoduleDialog.close(); directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(parentPath, { force: true }); selectEntry(addedPath);
    state.changesScope = 'global'; refs.changesDrawer.classList.add('open');
    const message = `${addedPath} added. Commit .gitmodules + the submodule link, then Publish/Push that commit to the server.`; status(message); showOperationToast(message);
  } catch (error) { const message = String(error); refs.submoduleAddStatus.textContent = `Not added: ${message}`; refs.submoduleAddStatus.className = 'submodule-operation-status error'; status(message, 'error'); showOperationToast(`Submodule was not added: ${message}`, 'error'); refs.confirmAddSubmodule.disabled = false; }
  finally { refs.confirmAddSubmodule.textContent = 'Add submodule'; }
}

async function loadRepository(path, options = {}) {
  status('Reading repository…', 'busy');
  const keepPath = options.keepPath ? state.currentPath : '';
  // A plain refresh must not silently kick you out of whatever view you were
  // looking at (e.g. Branch Map) back to Project Explorer — that made a
  // just-refreshed history look unchanged when really you just weren't
  // looking at it anymore. Only reset the view for a genuine fresh open.
  const keepView = options.keepPath ? state.view : 'explorer';
  try {
    const data = await invoke('load_repository', { path, force: Boolean(options.force) });
    // `data.commits` (assigned onto state.commits below) is always the full,
    // unscoped history — a scoped "History · <path>" view can't stay correctly
    // scoped through a refresh without re-querying that same scope, so it
    // falls back to the full Branch Map instead of showing stale-looking
    // scoped chrome over full data.
    directoryCache.clear(); Object.assign(state, data); state.allCommits = data.commits; state.historyScope = ''; state.view = keepView; state.commanderPath = options.keepPath ? state.commanderPath : ''; state.commanderRows = options.keepPath ? state.commanderRows : [];
    state.remoteRef = data.branches.find(branch => branch.remote)?.name || '';
    // The toolbar's "Pop stash" visibility must reflect whether a stash
    // actually exists on disk, not a separately hand-tracked flag — that
    // flag never got reconciled with reality, so it stayed false (hiding
    // "Pop stash") if the app was relaunched with a stash already pending,
    // and stayed true after popping just one of several stashes even when
    // more were still left (per-file stash makes having several at once
    // common). Deriving it fresh from the real list every reload fixes both.
    state.hasStash = state.stashes.length > 0; updateStashUI();
    await openDirectory(keepPath, { force: true }); status(`${data.commits.length} commits loaded`);
    addRecentRepo(path, data.repository.name);
    updatePublishIndicator();
    await checkForMergeConflicts();
  } catch (error) { handleError(error); }
}

function renderRecentRepos() {
  const list = $('#recentReposList');
  if (recentRepos.length === 0) { $('#recentReposBar').hidden = true; return; }
  $('#recentReposBar').hidden = false;
  list.innerHTML = recentRepos.map(repo => `<button class="recent-repo-btn" data-path="${esc(repo.path)}" title="${esc(repo.path)}">${esc(repo.name)}</button>`).join('');
  list.querySelectorAll('.recent-repo-btn').forEach(btn => btn.addEventListener('click', () => loadRepository(btn.dataset.path)));
}

$('#closeRecentRepos').addEventListener('click', () => $('#recentReposBar').hidden = true);

// Drag & Drop - open files in editor
document.addEventListener('dragover', (e) => {
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
});
document.addEventListener('drop', (e) => {
  e.preventDefault();
  if (!e.dataTransfer?.files?.length) return;
  const files = Array.from(e.dataTransfer.files);
  files.forEach(file => {
    if (file.type.startsWith('text/') || file.name.endsWith('.md') || file.name.endsWith('.txt')) {
      const reader = new FileReader();
      reader.onload = (event) => {
        state.editingPath = file.name;
        state.editorOriginal = event.target.result;
        $('#editorTitle').textContent = `Edit ${file.name}`;
        $('#editorPath').textContent = file.name;
        $('#editorContent').value = event.target.result;
        $('#editorDialog').showModal();
      };
      reader.readAsText(file);
    }
  });
});

async function updatePublishIndicator() {
  if (!invoke || !state.repository) return;
  try { state.remotes = await invoke('list_remotes', { repositoryPath: state.repository.path }); const remote = state.remotes[0]?.name, branch = state.repository.current_branch; if (!remote || !branch) { refs.publishBadge.textContent = '—'; refs.publishSubtitle.textContent = 'No remote or detached HEAD'; return; } const info = await invoke('publish_status', { repositoryPath: state.repository.path, branch, remote }); refs.publishBadge.textContent = info.commits.length; refs.publishSubtitle.textContent = info.commits.length ? `${info.commits.length} commit${info.commits.length === 1 ? '' : 's'} not on ${remote}` : 'Everything is on the server'; } catch (_) { refs.publishBadge.textContent = '!'; refs.publishSubtitle.textContent = 'Cannot compare with server branch'; }
}

function render() {
  const loaded = Boolean(state.repository);
  document.body.classList.toggle('commander-mode', ['commander','remotes'].includes(state.view));
  if (loaded && !state.remoteRef) state.remoteRef = state.branches.find(branch => branch.remote)?.name || '';
  refs.emptyState.hidden = loaded; refs.explorerView.hidden = !loaded || state.view !== 'explorer'; refs.commanderView.hidden = !loaded || state.view !== 'commander'; refs.graphView.hidden = !loaded || state.view !== 'graph'; refs.remotesView.hidden = !loaded || state.view !== 'remotes';
  refs.repoName.textContent = loaded ? state.repository.name : 'Open a repository';
  refs.repoPath.textContent = loaded ? state.repository.path : 'Choose an existing Git folder';
  refs.currentBranch.textContent = loaded ? (state.repository.current_branch || 'Detached HEAD') : 'No branch';
  refs.viewTitle.textContent = state.view === 'explorer' ? 'Project Explorer' : state.view === 'commander' ? 'Local ↔ Remote' : state.view === 'remotes' ? 'Remotes' : state.graphContext ? `Submodule Map · ${state.graphContext.name}` : state.historyScope ? `History · ${state.historyScope}` : 'Branch Map';
  refs.graphSubtitle.textContent = !loaded ? 'Navigate folders and inspect every item in your repository.' : state.view === 'explorer' ? `${state.entries.length} items in ${state.currentPath || state.repository.name}` : state.view === 'commander' ? 'Compare the workspace with a cached remote snapshot—no second checkout.' : state.view === 'remotes' ? 'Configured server locations and explicit fetch controls.' : `${state.commits.length} commits across ${state.branches.length} branches`;
  refs.search.placeholder = state.view === 'explorer' ? 'Filter this folder' : state.view === 'commander' ? 'Filter comparison' : 'Find commit or author';
  refs.search.closest('label').hidden = state.view === 'remotes';
  refs.goUp.hidden = refs.reloadFolder.hidden = !['explorer','commander'].includes(state.view); refs.goUp.disabled = state.view === 'explorer' ? !state.currentPath : !state.commanderPath;
  refs.commitScope.hidden = refs.showPathHistory.hidden = state.view !== 'explorer' || !loaded;
  $('#showFolderChanges').hidden = state.view !== 'explorer' || !loaded;
  $('#addSubmodule').hidden = state.view !== 'explorer' || !loaded;
  const folderChangeCount = loaded ? state.changes.filter(change => !state.currentPath || change.path === state.currentPath || change.path.startsWith(`${state.currentPath}/`)).length : 0;
  $('#showFolderChanges').textContent = `Folder changes · ${folderChangeCount}`;
  const activeScope = state.selectedEntry?.relative_path || state.currentPath;
  const activeName = state.selectedEntry?.name || (state.currentPath ? state.currentPath.split('/').pop() : state.repository?.name);
  refs.commitScope.textContent = state.selectedEntry ? `Commit ${activeName}` : state.currentPath ? 'Commit folder' : 'Commit repository';
  const scopeChangeCount = loaded ? state.changes.filter(change => !activeScope || change.path === activeScope || change.path.startsWith(`${activeScope}/`)).length : 0;
  refs.commitScope.disabled = !scopeChangeCount;
  refs.commitScope.title = scopeChangeCount ? '' : 'Nothing to commit here — no uncommitted local changes in this scope';
  $('#navExplorer').classList.toggle('active', state.view === 'explorer'); $('#navGraph').classList.toggle('active', state.view === 'graph');
  $('#navCommander').classList.toggle('active', state.view === 'commander');
  $('#navRemotes').classList.toggle('active', state.view === 'remotes');
  refs.locationRepository.textContent = state.graphContext?.name || state.repository?.name || '—'; refs.locationBranch.textContent = state.repository?.current_branch || 'Detached HEAD'; refs.locationPath.textContent = state.view === 'commander' ? `/${state.commanderPath}` : state.view === 'explorer' ? `/${state.currentPath}` : state.view === 'graph' ? 'commit history' : 'remote configuration';
  refs.leaveSubmoduleGraph.hidden = !state.graphContext;
  renderBranches(); renderExplorer(); renderCommander(); renderGraph(); renderRemotes(); renderChanges();
}

function renderCommanderBreadcrumbs() {
  const parts = state.commanderPath ? state.commanderPath.split('/') : []; let accumulated = '';
  refs.commanderBreadcrumbs.innerHTML = `<button class="crumb root" data-commander-path="">▰ ${esc(state.repository?.name || 'Repository')}</button>` + parts.map(part => {
    accumulated = accumulated ? `${accumulated}/${part}` : part; return `<span class="crumb-separator">›</span><button class="crumb" data-commander-path="${esc(accumulated)}">${esc(part)}</button>`;
  }).join('');
  refs.commanderBreadcrumbs.querySelectorAll('[data-commander-path]').forEach(button => button.addEventListener('click', () => openCommanderDirectory(button.dataset.commanderPath)));
}

function commanderSide(entry, side) {
  if (!entry) return `<span class="commander-side empty ${side}">— not present —</span>`;
  return `<span class="commander-side ${side}">${iconFor(entry)}<span class="commander-file-copy"><strong>${esc(entry.name)}</strong><small>${entry.kind}${entry.kind === 'file' ? ` · ${formatSize(entry.size)}` : ''}</small></span></span>`;
}

function renderCommander() {
  if (!state.repository) return;
  renderCommanderBreadcrumbs();
  const remoteBranches = state.branches.filter(branch => branch.remote);
  refs.remoteRef.innerHTML = remoteBranches.map(branch => `<option value="${esc(branch.name)}" ${branch.name === state.remoteRef ? 'selected' : ''}>${esc(branch.name)}</option>`).join('') || '<option value="">No remote refs</option>';
  const query = refs.search.value.trim().toLowerCase(); const rows = state.commanderRows.filter(row => !query || row.name.toLowerCase().includes(query));
  const upRow = state.commanderPath ? `<button class="commander-row commander-grid up-row" data-commander-up="1">
    <span class="commander-side local">${iconFor({kind:'folder'})}<span class="commander-file-copy"><strong>..</strong><small>Parent folder</small></span></span><span class="compare-state"><i></i></span><span class="commander-side remote">${iconFor({kind:'folder'})}<span class="commander-file-copy"><strong>..</strong><small>Parent folder</small></span></span>
  </button>` : '';
  refs.commanderRows.innerHTML = upRow + rows.map(row => `<button class="commander-row commander-grid ${row.relative_path === state.commanderFocus ? 'focused' : ''}" data-commander-entry="${esc(row.relative_path)}">
    ${commanderSide(row.local, 'local')}<span class="compare-state ${esc(row.status)}"><i></i>${esc(row.status.replace('-', ' '))}</span>${commanderSide(row.remote, 'remote')}</button>`).join('') || (state.commanderPath ? '' : '<div class="loading-row">No items to compare</div>');
  refs.commanderRows.querySelector('[data-commander-up]')?.addEventListener('click', () => { const parent = state.commanderPath.split('/').slice(0, -1).join('/'); openCommanderDirectory(parent); });
  refs.commanderRows.querySelectorAll('[data-commander-entry]').forEach(rowNode => {
    const row = state.commanderRows.find(item => item.relative_path === rowNode.dataset.commanderEntry);
    rowNode.addEventListener('dblclick', () => { const entry = row?.local || row?.remote; if (entry?.kind === 'folder') openCommanderDirectory(row.relative_path); });
    rowNode.addEventListener('click', () => { const localIsFile = !row?.local || row.local.kind === 'file'; const remoteIsFile = !row?.remote || row.remote.kind === 'file'; const eitherIsFile = row?.local?.kind === 'file' || row?.remote?.kind === 'file'; if (eitherIsFile && localIsFile && remoteIsFile) openFileCompare(row); });
  });
  const focused = refs.commanderRows.querySelector('.commander-row.focused'); if (focused) requestAnimationFrame(() => focused.scrollIntoView({ block: 'center' }));
}

async function openCommanderDirectory(path) {
  if (!state.remoteRef) { status('No remote-tracking branch is available. Fetch the repository first.', 'error'); return; }
  state.commanderPath = path; refs.commanderRows.innerHTML = '<div class="loading-row"><i class="spinner"></i>Comparing local and remote…</div>';
  if (!invoke) { state.commanderRows = previewCommanderRows(); render(); return; }
  try { const result = await invoke('compare_remote_directory', { repositoryPath: state.repository.path, relativePath: path, remoteRef: state.remoteRef }); state.commanderRows = result.rows; render(); status(`Compared with ${state.remoteRef.slice(0, 40)}`); }
  catch (error) { status(String(error), 'error'); refs.commanderRows.innerHTML = `<div class="loading-row">${esc(String(error))}</div>`; }
}

function previewCommanderRows() {
  return previewData.entries.map((entry, index) => ({ name: entry.name, relative_path: entry.relative_path, local: index === 2 ? null : entry, remote: index === 5 ? null : { ...entry, size: index === 3 ? 1720 : entry.size }, status: index === 5 ? 'local-only' : index === 3 ? 'modified' : index === 2 ? 'remote-only' : 'same' }));
}

async function openFileCompare(row) {
  state.comparingRow = row;
  const localMissing = !row.local; const remoteMissing = !row.remote;
  refs.compareTitle.textContent = row.name;
  refs.compareSubtitle.textContent = localMissing ? `Only exists on ${state.remoteRef} — not fetched locally yet` : remoteMissing ? `Only exists locally — not on ${state.remoteRef}` : `Local workspace compared with ${state.remoteRef}`;
  refs.localCompare.textContent = refs.remoteCompare.textContent = 'Loading…';
  setCompareActionStatus('Ready — choose one action for this file.');
  // Stage/Unstage/Discard need a local file; Restore-from-remote needs a remote file.
  $('#compareStage').disabled = localMissing; $('#compareUnstage').disabled = localMissing; $('#compareRestoreHead').disabled = localMissing; $('#compareRestoreRemote').disabled = remoteMissing;
  refs.compareDialog.showModal();
  if (!invoke) { renderComparisonContents('version = "0.2.0"\nfeatures = ["local"]', 'version = "0.1.0"\nfeatures = []'); return; }
  try { const comparison = await invoke('compare_file_contents', { repositoryPath: state.repository.path, relativePath: row.relative_path, remoteRef: state.remoteRef }); renderComparisonContents(comparison.local_content || (localMissing ? '(file does not exist locally)' : ''), comparison.remote_content); }
  catch (error) { refs.localCompare.textContent = String(error); refs.remoteCompare.textContent = ''; }
}

function setCompareActionStatus(message, kind = '') { const node = $('#compareActionStatus'); node.textContent = message; node.className = `compare-status-line ${kind}`.trim(); }
function setCompareActionsDisabled(disabled) {
  if (disabled) { ['#compareRestoreRemote','#compareRestoreHead','#compareStage','#compareUnstage'].forEach(selector => { $(selector).disabled = true; }); return; }
  // Re-enabling must respect which side of the comparison actually exists.
  const row = state.comparingRow; const localMissing = !row?.local; const remoteMissing = !row?.remote;
  $('#compareStage').disabled = localMissing; $('#compareUnstage').disabled = localMissing; $('#compareRestoreHead').disabled = localMissing; $('#compareRestoreRemote').disabled = remoteMissing;
}

async function applyFileRecovery(forcedAction = '') {
  const action = forcedAction, row = state.comparingRow;
  if (!row) { setCompareActionStatus('No file is selected for comparison.', 'error'); return; }
  if (!action) { setCompareActionStatus('No action was received. Close Compare and open the file again.', 'error'); return; }
  const descriptions = { remote: `fetch ${state.remoteRef} and replace the working file with that server snapshot — may differ from your last local commit; staging is not changed`, head: 'discard working-file edits and restore your last local commit (HEAD) — does not fetch anything from the server', stage: 'add the current working-file content to the staging area', unstage: 'remove only the staging-area entry while keeping working-file edits' };
  if (['remote','head'].includes(action) && !await customConfirm(`This will ${descriptions[action]} for ${row.relative_path}. Continue?`, { title: action === 'head' ? 'Restore from last commit (HEAD)' : 'Restore from server', danger: true, okLabel: 'Continue' })) return;
  if (!invoke) { setCompareActionStatus(`Preview complete: ${descriptions[action]}`, 'success'); return; }
  try {
    setCompareActionsDisabled(true); setCompareActionStatus(action === 'remote' ? `Contacting server and fetching ${state.remoteRef}…` : `Applying ${action} to ${row.name}…`, 'busy');
    status(action === 'remote' ? `Fetching ${state.remoteRef} and restoring ${row.name}…` : `Applying action to ${row.name}…`, 'busy');
    if (action === 'remote') {
      await invoke('restore_remote_file', { repositoryPath: state.repository.path, relativePath: row.relative_path, remoteRef: state.remoteRef });
      const comparison = await invoke('compare_file_contents', { repositoryPath: state.repository.path, relativePath: row.relative_path, remoteRef: state.remoteRef }); renderComparisonContents(comparison.local_content, comparison.remote_content); refs.compareSubtitle.textContent = `LOCAL NOW MATCHES ${state.remoteRef} · fetched from server`; directoryCache.clear();
      const msg = `${row.name}: the file on disk was overwritten with the ${state.remoteRef} version. Yes — the project was updated.`;
      setCompareActionStatus(`Success: ${row.name} now contains the fetched server version.`, 'success'); status(msg); showOperationToast(msg, 'success'); return;
    }
    if (action === 'head') await invoke('restore_file', { repositoryPath: state.repository.path, relativePath: row.relative_path, sourceRef: 'HEAD' }); else await invoke(action === 'stage' ? 'stage_files' : 'unstage_files', { path: state.repository.path, files: [row.relative_path] });
    const folder = state.commanderPath; refs.compareDialog.close(); await loadRepository(state.repository.path); state.view = 'commander'; state.commanderPath = folder; state.commanderFocus = row.relative_path; await openCommanderDirectory(folder);
    const doneMsg = action === 'head' ? `${row.name}: reverted to HEAD. The project on disk was updated.` : action === 'stage' ? `${row.name}: staged.` : `${row.name}: unstaged, edits kept on disk.`;
    status(doneMsg); showOperationToast(doneMsg, 'success');
  }
  catch (error) { const message = String(error); setCompareActionStatus(`Failed: ${message}`, 'error'); status(message, 'error'); showOperationToast(`Failed: ${message}`, 'error'); }
  finally { setCompareActionsDisabled(false); }
}
function updateRecoveryHelp(action = '') { const help = { stage: '`git add` – Stage the current file content', unstage: 'UNSTAGE (`git restore --staged`) – unstages only, your edits on disk stay exactly as they are', head: 'RESTORE FROM LAST COMMIT (`git checkout HEAD -- file`) – no network access; discards edits using what you already have locally', remote: `RESTORE FROM SERVER – fetches ${state.remoteRef || 'remote'} first, then overwrites the file with that server version (can differ from your last local commit if the server has newer changes)` }; if (action) { $('#recoveryHelp').textContent = help[action]; } else { const allOptions = `Stage: ${help.stage} • Unstage: ${help.unstage} • Restore from last commit: ${help.head} • Restore from server: ${help.remote}`; $('#recoveryHelp').textContent = allOptions; } }

// A real line-level diff (LCS-based), not a naive index-by-index compare.
// Index-by-index compare misaligns everything after a single inserted or
// deleted line — every line below it then looks "different" even when only
// one line actually changed, and can even mask a real difference if two
// unrelated lines happen to line up at the same index. This aligns matching
// lines wherever they fall on either side and leaves a blank filler row on
// the other side for a pure insertion/deletion, so only the lines that
// actually changed are ever highlighted.
function lcsAlign(a, b) {
  const n = a.length, m = b.length;
  if (n * m > 2_000_000) return null; // too large for the O(n*m) table — caller falls back
  const dp = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  let i = 0, j = 0; const left = []; const right = [];
  while (i < n && j < m) {
    if (a[i] === b[j]) { left.push({ text: a[i], same: true }); right.push({ text: b[j], same: true }); i++; j++; }
    else if (dp[i + 1][j] >= dp[i][j + 1]) { left.push({ text: a[i], same: false }); right.push({ text: null }); i++; }
    else { left.push({ text: null }); right.push({ text: b[j], same: false }); j++; }
  }
  while (i < n) { left.push({ text: a[i], same: false }); right.push({ text: null }); i++; }
  while (j < m) { left.push({ text: null }); right.push({ text: b[j], same: false }); j++; }
  return { left, right };
}

function renderComparisonContents(localText, remoteText) {
  const localLines = localText.split('\n'); const remoteLines = remoteText.split('\n');
  const aligned = lcsAlign(localLines, remoteLines);
  if (!aligned) {
    // Fallback for very large files: the old, simpler index compare — still
    // correct for files where nothing was inserted/deleted mid-file, just
    // not alignment-aware.
    const count = Math.max(localLines.length, remoteLines.length);
    const renderSide = (lines, other, remoteSide) => Array.from({ length: count }, (_, index) => {
      const line = lines[index] ?? ''; const different = line !== (other[index] ?? '');
      return `<span class="${different ? `diff-line${remoteSide ? ' remote-line' : ''}` : 'same-line'}"><i class="line-number">${index + 1}</i>${esc(line) || ' '}</span>`;
    }).join('');
    refs.localCompare.innerHTML = renderSide(localLines, remoteLines, false); refs.remoteCompare.innerHTML = renderSide(remoteLines, localLines, true);
    return;
  }
  const renderSide = (rows, remoteSide) => { let lineNumber = 0; return rows.map(row => {
    if (row.text === null) return `<span class="filler-line"><i class="line-number"></i></span>`;
    lineNumber++;
    const cls = row.same ? 'same-line' : `diff-line${remoteSide ? ' remote-line' : ''}`;
    return `<span class="${cls}"><i class="line-number">${lineNumber}</i>${esc(row.text) || ' '}</span>`;
  }).join(''); };
  refs.localCompare.innerHTML = renderSide(aligned.left, false); refs.remoteCompare.innerHTML = renderSide(aligned.right, true);
}

function formatSize(bytes) {
  if (!bytes) return '—'; if (bytes < 1024) return `${bytes} B`; if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`; return `${(bytes / 1048576).toFixed(1)} MB`;
}

function formatModified(seconds) {
  if (!seconds) return '—'; return new Intl.DateTimeFormat(undefined, { month: 'short', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(new Date(seconds * 1000));
}

function iconFor(entry) {
  if (entry.kind === 'folder') return '<span class="entry-icon folder">▰</span>';
  if (entry.kind === 'submodule') return '<span class="entry-icon submodule">◇</span>';
  if (entry.kind === 'symlink') return '<span class="entry-icon symlink">↗</span>';
  return `<span class="entry-icon file">${esc((entry.name.split('.').pop() || '').slice(0, 3).toUpperCase())}</span>`;
}

function gitState(entry) {
  const labels = { M: 'Modified locally', A: 'Added locally', D: 'Deleted locally', R: 'Renamed locally', '??': 'New, untracked', '•': 'Modified files inside' };
  if (!entry.tracked) return '<span class="git-state untracked"><i class="git-dot"></i>New, untracked</span>';
  if (entry.kind === 'submodule' && entry.status === 'M' && entry.submodule_has_unpushed_commits) {
    return '<span class="git-state new-version"><i class="git-dot"></i>New version</span>';
  }
  if (entry.status) return `<span class="git-state changed"><i class="git-dot"></i>${esc(labels[entry.status] || entry.status)}</span>`;
  // Fully committed (no working-tree status at all) but that commit hasn't
  // reached the branch's upstream yet — a real, distinct state from both
  // "clean" and "modified": nothing here needs a commit, it needs a push.
  if (entry.unpushed) return `<span class="git-state unpushed"><i class="git-dot"></i>${entry.kind === 'folder' ? 'Contains unpushed commits' : 'Not pushed yet'}</span>`;
  return '<span class="git-state"><i class="git-dot"></i>Tracked</span>';
}

function renderBreadcrumbs() {
  const parts = state.currentPath ? state.currentPath.split('/') : [];
  let accumulated = '';
  refs.breadcrumbs.innerHTML = `<button class="crumb root" data-path="">▰ ${esc(state.repository?.name || 'Repository')}</button>` + parts.map(part => {
    accumulated = accumulated ? `${accumulated}/${part}` : part;
    return `<span class="crumb-separator">›</span><button class="crumb" data-path="${esc(accumulated)}">${esc(part)}</button>`;
  }).join('');
  refs.breadcrumbs.querySelectorAll('[data-path]').forEach(crumb => crumb.addEventListener('click', () => openDirectory(crumb.dataset.path)));
}

let explorerRenderState = { path: null, query: null, entries: null };
let lastExplorerClick = { path: null, time: 0 };
function renderExplorer() {
  if (!state.repository) return;
  renderBreadcrumbs();
  const query = refs.search.value.trim().toLowerCase();
  // If nothing but the selection changed (a plain click, no folder reload and
  // no new search), update just the "selected" class in place instead of
  // rebuilding every row's DOM node. Rebuilding on every click replaces the
  // very button the user just clicked — on Windows/WebView2 that resets the
  // browser's double-click sequence (it requires both clicks to land on the
  // same element), so a folder needed two double-clicks to open. macOS's
  // WebKit is more lenient about this, which is why it only showed up there.
  if (explorerRenderState.path === state.currentPath && explorerRenderState.query === query && explorerRenderState.entries === state.entries) {
    refs.fileList.querySelectorAll('[data-entry]').forEach(row => row.classList.toggle('selected', state.selectedEntry?.relative_path === row.dataset.entry));
    return;
  }
  explorerRenderState = { path: state.currentPath, query, entries: state.entries };
  const entries = state.entries.filter(entry => !query || entry.name.toLowerCase().includes(query));
  const upRow = state.currentPath ? `<button class="file-row file-grid up-row" data-go-up="1">
    <span class="file-main"><span class="entry-icon folder">▲</span><span class="entry-copy"><span class="entry-name">..</span><span class="entry-hint">Parent folder</span></span></span>
    <span></span><span></span><span></span>
  </button>` : '';
  refs.fileList.innerHTML = upRow + entries.map(entry => `<button class="file-row file-grid ${entry.status || !entry.tracked ? 'has-change' : ''} ${state.selectedEntry?.relative_path === entry.relative_path ? 'selected' : ''}" data-entry="${esc(entry.relative_path)}">
    <span class="file-main">${iconFor(entry)}<span class="entry-copy"><span class="entry-name">${esc(entry.name)}${entry.kind === 'submodule' ? '<b class="inline-submodule-badge">SUBMODULE</b>' : ''}</span><span class="entry-hint">${entry.kind === 'submodule' ? 'Independent Git repository' : entry.kind}</span></span>${['folder','submodule'].includes(entry.kind) ? '<span class="folder-arrow">›</span>' : ''}</span>
    ${gitState(entry)}<span class="file-size">${entry.kind === 'file' ? formatSize(entry.size) : '—'}</span><span class="file-modified">${formatModified(entry.modified)}</span>
  </button>`).join('') || (state.currentPath ? '' : '<div class="empty-change">This folder is empty</div>');
  refs.fileList.querySelector('[data-go-up]')?.addEventListener('click', () => {
    const now = Date.now();
    const isDoubleClick = lastExplorerClick.path === '..' && now - lastExplorerClick.time < 500;
    lastExplorerClick = { path: '..', time: now };
    if (isDoubleClick) { lastExplorerClick = { path: null, time: 0 }; openDirectory(state.currentPath.split('/').slice(0, -1).join('/')); }
  });
  refs.fileList.querySelectorAll('[data-entry]').forEach(row => {
    // Single click selects, double click opens a folder/submodule — the
    // familiar file-explorer convention. The browser's native `dblclick`
    // event requires both clicks to land on the very same DOM element within
    // its own timing window; since a click here re-renders (replacing rows
    // in the general case, and always doing real async work), Chromium/
    // WebView2 was unreliable about recognizing the second click as part of
    // the same double-click on Windows — macOS's WebKit is more lenient.
    // Detecting the double-click ourselves — by comparing the clicked path
    // and a timestamp, not the DOM node — sidesteps that entirely.
    row.addEventListener('click', () => {
      const now = Date.now();
      const isDoubleClick = lastExplorerClick.path === row.dataset.entry && now - lastExplorerClick.time < 500;
      lastExplorerClick = { path: row.dataset.entry, time: now };
      const entry = state.entries.find(item => item.relative_path === row.dataset.entry);
      if (isDoubleClick && entry && ['folder', 'submodule'].includes(entry.kind)) { lastExplorerClick = { path: null, time: 0 }; openDirectory(entry.relative_path); return; }
      selectEntry(row.dataset.entry);
    });
    row.addEventListener('contextmenu', event => {
      const entry = state.entries.find(item => item.relative_path === row.dataset.entry);
      if (entry?.kind !== 'submodule') return;
      event.preventDefault(); selectEntry(entry.relative_path); openSubmoduleMenu(entry, event.clientX, event.clientY);
    });
  });
}

async function openDirectory(path, options = {}) {
  if (!state.repository) return;
  state.currentPath = path; state.selectedEntry = null;
  if (!options.force && directoryCache.has(path)) { state.entries = directoryCache.get(path); render(); return; }
  refs.fileList.innerHTML = '<div class="loading-row"><i class="spinner"></i>Loading folder…</div>';
  if (!invoke) { state.entries = previewData.entries; directoryCache.set(path, state.entries); render(); return; }
  try { state.entries = await invoke('load_directory', { repositoryPath: state.repository.path, relativePath: path }); directoryCache.set(path, state.entries); render(); }
  catch (error) { status(String(error), 'error'); refs.fileList.innerHTML = `<div class="empty-change">${esc(String(error))}</div>`; }
}

async function openSubmoduleMenu(entry, x, y) {
  refs.submoduleMenu.hidden = false;
  refs.submoduleMenu.style.left = `${Math.min(x, innerWidth - 460)}px`;
  refs.submoduleMenu.style.top = `${Math.min(y, innerHeight - 590)}px`;
  refs.submoduleMenuName.textContent = entry.name; refs.currentSubmoduleVersion.textContent = 'Loading…';
  refs.submoduleVersions.innerHTML = '<div class="version-loading"><i class="spinner"></i>Reading branches and commits…</div>';
  if (!invoke) {
    submoduleMenuData = { path: entry.relative_path, current_revision: 'a39f21d81ce0', current_branch: 'main', versions: [
      { name: 'main', revision: 'a39f21d81ce0', kind: 'branch', current: true, subject: 'Stable diagnostics API', author: 'Andrei Pop', date: '2026-08-14' },
      { name: 'release/2.4', revision: 'bd51e40ca112', kind: 'branch', current: false, subject: 'Release configuration', author: 'Maria Ionescu', date: '2026-08-12' },
      { name: 'origin/feature/events', revision: 'de91822aef33', kind: 'remote', current: false, subject: 'Add event mapping', author: 'Victor Ene', date: '2026-08-11' },
      { name: 'a39f21d', revision: 'a39f21d81ce0', kind: 'commit', current: true, subject: 'Stable diagnostics API', author: 'Andrei Pop', date: '2026-08-14' },
      { name: 'bd51e40', revision: 'bd51e40ca112', kind: 'commit', current: false, subject: 'Release configuration', author: 'Maria Ionescu', date: '2026-08-12' }
    ] }; renderSubmoduleVersions(); return;
  }
  try { submoduleMenuData = await invoke('submodule_versions', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); renderSubmoduleVersions(); }
  catch (error) { refs.submoduleVersions.innerHTML = `<div class="version-loading">${esc(String(error))}</div>`; }
}

function renderSubmoduleVersions() {
  if (!submoduleMenuData) return;
  refs.currentSubmoduleVersion.textContent = `${submoduleMenuData.current_branch || 'detached'} · ${submoduleMenuData.current_revision.slice(0, 8)}`;
  const versions = submoduleMenuData.versions.filter(item => versionFilter === 'branch' ? ['branch','remote'].includes(item.kind) : item.kind === 'commit');
  const kindLabel = { branch: 'BRANCH', remote: 'REMOTE BRANCH', commit: 'COMMIT (detached)' };
  refs.submoduleVersions.innerHTML = versions.map(item => `<button class="version-row ${item.current ? 'current' : ''}" data-revision="${esc(item.revision)}" data-version-kind="${esc(item.kind)}" data-name="${esc(item.name)}">
    <span class="version-symbol">${item.kind === 'commit' ? '●' : '⑂'}</span><span class="version-name">${esc(item.name)}<b class="version-kind-badge">${esc(kindLabel[item.kind] || item.kind)}</b></span><span class="version-copy"><span class="version-subject">${esc(item.subject)}</span><span class="version-meta">${esc(item.author)} · ${esc(item.date)}</span></span>${item.current ? '<span class="current-label">CURRENT</span>' : ''}</button>`).join('') || '<div class="version-loading">No versions found</div>';
  refs.submoduleVersions.querySelectorAll('[data-revision]').forEach(row => row.addEventListener('click', () => switchSubmoduleVersion(row.dataset.revision, row.dataset.versionKind, row.dataset.name)));
}

async function switchSubmoduleVersion(revision, kind, name) {
  if (!invoke) { refs.currentSubmoduleVersion.textContent = `preview · ${revision.slice(0, 8)}`; return; }
  try {
    status('Switching submodule version…', 'busy');
    await invoke('switch_submodule_version', { repositoryPath: state.repository.path, relativePath: submoduleMenuData.path, revision, versionKind: kind, name: name || '' });
    const folder = state.currentPath; const data = await invoke('load_repository', { path: state.repository.path, force: false });
    Object.assign(state, data); state.view = 'explorer'; directoryCache.clear(); refs.submoduleMenu.hidden = true; await openDirectory(folder, { force: true });
    const target = kind === 'branch' ? `branch "${name}"` : kind === 'remote' ? `remote branch "${name}" (detached at that commit)` : `commit ${revision.slice(0, 8)} (detached — not on any branch)`;
    const successMsg = `Submodule switched to ${target}. It now shows as "Modified" here — that's expected: the project hasn't recorded the new pointer yet. Select the submodule and use "Commit this item" to save it.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  } catch (error) { const message = handleError(error); showOperationToast(`Could not switch version: ${message}`, 'error'); }
}

async function selectEntry(path) {
  state.selectedEntry = state.entries.find(entry => entry.relative_path === path);
  if (state.selectedEntry?.kind === 'file' && refs.editorDialog.open) {
    openEditor(state.selectedEntry);
  }
  render();
  if (!invoke) return renderEntryDetails({ ...state.selectedEntry, item_count: state.selectedEntry.kind === 'folder' ? 12 : null, submodule_url: state.selectedEntry.kind === 'submodule' ? 'git@example.com:platform/diagnostics-core.git' : null, submodule_branch: state.selectedEntry.kind === 'submodule' ? 'main' : null, last_commit_id: 'a39f21d', last_commit_subject: 'P:423421431 test', last_commit_author: 'Andrei Pop', last_commit_date: '2026-08-14' });
  try { renderEntryDetails(await invoke('entry_details', { repositoryPath: state.repository.path, relativePath: path })); }
  catch (error) { handleError(error); }
}

function selectedScope() {
  return { path: state.selectedEntry?.relative_path || state.currentPath || '', name: state.selectedEntry?.name || (state.currentPath ? state.currentPath.split('/').pop() : state.repository?.name || 'repository') };
}

function scopeHasChanges(scope) {
  if (state.changes.some(change => !scope.path || change.path === scope.path || change.path.startsWith(`${scope.path}/`))) return true;
  // A file inside a submodule never shows up in state.changes — the parent's
  // own status scan can't see inside a submodule's own index at all, only the
  // submodule as one opaque entry. The currently selected entry's own status
  // (sourced correctly from the submodule's own repo when it's inside one) is
  // already accurate, so fall back to trusting that directly.
  const entry = state.selectedEntry;
  if (entry && entry.relative_path === scope.path) return Boolean(entry.status) || !entry.tracked;
  return false;
}

function openScopeCommit() {
  const scope = selectedScope();
  if (!scopeHasChanges(scope)) { const msg = `Nothing to commit — "${scope.name}" has no uncommitted local changes.`; status(msg); showOperationToast(msg, 'error'); return; }
  refs.commitScopeName.textContent = scope.name; refs.scopeCommitMessage.value = refs.defaultCommitMessage.value.trim(); refs.confirmScopeCommit.disabled = !refs.scopeCommitMessage.value.trim(); refs.commitScopeDialog.showModal(); refs.scopeCommitMessage.focus();
}

async function commitSelectedScope(event) {
  event.preventDefault(); const scope = selectedScope(); const message = refs.scopeCommitMessage.value.trim(); if (!message) return;
  if (!invoke) { refs.commitScopeDialog.close(); status(`Preview: committed ${scope.name}`); return; }
  if (!scopeHasChanges(scope)) { const msg = `Nothing to commit — "${scope.name}" has no uncommitted local changes.`; refs.commitScopeDialog.close(); status(msg); showOperationToast(msg, 'error'); return; }
  refs.confirmScopeCommit.disabled = true; refs.confirmScopeCommit.textContent = 'Committing…';
  try {
    await invoke('commit_path', { repositoryPath: state.repository.path, relativePath: scope.path, message });
    const folder = state.currentPath; refs.commitScopeDialog.close(); const data = await invoke('load_repository', { path: state.repository.path, force: false });
    Object.assign(state, data); state.allCommits = data.commits; state.selectedEntry = null; directoryCache.clear(); await openDirectory(folder, { force: true });
    const successMsg = `Committed "${scope.name}". Push when you're ready to send it to the server.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  } catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
  finally { refs.confirmScopeCommit.textContent = 'Commit selection'; refs.confirmScopeCommit.disabled = !refs.scopeCommitMessage.value.trim(); }
}

async function showSelectedHistory() {
  const scope = selectedScope();
  if (!invoke) { state.historyScope = scope.name; state.view = 'graph'; render(); return; }
  try { status(`Loading history for ${scope.name}…`, 'busy'); state.commits = await invoke('path_history', { repositoryPath: state.repository.path, relativePath: scope.path }); state.historyScope = scope.name; state.view = 'graph'; refs.search.value = ''; render(); status(`${state.commits.length} commits for ${scope.name}`); }
  catch (error) { handleError(error); }
}

function renderEntryDetails(entry) {
  const kindLabel = entry.kind === 'submodule' ? 'Git submodule' : entry.kind.charAt(0).toUpperCase() + entry.kind.slice(1);
  refs.details.innerHTML = `<div class="entry-details"><div class="entry-preview ${esc(entry.kind)}">${entry.kind === 'submodule' ? '◇' : entry.kind === 'folder' ? '▰' : '▤'}</div>
    <h2>${esc(entry.name)}</h2><div class="entry-path">${esc(entry.relative_path)}</div>${entry.kind === 'submodule' ? '<span class="submodule-badge">◇ Git submodule</span>' : ''}
    ${entry.status || !entry.tracked ? `<div class="local-change-banner"><i></i><div><strong>${entry.kind === 'submodule' && entry.submodule_push_status ? 'New version locally (not pushed yet)' : entry.tracked ? 'Modified locally' : 'New local file'}</strong><span>${entry.kind === 'submodule' && entry.submodule_push_status ? 'Committed here, on this machine — not sent to the submodule\'s server yet.' : 'This item differs from the committed repository state.'}</span></div></div>` : ''}
    <div class="context-actions">${entry.kind === 'file' ? '<button data-detail-action="edit">Edit local file</button>' : '<button data-detail-action="open">Open folder</button>'}<button data-detail-action="server">Open on server ↗</button><button data-detail-action="history">View history</button>${entry.status ? '<button data-detail-action="commit">Commit this item</button>' : ''}${entry.kind === 'file' && (entry.status || !entry.tracked) ? `<button data-detail-action="stage" data-tooltip="git add — add this file's current content to staging">＋ Stage this file</button><button data-detail-action="unstage" data-tooltip="Unstage — git restore --staged. Removes only the staging entry; your edits on disk are kept exactly as they are.">− Unstage</button><button data-detail-action="stashfile" data-tooltip="Sets this file aside in a temporary holding area (the stash) — it's left out of any commit, and out of your working folder, until you bring it back with Pop stash">⇕ Stash this file</button><button data-detail-action="head" class="danger-action-soft" data-tooltip="Restore from your last local commit (HEAD) — git checkout HEAD -- file. Permanently discards ALL edits; the file on disk becomes identical to what you last committed. Cannot be undone.">↶ Restore from last commit (HEAD)</button><button data-detail-action="compare" data-tooltip="Open side-by-side compare with restore options">⇄ Compare with remote</button>` : ''}${entry.kind === 'submodule' ? `<button data-detail-action="subserver">Open submodule repository ↗</button><button data-detail-action="subgraph">Submodule branch map</button><button data-detail-action="subnewbranch" data-tooltip="Create a new local branch in this submodule, starting from its current commit, and switch to it">＋ New branch…</button><button data-detail-action="versions">Change version</button><button data-detail-action="subcommit" ${entry.status ? '' : 'disabled'} data-tooltip="${entry.status ? 'Commit uncommitted changes inside the submodule' : 'Nothing to commit — no uncommitted changes inside this submodule'}">Commit submodule</button><button data-detail-action="subpull" data-tooltip="Fast-forward pull — brings in new commits from the submodule's remote. Refuses if it would require a manual merge.">Pull submodule</button><button data-detail-action="submerge" data-tooltip="Merge a branch into this submodule's current branch, with conflict resolution if needed">Merge branch…</button><button data-detail-action="subpush">Push submodule</button><button data-detail-action="subforcepush" class="danger-action-soft" data-tooltip="⚠️ Overwrites the remote branch with your local history, discarding any commits there aren't in yours. Only safe if nobody else uses that remote.">Force push submodule…</button><button data-detail-action="subfetch">Fetch submodule</button><button data-detail-action="location">Replace repository URL</button>` : ''}<button class="danger-action" data-detail-action="delete">Delete…</button></div>
    <div class="detail-section"><h3>GENERAL</h3><div class="detail-grid"><span>Type</span><strong>${kindLabel}</strong><span>Git</span><strong>${entry.tracked ? (entry.status || (entry.unpushed ? (entry.kind === 'folder' ? 'Clean — contains unpushed commits' : 'Committed, not pushed yet') : 'Tracked, clean')) : 'Untracked'}</strong>
    ${entry.item_count != null ? `<span>Items</span><strong>${entry.item_count}</strong>` : `<span>Size</span><strong>${formatSize(entry.size)}</strong>`}<span>Modified</span><strong>${formatModified(entry.modified)}</strong></div></div>
    ${entry.kind === 'submodule' ? `<div class="detail-section"><h3>SUBMODULE</h3><div class="detail-grid"><span>Remote</span><strong>${esc(entry.submodule_url || 'Not configured')}</strong><span>Branch</span><strong>${esc(entry.submodule_branch || 'Default')}</strong><span>Status</span><strong>${entry.status ? (entry.status === 'M' ? 'Has local changes' : 'Modified') : 'Clean'}</strong></div>
    ${entry.submodule_unpushed_commits?.length ? `<div class="submodule-push-banner"><i></i><span>${entry.submodule_unpushed_commits.length} commit${entry.submodule_unpushed_commits.length === 1 ? '' : 's'} not yet pushed to its own remote:</span></div><div class="submodule-unpushed-list">${entry.submodule_unpushed_commits.map(commit => `<div class="submodule-unpushed-commit"><strong>${commitSubjectHtml(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author)} · ${esc(commit.date)}</small></div>`).join('')}</div>` : entry.submodule_push_status ? `<div class="submodule-push-banner"><i></i><span>${esc(entry.submodule_push_status)}</span></div>` : ''}</div>` : ''}
    ${entry.kind === 'submodule' ? `<div class="detail-section"><h3>SUBMODULE COMMIT (actual change)</h3><div class="detail-grid"><span>Commit</span><strong>${entry.submodule_commit_id ? `<a href="#" class="commit-server-link" data-commit-id="${esc(entry.submodule_commit_id)}" data-submodule-path="${esc(entry.relative_path)}" title="Open this commit on the submodule's own server">${esc(entry.submodule_commit_id.slice(0, 8))} ↗</a>` : 'No commit'}</strong><span>Message</span><strong>${commitSubjectHtml(entry.submodule_commit_subject || '—')}</strong><span>Author</span><strong>${esc(entry.submodule_commit_author || '—')}</strong><span>Date</span><strong>${esc(entry.submodule_commit_date || '—')}</strong></div></div>` : ''}
    <div class="detail-section"><h3>${entry.kind === 'submodule' ? 'PROJECT COMMIT (gitlink update)' : 'LAST COMMIT'}</h3><div class="detail-grid"><span>Commit</span><strong>${entry.last_commit_id ? `<a href="#" class="commit-server-link" data-commit-id="${esc(entry.last_commit_id)}" title="Open this commit on the server">${esc(entry.last_commit_id.slice(0, 8))} ↗</a>` : 'No commit'}</strong><span>Message</span><strong>${commitSubjectHtml(entry.last_commit_subject || '—')}</strong><span>Author</span><strong>${esc(entry.last_commit_author || '—')}</strong><span>Date</span><strong>${esc(entry.last_commit_date || '—')}</strong></div></div></div>`;
  refs.details.querySelectorAll('[data-detail-action]').forEach(button => button.addEventListener('click', () => { Promise.resolve(handleDetailAction(button.dataset.detailAction, entry, button)).catch(error => handleError(error)); }));
}

async function handleDetailAction(action, entry, button) {
  if (action === 'edit') return openEditor(entry);
  if (action === 'open') return openDirectory(entry.relative_path);
  if (action === 'history') return showSelectedHistory();
  if (action === 'commit') return openScopeCommit();
  if (action === 'versions') return await openSubmoduleMenu(entry, innerWidth - 480, 110);
  if (action === 'subgraph') return openSubmoduleGraph(entry);
  if (action === 'server') return openEntryOnServer(entry, false);
  if (action === 'subserver') return openEntryOnServer(entry, true);
  if (action === 'location') return replaceSubmoduleLocation(entry);
  if (action === 'delete') return deleteEntry(entry, button);
  if (action === 'compare') return compareEntryWithRemote(entry);
  if (action === 'subcommit') return commitSubmoduleChanges(entry);
  if (action === 'subpull') return pullSubmodule(entry);
  if (action === 'submerge') return openMergeBranchDialog(mergeTargetForSubmodule(entry));
  if (action === 'subpush') return pushSubmodule(entry);
  if (action === 'subforcepush') return forcePushSubmodule(entry);
  if (action === 'subfetch') return fetchSubmodule(entry);
  if (action === 'subnewbranch') return createSubmoduleBranch(entry);
  if (['head','stage','unstage'].includes(action)) return runEntryFileAction(entry, action);
  if (action === 'stashfile') return stashOneFile(entry.relative_path);
}

async function createSubmoduleBranch(entry) {
  if (entry.kind !== 'submodule') return;
  const name = await customPrompt(`New branch name for submodule ${entry.name} (created from its current commit):`, '', { title: 'New submodule branch' });
  if (!name) return;
  if (!invoke) return status(`Preview: created branch "${name}" in ${entry.name}`);
  try {
    status(`Creating branch "${name}" in ${entry.name}…`, 'busy');
    await invoke('create_submodule_branch', { repositoryPath: state.repository.path, relativePath: entry.relative_path, branch: name });
    directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true });
    if (state.selectedEntry?.relative_path === entry.relative_path) await selectEntry(entry.relative_path);
    const msg = `${entry.name}: branch "${name}" created and checked out.`;
    status(msg); showOperationToast(msg, 'success');
  } catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

async function openEntryOnServer(entry, submodule) {
  if (!invoke) return status(`Preview: open ${entry.name} on server`);
  try {
    if (submodule) {
      const data = await invoke('submodule_repository', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
      await invoke('open_repository_item', { repositoryPath: data.repository.path, relativePath: '', kind: 'folder' });
    } else await invoke('open_repository_item', { repositoryPath: state.repository.path, relativePath: entry.relative_path, kind: entry.kind });
  } catch (error) { handleError(error); }
}

async function replaceSubmoduleLocation(entry) {
  const url = await customPrompt(`New Git repository URL for submodule ${entry.name}:`, entry.submodule_url || '', { title: 'Replace repository URL' });
  if (!url || url.trim() === (entry.submodule_url || '')) return;
  if (!await customConfirm(`Replace the source repository for ${entry.relative_path}?\n\nThe .gitmodules URL and local origin will change, then the new origin will be fetched.`, { title: 'Replace repository URL', danger: true, okLabel: 'Replace' })) return;
  if (!invoke) return status(`Preview: change submodule URL to ${url.trim()}`);
  try { status('Changing submodule repository and fetching refs…', 'busy'); await invoke('change_submodule_url', { repositoryPath: state.repository.path, relativePath: entry.relative_path, url: url.trim() }); const folder = state.currentPath; directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(folder, { force: true }); status(`${entry.name}: repository location updated`); }
  catch (error) { handleError(error); }
}

async function commitSubmoduleChanges(entry) {
  if (entry.kind !== 'submodule') return;
  const message = await customPrompt(`Commit message for changes in ${entry.name}:`, '', { title: 'Commit submodule' });
  if (!message?.trim()) return;
  if (!invoke) return status(`Preview: committed changes in ${entry.name}`);
  try {
    status(`Committing changes in ${entry.name}…`, 'busy');
    await invoke('commit_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path, message: message.trim() });
    directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true });
    const successMsg = `${entry.name}: committed inside the submodule. The project's link to it was updated automatically to this new commit — commit/push the project when you're ready to share that.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message2 = handleError(error); showOperationToast(`Commit failed: ${message2}`, 'error'); }
}

async function pullSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  if (!invoke) return status(`Preview: pulled ${entry.name}`);
  try {
    status(`Pulling ${entry.name} from its remote…`, 'busy');
    await invoke('pull_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true });
    if (state.selectedEntry?.relative_path === entry.relative_path) await selectEntry(entry.relative_path);
    const successMsg = `${entry.name}: pulled the latest commits from its remote (fast-forward).`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) {
    const message = handleError(error);
    if (String(error).toLowerCase().includes('diverged')) {
      showOperationToast(`${message}\nTry "Merge branch…" instead — it can resolve the conflicts here.`, 'error');
    } else { showOperationToast(message, 'error'); }
  }
}

// ---- Merge & conflict resolution -----------------------------------------
// Works identically for the main repository and for a submodule — a submodule
// is just another repository at a different path, addressed the same way the
// rest of the app already does: (parent repositoryPath, submodule targetPath).
function mergeTargetForMain() { return { targetPath: '', label: state.repository.current_branch || 'current branch', isSubmodule: false }; }
function mergeTargetForSubmodule(entry) { return { targetPath: entry.relative_path, label: entry.name, isSubmodule: true }; }

async function openMergeBranchDialog(target) {
  state.mergeTarget = target;
  refs.mergeBranchCurrent.value = target.label;
  refs.mergeBranchSubtitle.textContent = target.isSubmodule ? `Merge a branch into "${target.label}" (submodule).` : "Bring another branch's commits into your current branch.";
  refs.mergeBranchStatus.textContent = '';
  refs.confirmMergeBranch.disabled = false; refs.confirmMergeBranch.textContent = 'Merge';
  if (!invoke) { refs.mergeBranchSource.innerHTML = '<option value="main">main</option>'; refs.mergeBranchDialog.showModal(); return; }
  try {
    const branches = target.isSubmodule
      ? (await invoke('submodule_repository', { repositoryPath: state.repository.path, relativePath: target.targetPath })).branches
      : state.branches;
    const options = branches.filter(branch => !branch.current);
    refs.mergeBranchSource.innerHTML = options.map(branch => `<option value="${esc(branch.name)}">${esc(branch.name)}${branch.remote ? ' (remote)' : ''}</option>`).join('') || '<option value="" disabled>No other branches</option>';
    refs.mergeBranchDialog.showModal();
  } catch (error) { handleError(error); }
}

async function refreshAfterMerge() {
  directoryCache.clear();
  await loadRepository(state.repository.path, { keepPath: true });
  await checkForMergeConflicts();
}

refs.confirmMergeBranch.addEventListener('click', async () => {
  const sourceRef = refs.mergeBranchSource.value; if (!sourceRef) return;
  const target = state.mergeTarget;
  if (!invoke) { refs.mergeBranchDialog.close(); status(`Preview: merged ${sourceRef}`); return; }
  refs.confirmMergeBranch.disabled = true; refs.confirmMergeBranch.textContent = 'Merging…';
  try {
    const outcome = await invoke('merge_branch', { repositoryPath: state.repository.path, targetPath: target.targetPath, sourceRef });
    if (outcome.status === 'conflicts') {
      refs.mergeBranchDialog.close();
      await refreshAfterMerge();
      openConflictsDialog(target, outcome.conflicts, outcome.message);
    } else {
      refs.mergeBranchDialog.close();
      status(outcome.message); showOperationToast(outcome.message, 'success');
      await refreshAfterMerge();
    }
  } catch (error) { refs.mergeBranchStatus.textContent = String(error); handleError(error); }
  finally { refs.confirmMergeBranch.disabled = false; refs.confirmMergeBranch.textContent = 'Merge'; }
});

function renderConflictsList(target, conflicts) {
  refs.conflictsList.innerHTML = conflicts.map(conflict => `<div class="conflict-row" data-path="${esc(conflict.path)}">
    <div class="conflict-head"><span class="conflict-path">${esc(conflict.path)}</span><span class="conflict-state pending">PENDING</span></div>
    <div class="conflict-actions">
      <button data-resolve="ours" ${conflict.has_ours ? '' : 'disabled'} data-tooltip="Keep your version of this file">↤ Keep mine</button>
      <button data-resolve="theirs" ${conflict.has_theirs ? '' : 'disabled'} data-tooltip="Keep the incoming branch's version of this file">↦ Keep theirs</button>
      <button data-resolve="manual" data-tooltip="Open the file (with conflict markers) and edit it yourself">✎ Edit manually</button>
    </div>
  </div>`).join('') || '<div class="empty-change">No conflicts remaining</div>';
  refs.conflictsList.querySelectorAll('[data-resolve]').forEach(button => button.addEventListener('click', () => {
    const path = button.closest('.conflict-row').dataset.path; const kind = button.dataset.resolve;
    if (kind === 'manual') editConflictFile(target, path); else resolveConflictAction(target, path, kind);
  }));
}

function openConflictsDialog(target, conflicts, introMessage) {
  state.mergeTarget = target;
  const isStash = target.kind === 'stash';
  refs.conflictsTitle.textContent = isStash ? 'Stash conflicts' : 'Merge conflicts';
  refs.conflictsSubtitle.textContent = introMessage || (isStash
    ? `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} need resolution before the stashed changes are fully applied.`
    : `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} need resolution before the merge can be completed.`);
  // A stash pop needs no merge commit — the resolved content just becomes
  // your regular working-tree changes, ready for your next normal commit —
  // so there's nothing to type a message for here.
  refs.conflictsCommitMessageLabel.hidden = isStash;
  refs.conflictsLocalNote.hidden = isStash;
  refs.conflictsCommitMessage.value = target.isSubmodule ? `Merge into ${target.label}` : `Merge into ${state.repository.current_branch}`;
  refs.confirmCompleteMerge.textContent = isStash ? 'Done' : 'Complete merge';
  refs.abortMergeButton.textContent = isStash ? 'Discard and keep the stash' : 'Abort merge';
  refs.conflictsStatus.textContent = '';
  renderConflictsList(target, conflicts);
  refs.conflictsDialog.showModal();
}

async function refreshConflictsDialog(target) {
  try {
    const conflicts = await invoke('list_conflicts', { repositoryPath: state.repository.path, targetPath: target.targetPath });
    renderConflictsList(target, conflicts);
    const isStash = target.kind === 'stash';
    refs.conflictsSubtitle.textContent = conflicts.length
      ? `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} still need resolution.`
      : (isStash ? 'All conflicts resolved — the stashed changes are now in your working tree.' : 'All conflicts resolved — ready to complete the merge.');
    await checkForMergeConflicts();
  } catch (error) { handleError(error); }
}

async function resolveConflictAction(target, path, kind) {
  try {
    status(`Resolving ${path}…`, 'busy');
    await invoke('resolve_conflict', { repositoryPath: state.repository.path, targetPath: target.targetPath, relativePath: path, resolution: kind });
    const msg = `${path}: kept ${kind === 'ours' ? 'your' : 'the incoming'} version.`;
    status(msg); showOperationToast(msg, 'success');
    await refreshConflictsDialog(target);
  } catch (error) { handleError(error); }
}

function editConflictFile(target, path) {
  const joined = target.targetPath ? `${target.targetPath}/${path}` : path;
  state.editingConflict = { target, path };
  refs.editorTitle.textContent = `Resolve ${path}`;
  refs.editorPath.textContent = path;
  refs.editorContent.value = 'Loading…';
  refs.editorDialog.showModal();
  if (!invoke) { refs.editorContent.value = '<<<<<<< HEAD\n(your version)\n=======\n(their version)\n>>>>>>> branch\n'; return; }
  invoke('read_text_file', { repositoryPath: state.repository.path, relativePath: joined })
    .then(file => { refs.editorContent.value = file.content; refs.editorContent.focus(); })
    .catch(error => { refs.editorDialog.close(); handleError(error); });
}

refs.confirmCompleteMerge.addEventListener('click', async () => {
  const target = state.mergeTarget;
  if (target.kind === 'stash') {
    // No merge commit to make here — just confirm nothing is still
    // conflicted before letting the user walk away with it.
    try {
      const conflicts = await invoke('list_conflicts', { repositoryPath: state.repository.path, targetPath: target.targetPath });
      if (conflicts.length) { refs.conflictsStatus.textContent = `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} still need resolution first.`; return; }
      refs.conflictsDialog.close();
      const msg = 'Stash conflicts resolved — the changes are in your working tree, ready to commit.';
      status(msg); showOperationToast(msg, 'success');
      await refreshAfterMerge();
    } catch (error) { refs.conflictsStatus.textContent = String(error); }
    return;
  }
  const message = refs.conflictsCommitMessage.value.trim();
  if (!message) { refs.conflictsStatus.textContent = 'A merge commit message is required.'; return; }
  refs.confirmCompleteMerge.disabled = true; refs.confirmCompleteMerge.textContent = 'Completing…';
  try {
    await invoke('complete_merge', { repositoryPath: state.repository.path, targetPath: target.targetPath, message });
    refs.conflictsDialog.close();
    const msg = `Merge completed${target.isSubmodule ? ` in ${target.label}` : ''}.`;
    status(msg); showOperationToast(msg, 'success');
    await refreshAfterMerge();
  } catch (error) { refs.conflictsStatus.textContent = String(error); }
  finally { refs.confirmCompleteMerge.disabled = false; refs.confirmCompleteMerge.textContent = target.kind === 'stash' ? 'Done' : 'Complete merge'; }
});

refs.abortMergeButton.addEventListener('click', async () => {
  const target = state.mergeTarget;
  if (target.kind === 'stash') {
    if (!await customConfirm('Discard the conflict markers and restore your last commit? The stashed change stays in the stash list — nothing is lost, you can pop it again (or resolve it differently) later.', { title: 'Discard stash conflict', danger: true, okLabel: 'Discard' })) return;
    try {
      await invoke('abort_stash_conflict', { repositoryPath: state.repository.path });
      refs.conflictsDialog.close();
      const msg = 'Discarded — your working tree is back to normal, and the stash is still there.';
      status(msg); showOperationToast(msg, 'success');
      await refreshAfterMerge(); state.hasStash = true; updateStashUI();
    } catch (error) { handleError(error); }
    return;
  }
  if (!await customConfirm('Abort this merge? All conflict resolutions made so far will be discarded and the repository will return to its pre-merge state.', { title: 'Abort merge', danger: true, okLabel: 'Abort merge' })) return;
  try {
    await invoke('abort_merge', { repositoryPath: state.repository.path, targetPath: target.targetPath });
    refs.conflictsDialog.close();
    const msg = `Merge aborted${target.isSubmodule ? ` in ${target.label}` : ''}.`;
    status(msg); showOperationToast(msg, 'success');
    await refreshAfterMerge();
  } catch (error) { handleError(error); }
});

// Detects a merge left mid-resolution (e.g. the app was closed before finishing)
// and surfaces it via the sidebar banner so it's never silently stuck.
async function checkForMergeConflicts() {
  if (!invoke || !state.repository) { refs.mergeConflictsBanner.hidden = true; return; }
  try {
    const conflicts = await invoke('list_conflicts', { repositoryPath: state.repository.path, targetPath: '' });
    refs.mergeConflictsBanner.hidden = conflicts.length === 0;
    if (conflicts.length) {
      // The same conflicted-index state can come from a real merge or from a
      // stash pop that couldn't apply cleanly — they need different finishing
      // steps (a merge commit vs. nothing at all), so which one this banner
      // means has to be checked, not assumed.
      const inMerge = await invoke('merge_in_progress', { repositoryPath: state.repository.path, targetPath: '' }).catch(() => true);
      state.pendingConflictsKind = inMerge ? 'merge' : 'stash';
      refs.mergeConflictsSubtitle.textContent = `${conflicts.length} file${conflicts.length === 1 ? '' : 's'} to resolve${inMerge ? '' : ' (from a stash)'}`;
    }
    state.pendingMainConflicts = conflicts;
  } catch { refs.mergeConflictsBanner.hidden = true; }
}

refs.mergeConflictsBanner.addEventListener('click', () => openConflictsDialog({ ...mergeTargetForMain(), kind: state.pendingConflictsKind || 'merge' }, state.pendingMainConflicts || []));
$('#mergeCurrent').addEventListener('click', () => state.repository && openMergeBranchDialog(mergeTargetForMain()));

async function forcePushSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  const firstConfirm = await customConfirm(
    `⚠️ FORCE PUSH will OVERWRITE the remote branch for "${entry.name}" with your local history. Any commits on the remote that aren't in your local history will be PERMANENTLY LOST from the server.\n\nOnly do this if you're certain nobody else is using that remote. Continue?`,
    { title: 'Force push — destructive', danger: true, okLabel: 'I understand, continue' }
  );
  if (!firstConfirm) { status('Force push cancelled'); return; }
  const secondConfirm = await customConfirm(
    `Last check: force push submodule "${entry.name}" now? This cannot be undone from here.`,
    { title: 'Force push — final confirmation', danger: true, okLabel: 'Force push' }
  );
  if (!secondConfirm) { status('Force push cancelled'); return; }
  if (!invoke) return status(`Preview: force pushed ${entry.name}`);
  try {
    status(`Force pushing ${entry.name}…`, 'busy');
    const result = await invoke('force_push_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true });
    const shortSha = (result?.revision || '').slice(0, 8);
    const successMsg = `${entry.name}: force pushed to branch "${result?.branch}" (now at ${shortSha}). This project now has a new local commit recording that — see "Unpublished commits" in the sidebar to push it too.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

// Shows the actual commits about to be pushed — before, this was a plain
// "push this submodule?" confirm with no list, which was the whole complaint:
// you had no way to see what you were about to send anywhere.
async function pushSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  if (!invoke) return status(`Preview: pushed ${entry.name}`);
  $('#submodulePublishTitle').textContent = `Push ${entry.name}`;
  $('#submodulePublishCommits').innerHTML = '<div class="loading-row"><i class="spinner"></i>Checking what needs to be pushed…</div>';
  $('#submodulePublishSummary').textContent = 'Checking…'; $('#submodulePublishStatus').textContent = '';
  $('#confirmSubmodulePublish').disabled = true;
  $('#submodulePublishDialog').showModal();
  let commits = [];
  try { const details = await invoke('entry_details', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); commits = details.submodule_unpushed_commits || []; }
  catch (error) { $('#submodulePublishCommits').innerHTML = `<div class="publish-empty">${esc(String(error))}</div>`; return; }
  $('#submodulePublishCommits').innerHTML = commits.map((commit, index) => `<div class="publish-commit"><span>${index + 1}</span><i></i><div><strong>${commitSubjectHtml(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author)} · ${esc(commit.date)}</small></div><b>WILL PUSH</b></div>`).join('') || '<div class="publish-empty">Nothing to push — already up to date, or this submodule has no upstream.</div>';
  $('#submodulePublishSummary').textContent = `${commits.length} commit${commits.length === 1 ? '' : 's'} to push`;
  $('#confirmSubmodulePublish').disabled = !commits.length;

  const confirmed = await new Promise(resolve => {
    const dialog = $('#submodulePublishDialog');
    const onClose = () => { dialog.removeEventListener('close', onClose); resolve(dialog.returnValue === 'default'); };
    dialog.addEventListener('close', onClose);
  });
  if (!confirmed) { status('Push cancelled'); return; }

  try {
    status(`Pushing ${entry.name} to its remote…`, 'busy');
    const result = await invoke('push_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true });
    const shortSha = (result?.revision || '').slice(0, 8);
    const successMsg = `${entry.name}: pushed to branch "${result?.branch}" on its remote (now at ${shortSha}). This project now has a new local commit recording that — see "Unpublished commits" in the sidebar to push it too.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

async function fetchSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  if (!invoke) return status(`Preview: fetched ${entry.name}`);
  try { status(`Fetching ${entry.name}…`, 'busy'); await invoke('fetch_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); status(`${entry.name}: fetched from remote`); showOperationToast(`${entry.name}: fetched from remote`, 'success'); }
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

async function removeEntryFromGit(entry) {
  const label = entry.kind === 'submodule' ? 'submodule, its working folder and its Git link' : entry.kind === 'folder' ? 'folder and all tracked files inside it' : 'file';
  if (!await customConfirm(`REMOVE FROM GIT: delete ${label} locally and stage the deletion of "${entry.relative_path}".\n\nNothing is removed from the server until you commit and push. Continue?`, { title: 'Remove from Git', danger: true, okLabel: 'Remove' })) return;
  if (!invoke) return status(`Preview: remove ${entry.name} from Git`);
  try { status(`Removing ${entry.name} from Git…`, 'busy'); const folder = state.currentPath; await invoke('remove_git_path', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(folder, { force: true }); state.changesScope = 'global'; refs.changesDrawer.classList.add('open'); const message = `${entry.name} deleted locally. Commit the staged deletion, then Publish/Push it to update the server.`; status(message); showOperationToast(message); }
  catch (error) { handleError(error); }
}

async function deleteEntry(entry, button) {
  if (!button?.dataset.confirmed) {
    button.dataset.confirmed = 'true';
    button.textContent = `Confirm delete “${entry.name}”`;
    button.classList.add('confirm-danger');
    const consequence = entry.tracked ? 'The deletion will appear in Workspace, ready for Commit and Push.' : 'This item exists only locally, so no server change will be created.';
    status(`Press “Confirm delete ${entry.name}” once more. ${consequence}`, 'error');
    setTimeout(() => {
      if (!button.isConnected || button.dataset.confirmed !== 'true') return;
      delete button.dataset.confirmed;
      button.textContent = 'Delete…';
      button.classList.remove('confirm-danger');
    }, 8000);
    return;
  }
  button.disabled = true;
  if (!invoke) return status(`Preview: delete ${entry.name}`);
  try { status(`Deleting ${entry.name} locally…`, 'busy'); const folder = state.currentPath; await invoke(entry.tracked ? 'remove_git_path' : 'delete_local_path', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(folder, { force: true }); if (entry.tracked) { state.changesScope = 'global'; refs.changesDrawer.classList.add('open'); } const message = entry.tracked ? `${entry.name} deleted locally. Its deletion is in Workspace; commit it, then Push to update the server.` : `${entry.name} deleted locally. It was local-only, so no commit or push is needed.`; status(message); showOperationToast(message); }
  catch (error) { status(String(error), 'error'); showOperationToast(String(error), 'error'); }
}

async function compareEntryWithRemote(entry) { state.commanderFocus = entry.relative_path; state.commanderPath = entry.relative_path.split('/').slice(0, -1).join('/'); state.view = 'commander'; state.commanderRows = []; render(); await openCommanderDirectory(state.commanderPath); const row = state.commanderRows.find(item => item.relative_path === entry.relative_path); if (row?.local?.kind === 'file' && row.remote?.kind === 'file') openFileCompare(row); else status('This file is not available on both local and selected remote', 'error'); }
async function runEntryFileAction(entry, action) {
  const explanations = { head: 'RESTORE: this permanently discards ALL uncommitted edits in this file. The file on disk will become identical to the last commit (HEAD). This cannot be undone. Continue?', stage: 'Add this file to the staging area?', unstage: 'UNSTAGE: remove this file from staging. Your edits on disk are kept exactly as they are — only the staging entry is removed. Continue?' };
  const successMessages = { head: `${entry.name}: restored from the last commit (HEAD) — edits on disk were discarded.`, stage: `${entry.name}: staged. It will be included in the next commit.`, unstage: `${entry.name}: unstaged — removed from staging, edits on disk were kept unchanged.` };
  const titles = { head: 'Restore file', stage: 'Stage file', unstage: 'Unstage file' };
  if (!await customConfirm(explanations[action], { title: titles[action], danger: action === 'head', okLabel: action === 'head' ? 'Discard everything' : 'Continue' })) return;
  if (!invoke) { status(`Preview: ${action} ${entry.name}`); showOperationToast(`Preview: ${action} ${entry.name}`); return; }
  try {
    if (action === 'head') await invoke('restore_file', { repositoryPath: state.repository.path, relativePath: entry.relative_path, sourceRef: 'HEAD' }); else await invoke(action === 'stage' ? 'stage_files' : 'unstage_files', { path: state.repository.path, files: [entry.relative_path] });
    const folder = state.currentPath; directoryCache.clear(); await loadRepository(state.repository.path); if (folder) await openDirectory(folder, { force: true });
    if (state.selectedEntry?.relative_path === entry.relative_path) await selectEntry(entry.relative_path);
    status(successMessages[action]); showOperationToast(successMessages[action], 'success');
  } catch (error) { const message = handleError(error); showOperationToast(`Failed: ${message}`, 'error'); }
}

async function openEditor(entry) {
  state.editingPath = entry.relative_path; state.editorOriginal = ''; refs.editorTitle.textContent = `Edit ${entry.name}`; refs.editorPath.textContent = entry.relative_path; refs.editorContent.value = 'Loading…'; refs.editorDialog.showModal();
  if (!invoke) { refs.editorContent.value = 'Preview editor\n'; state.editorOriginal = refs.editorContent.value; addEditorBlameHints(); return; }
  try { const file = await invoke('read_text_file', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); refs.editorContent.value = file.content; state.editorOriginal = file.content; updateEditorSaveState(); refs.editorContent.focus(); addEditorBlameHints(); }
  catch (error) { refs.editorDialog.close(); handleError(error); }
}

function addEditorBlameHints() {
  if (state.editingConflict) return;
  setTimeout(() => {
    const content = refs.editorContent;
    const lines = content.value.split('\n');
    const hints = lines.map((line, i) => `<span data-line="${i}" data-tooltip="Loading blame info…" style="color:#6b7f96;font-size:10px;">${String(i+1).padStart(4)}</span>`).join('\n');

    if (!invoke) return;
    try {
      invoke('file_blame', { repositoryPath: state.repository.path, relativePath: state.editingPath })
        .then(blame => {
          const blameByLine = {};
          if (blame && blame.lines) {
            blame.lines.forEach((info, i) => {
              blameByLine[i] = info;
            });
          }
          document.querySelectorAll('[data-line]').forEach(lineEl => {
            const lineNum = parseInt(lineEl.dataset.line);
            const info = blameByLine[lineNum];
            if (info) {
              const tooltip = `${info.author || '?'} · ${info.date || '?'} · ${info.message || '?'}`;
              lineEl.setAttribute('data-tooltip', tooltip);
            }
          });
        })
        .catch(() => {});
    } catch (e) {}
  }, 300);
}

async function saveEditor(event) {
  event?.preventDefault();
  if (state.editingConflict) {
    const { target, path } = state.editingConflict;
    const joined = target.targetPath ? `${target.targetPath}/${path}` : path;
    try {
      status(`Saving ${path}…`, 'busy');
      await invoke('write_text_file', { repositoryPath: state.repository.path, relativePath: joined, content: refs.editorContent.value });
      await invoke('resolve_conflict', { repositoryPath: state.repository.path, targetPath: target.targetPath, relativePath: path, resolution: 'manual' });
      refs.editorDialog.close(); state.editingConflict = null;
      status(`${path}: marked resolved.`); showOperationToast(`${path}: marked resolved.`, 'success');
      await refreshConflictsDialog(target);
    } catch (error) { handleError(error); }
    return;
  }
  const folder = state.currentPath;
  try { status(`Saving ${state.editingPath}…`, 'busy'); await saveEditorContent(); refs.editorDialog.close(); directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(folder, { force: true }); status(`Saved ${state.editingPath}`); }
  catch (error) { handleError(error); }
}

async function saveEditorContent() { if (!invoke) { state.editorOriginal = refs.editorContent.value; updateEditorSaveState(); return; } await invoke('write_text_file', { repositoryPath: state.repository.path, relativePath: state.editingPath, content: refs.editorContent.value }); state.editorOriginal = refs.editorContent.value; updateEditorSaveState(); }
function updateEditorSaveState() { const dirty = refs.editorContent.value !== state.editorOriginal; $('#editorSaveState').textContent = dirty ? 'Unsaved local edits' : 'Saved locally · UTF-8 · maximum 2 MB'; $('#editorSaveState').classList.toggle('warning', dirty); }

async function openSubmoduleGraph(entry) {
  clearDetails('Select a submodule commit');
  if (!invoke) { state.graphContext = { name: entry.name }; state.view = 'graph'; render(); return; }
  try { const data = await invoke('submodule_repository', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); state.graphContext = { name: entry.name, parent: { repository: state.repository, branches: state.branches, commits: state.allCommits, changes: state.changes } }; state.repository = data.repository; state.branches = data.branches; state.commits = data.commits; state.allCommits = data.commits; state.changes = data.changes; state.stashes = data.stashes || []; state.view = 'graph'; render(); }
  catch (error) { handleError(error); }
}

function leaveSubmoduleGraph() { const parent = state.graphContext?.parent; if (!parent) return; Object.assign(state, parent); state.allCommits = parent.commits; state.graphContext = null; state.view = 'explorer'; openDirectory(state.currentPath, { force: true }); }

function renderBranches() {
  refs.branches.innerHTML = state.branches.map((branch, index) => `<div class="branch-row ${branch.current ? 'active' : ''}" data-branch="${esc(branch.name)}" data-is-remote="${branch.remote ? 'true' : 'false'}">
    <span class="branch-bullet" style="border-color:${palette[index % palette.length]}"></span>
    <span class="branch-name">${esc(branch.name)}</span>
    <div style="display:flex;gap:6px;margin-left:auto;">
      ${branch.current ? '<small>HEAD</small>' : branch.remote ? '<small>remote</small>' : `<button class="switch-branch" data-branch="${esc(branch.name)}" title="Switch to ${esc(branch.name)}">↔</button>`}
      ${!branch.remote && !branch.current ? `<button class="branch-menu" data-branch="${esc(branch.name)}" title="Branch actions" style="width:20px;height:20px;padding:0;font-size:14px;border-radius:3px;">⋮</button>` : ''}
    </div>
  </div>`).join('') || '<div class="empty-change">No branches</div>';
  refs.branches.querySelectorAll('.switch-branch').forEach(button => button.addEventListener('click', event => { event.stopPropagation(); switchBranch(button.dataset.branch); }));
  refs.branches.querySelectorAll('.branch-menu').forEach(button => button.addEventListener('click', event => { event.stopPropagation(); showBranchMenu(button.dataset.branch, event); }));
}

function showBranchMenu(branchName, event) {
  const menu = `<div style="position:fixed;top:${event.clientY}px;left:${event.clientX}px;z-index:100;background:#1a2530;border:1px solid #465563;border-radius:6px;box-shadow:0 8px 24px #0008;">
    <button style="display:block;width:100%;padding:8px 14px;text-align:left;border:0;background:transparent;color:#d8e5f0;cursor:pointer;font-size:12px;" data-action="rename">Rename</button>
    <button style="display:block;width:100%;padding:8px 14px;text-align:left;border:0;background:transparent;color:#d8e5f0;cursor:pointer;font-size:12px;border-top:1px solid #465563;" data-action="delete">Delete</button>
  </div>`;
  const menuEl = document.createElement('div');
  menuEl.innerHTML = menu;
  const menuContainer = menuEl.firstElementChild;
  document.body.appendChild(menuContainer);
  menuContainer.querySelectorAll('[data-action]').forEach(btn => {
    btn.addEventListener('click', async () => {
      document.body.removeChild(menuContainer);
      const action = btn.dataset.action;
      if (action === 'rename') { const newName = await customPrompt(`Rename branch "${branchName}" to:`, branchName, { title: 'Rename branch' }); if (newName && newName !== branchName) await renameBranch(branchName, newName); }
      else if (action === 'delete') { if (await customConfirm(`Delete branch "${branchName}"?`, { title: 'Delete branch', danger: true, okLabel: 'Delete' })) await deleteBranch(branchName); }
    });
  });
  document.addEventListener('click', (e) => { if (!menuContainer.contains(e.target)) document.body.removeChild(menuContainer); }, { once: true });
}

async function renameBranch(oldName, newName) {
  if (!state.repository) return;
  if (!invoke) { status(`Preview: renamed ${oldName} to ${newName}`); return; }
  try { status('Renaming branch…', 'busy'); await invoke('rename_branch', { repositoryPath: state.repository.path, oldName, newName }); await loadRepository(state.repository.path, { keepPath: true }); status(`Branch renamed to ${newName}`); }
  catch (error) { handleError(error); }
}

async function deleteBranch(branchName) {
  if (!state.repository) return;
  if (!invoke) { status(`Preview: deleted ${branchName}`); return; }
  try { status('Deleting branch…', 'busy'); await invoke('delete_branch', { repositoryPath: state.repository.path, branchName }); await loadRepository(state.repository.path, { keepPath: true }); status(`Branch ${branchName} deleted`); }
  catch (error) { handleError(error); }
}

function renderRemotes() {
  refs.remoteCards.innerHTML = state.remotes.map(remote => `<article class="remote-card"><div class="remote-symbol">◎</div><div><h2>${esc(remote.name)}</h2><span>FETCH URL</span><code>${esc(remote.fetch_url)}</code><span>PUSH URL</span><code>${esc(remote.push_url)}</code></div><button data-fetch-remote="${esc(remote.name)}">Fetch now</button></article>`).join('') || '<div class="remote-empty">No remote is configured for this repository.</div>';
  refs.remoteCards.querySelectorAll('[data-fetch-remote]').forEach(button => button.addEventListener('click', () => fetchRemote(button.dataset.fetchRemote)));
}

async function loadRemotes() { state.view = 'remotes'; refs.search.value = ''; clearDetails('Remote configuration'); if (invoke) { try { state.remotes = await invoke('list_remotes', { repositoryPath: state.repository.path }); } catch (error) { handleError(error); } } else state.remotes = [{ name: 'origin', fetch_url: 'git@example.com:vehicle-control.git', push_url: 'git@example.com:vehicle-control.git' }]; render(); }
async function fetchRemote(name) { try { status(`Fetching ${name}…`, 'busy'); await invoke('fetch_remote', { repositoryPath: state.repository.path, remote: name }); await loadRepository(state.repository.path, { keepPath: true }); await loadRemotes(); status(`${name} updated`); } catch (error) { handleError(error); } }
async function fetchAllRemotes() {
  if (!state.repository) return;
  if (!invoke) return status('Preview: fetched all remotes');
  try { status('Fetching every remote…', 'busy'); await invoke('fetch_all_remotes', { repositoryPath: state.repository.path }); await loadRepository(state.repository.path, { keepPath: true }); await loadRemotes(); const msg = `${state.remotes.length} remote${state.remotes.length === 1 ? '' : 's'} updated`; status(msg); showOperationToast(msg, 'success'); }
  catch (error) { handleError(error); }
}

async function stashWork() {
  if (!state.repository) return;
  if (!invoke) { state.hasStash = true; updateStashUI(); status('Preview: work stashed'); return; }
  try { status('Saving work in progress…', 'busy'); await invoke('stash_changes', { repositoryPath: state.repository.path }); state.hasStash = true; updateStashUI(); refs.commitMessage.value = ''; await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true }); status('Work saved to stash'); showOperationToast('Work stashed. Use "Pop stash" to restore it.'); }
  catch (error) { handleError(error); }
}

async function stashOneFile(path) {
  if (!state.repository) return;
  if (!invoke) { status(`Preview: ${path} stashed`); return; }
  try {
    status(`Setting ${path} aside…`, 'busy');
    await invoke('stash_file', { repositoryPath: state.repository.path, relativePath: path });
    state.hasStash = true; updateStashUI();
    const folder = state.currentPath; await loadRepository(state.repository.path); if (folder) await openDirectory(folder, { force: true });
    // The stashed file just went back to its committed state, so the details
    // panel (if it's the one showing) needs to drop its now-stale "Modified"
    // banner and action buttons rather than keep them around.
    if (state.selectedEntry?.relative_path === path) await selectEntry(path);
    status(`${path} set aside — use "Pop stash" to bring it back`); showOperationToast(`${path} moved to the stash. It won't be part of any commit until you Pop it back.`);
  } catch (error) { handleError(error); }
}

// "Pop stash" opens the full list rather than blindly restoring whatever
// happens to be most recent — with per-file stash making it common to have
// several at once, silently guessing which one you meant was the actual
// complaint ("I can't find the list of stashes anywhere").
function popStash() {
  if (!state.repository || !state.hasStash) return;
  refs.stashesDialog.showModal();
  refreshStashesList();
}

// Bumped on every render — a file-list fetch launched by an older render
// that resolves after a newer one has already started is discarded instead
// of writing (now-stale) data into the dialog. Without this, restoring two
// files in quick succession, or restoring one right as the dialog was still
// loading, could let an earlier, now-outdated response land last and make a
// just-restored file appear to still be there.
let stashesRenderGeneration = 0;

async function refreshStashesList() {
  if (!state.repository) return;
  try { const data = await invoke('load_repository', { path: state.repository.path, force: true }); state.stashes = data.stashes; state.hasStash = data.stashes.length > 0; updateStashUI(); }
  catch (error) { handleError(error); }
  renderStashesList();
}

function renderStashesList() {
  const generation = ++stashesRenderGeneration;
  const stashes = state.stashes || [];
  refs.stashesList.innerHTML = stashes.map(stash => `<div class="conflict-row stash-entry-row" data-stash-index="${stash.index}">
    <div class="conflict-head"><span class="conflict-path">stash@{${stash.index}}: ${esc(stash.message.replace(/^WIP on [^:]+:\s*[0-9a-f]+\s*/, 'WIP on ') || 'Saved work')}</span><button class="stash-drop-icon" data-drop-stash="${stash.index}" title="Drop this entire stash — discards everything left in it, for good">✕</button></div>
    <div class="stash-file-list" data-stash-file-list="${stash.index}"><i class="spinner"></i></div>
  </div>`).join('') || '<div class="empty-change">Nothing set aside right now.</div>';
  stashes.forEach(stash => loadStashFileList(stash.index, generation));
  refs.stashesList.querySelectorAll('[data-drop-stash]').forEach(button => button.addEventListener('click', () => dropStashEntry(Number(button.dataset.dropStash))));
}

// A plain list — one row per file, one small icon button that restores just
// that file immediately. Once restored, it's genuinely gone from this stash
// (not just hidden), so the row is removed from the list right away.
function loadStashFileList(stashIndex, generation) {
  const fileList = refs.stashesList.querySelector(`[data-stash-file-list="${stashIndex}"]`);
  if (!fileList) return;
  const render = files => {
    if (generation !== stashesRenderGeneration) return; // a newer render has since started — this response is stale
    fileList.innerHTML = files.length ? files.map(file => `<div class="stash-file-row" data-stash-file-path="${esc(file)}">
      <span class="stash-file">${esc(file)}</span>
      <button class="stash-restore-icon" data-restore-file="${esc(file)}" data-restore-stash="${stashIndex}" title="Restore just this file">⇈</button>
    </div>`).join('') : '<div class="empty-change">Nothing left in this stash</div>';
    fileList.querySelectorAll('[data-restore-file]').forEach(button => button.addEventListener('click', () => restoreOneStashFile(Number(button.dataset.restoreStash), button.dataset.restoreFile)));
  };
  if (!invoke) { render(['preview.txt']); return; }
  invoke('stash_entry_files', { repositoryPath: state.repository.path, stashIndex })
    .then(render)
    .catch(error => { if (generation === stashesRenderGeneration) fileList.innerHTML = `<div class="stash-file-row">${esc(String(error))}</div>`; });
}

async function restoreOneStashFile(index, path) {
  if (!invoke) { status(`Preview: ${path} restored`); return; }
  const row = refs.stashesList.querySelector(`[data-stash-index="${index}"] [data-stash-file-path="${CSS.escape(path)}"]`);
  const button = row?.querySelector('[data-restore-file]'); if (button) { button.disabled = true; button.innerHTML = '<i class="spinner"></i>'; }
  try {
    await invoke('restore_stash_paths', { repositoryPath: state.repository.path, stashIndex: index, paths: [path] });
    await loadRepository(state.repository.path, { keepPath: true });
    // A path-filtered restore can still conflict if this one file changed
    // since it was stashed — same resolution flow as a full pop.
    const conflicts = await invoke('list_conflicts', { repositoryPath: state.repository.path, targetPath: '' });
    if (conflicts.length) {
      const msg = `Restored, but ${path} conflicted — resolve it below.`;
      status(msg, 'error'); showOperationToast(msg, 'error');
      refs.stashesDialog.close();
      openConflictsDialog({ targetPath: '', label: state.repository.current_branch, isSubmodule: false, kind: 'stash' }, conflicts);
      return;
    }
    // Re-render the whole list from what the backend now actually reports —
    // force a genuinely fresh read rather than trust the short status cache,
    // so there's no window where a just-restored file could still show up.
    await refreshStashesList();
    const msg = `${path} restored.`;
    status(msg); showOperationToast(msg, 'success');
  } catch (error) { handleError(error); if (button) { button.disabled = false; button.textContent = '⇈'; } }
}

async function dropStashEntry(index) {
  if (!await customConfirm('Discard this stash? Its changes are gone for good — this cannot be undone.', { title: 'Drop stash', danger: true, okLabel: 'Drop' })) return;
  if (!invoke) { status('Preview: stash dropped'); return; }
  try {
    await invoke('drop_stash', { repositoryPath: state.repository.path, stashIndex: index });
    await loadRepository(state.repository.path, { keepPath: true });
    renderStashesList();
    status('Stash dropped');
  } catch (error) { handleError(error); }
}

function updateStashUI() { $('#stashWork').hidden = state.hasStash; $('#popStash').hidden = !state.hasStash; }

// ---- Git DAG / topology model -------------------------------------------
// Assigns each commit a stable vertical lane. Lanes are never renumbered or
// shifted for unrelated commits — a lane slot is only ever (a) kept as-is,
// (b) reused in place by the first still-unseen parent of the commit that
// currently occupies it, or (c) freed and later reused (first-fit) by a
// later, unrelated fork. This is what keeps a linear branch pinned to one
// lane for its entire visible history instead of drifting row to row.
// Every commit reachable from `primaryTipId` (the selected branch's tip) —
// walking ALL parents, not just first-parent, so a merge into the selected
// branch still counts its merged-in history as "belongs to this branch".
function reachableFrom(tipId, commits) {
  const set = new Set(); if (!tipId) return set;
  const byId = new Map(commits.map(c => [c.id, c]));
  const stack = [tipId];
  while (stack.length) {
    const id = stack.pop(); if (set.has(id)) continue; set.add(id);
    const commit = byId.get(id); if (commit) (commit.parents || []).forEach(p => stack.push(p));
  }
  return set;
}

// Lane 0 is reserved for the selected branch's own ancestry. A commit that
// isn't reachable from it — e.g. another branch's commit made after the two
// diverged — is never placed there, even if it's chronologically newer and
// would otherwise be first in line; it gets its own lane instead, and that
// lane only exists for as long as it actually needs to (freed again right
// after its edge reconnects to the primary chain).
function buildGraphModel(commits, primaryTipId) {
  const primarySet = reachableFrom(primaryTipId, commits);
  const lanes = []; // lanes[i] = commitId currently occupying that lane, or null
  if (primarySet.size && commits.some(c => c.id === primaryTipId)) lanes[0] = primaryTipId;
  const dedupe = list => (list || []).filter((id, index, all) => id && all.indexOf(id) === index);

  return commits.map((commit, row) => {
    const isPrimary = primarySet.has(commit.id);
    let lane = lanes.indexOf(commit.id);
    if (lane < 0) {
      if (isPrimary) { lane = lanes[0] == null ? 0 : lanes.indexOf(null); if (lane < 0) lane = lanes.length; }
      else { lane = lanes.length > 1 ? lanes.indexOf(null, 1) : -1; if (lane < 0) lane = Math.max(lanes.length, 1); }
    }
    const before = lanes.slice();
    lanes[lane] = null;

    const parentIds = dedupe(commit.parents);
    const newParents = parentIds.filter(id => !lanes.includes(id));
    const primaryParent = newParents.find(id => primarySet.has(id));
    const secondaryNewParents = newParents.filter(id => id !== primaryParent);
    if (primaryParent && lanes[0] == null) { lanes[0] = primaryParent; }
    if (secondaryNewParents.length) {
      let index = 0;
      if (lane !== 0) { lanes[lane] = secondaryNewParents[0]; index = 1; }
      for (; index < secondaryNewParents.length; index++) {
        let slot = lanes.length > 1 ? lanes.indexOf(null, 1) : -1; if (slot < 0) slot = Math.max(lanes.length, 1);
        lanes[slot] = secondaryNewParents[index];
      }
    }
    while (lanes.length && lanes[lanes.length - 1] == null) lanes.pop();
    const after = lanes.slice();

    return {
      commitId: commit.id, row, lane, before, after, isPrimary,
      parents: parentIds.map(id => ({ commitId: id, targetRow: row + 1, targetLane: after.indexOf(id) })).filter(p => p.targetLane >= 0),
      refs: commit.refs || [],
      type: 'commit',
    };
  });
}

const LANE_WIDTH = 30;
const laneX = lane => LANE_WIDTH / 2 + lane * LANE_WIDTH;

// Show at most a couple of ref labels inline — the graph's shape is the main
// signal, refs are a secondary lookup. Anything beyond that collapses into a
// single "+N" pill instead of filling the row with badges.
const MAX_INLINE_REFS = 2;
function refsBadges(refList, color, isHead) {
  const grouped = refList.filter(ref => ref !== 'HEAD');
  const headPill = isHead ? '<b class="head-pill">HEAD</b>' : '';
  if (!grouped.length && !headPill) return '';
  const shown = grouped.slice(0, MAX_INLINE_REFS);
  const rest = grouped.slice(MAX_INLINE_REFS);
  const restPill = rest.length ? `<b class="ref-pill ref-pill-more" data-tooltip="${esc(rest.join(', '))}">+${rest.length}</b>` : '';
  return `<span class="ref-pills">${headPill}${shown.map(ref => `<b class="ref-pill" style="--lane-color:${color}">${esc(ref)}</b>`).join('')}${restPill}</span>`;
}

// ---- Rendering -------------------------------------------------------------
// Every node/edge is drawn on a single continuous SVG overlaid across the whole
// list, using each row's *actual* rendered position (measured from the DOM
// after layout) rather than an assumed fixed row height — this is what
// guarantees a commit's dot always lines up exactly with its own row, and that
// a parent/child edge is one real line straight from one dot to the other,
// however many rows apart they end up being (instead of independent per-row
// segments that only look connected when every row happens to be the same
// height).
function renderGraph() {
  const query = refs.search.value.trim().toLowerCase();
  const commits = state.commits.filter(c => !query || `${c.subject} ${c.author} ${c.id} ${(c.refs || []).join(' ')}`.toLowerCase().includes(query));
  const currentBranch = state.repository?.current_branch;
  // "Selected branch" drives which lane is primary — defaults to whatever is
  // currently checked out. Any branch whose tip isn't reachable from it (a
  // sibling that's diverged, even if newer) gets pushed to its own lane
  // instead of ever sharing the primary one.
  const localBranchNames = (state.branches || []).filter(b => !b.remote).map(b => b.name);
  const primaryBranchName = state.graphPrimaryBranch && localBranchNames.includes(state.graphPrimaryBranch) ? state.graphPrimaryBranch : currentBranch;
  const primaryTip = primaryBranchName ? commits.find(c => (c.refs || []).includes(primaryBranchName)) : null;
  const model = buildGraphModel(commits, primaryTip?.id);
  const nodeById = new Map(commits.map((commit, index) => [commit.id, model[index]]));
  const headEntry = currentBranch ? commits.find(commit => (commit.refs || []).includes(currentBranch)) : null;
  if (headEntry) { nodeById.get(headEntry.id).isHead = true; }
  const maxLanes = Math.max(1, ...model.map(n => Math.max(n.before.length, n.after.length)));
  const lanesWidth = maxLanes * LANE_WIDTH;

  refs.laneLegend.style.setProperty('--lanes-width', `${lanesWidth}px`);
  refs.laneLegend.innerHTML = `<span class="time-direction"><b>NEWEST</b><i>↓</i><b>OLDEST</b></span>
    ${currentBranch ? `<span class="head-banner">HEAD <i>→</i> <b>${esc(currentBranch)}</b></span>` : ''}
    ${localBranchNames.length > 1 ? `<label class="primary-branch-picker"><span>Primary</span><select id="graphPrimaryBranch">${localBranchNames.map(name => `<option value="${esc(name)}" ${name === primaryBranchName ? 'selected' : ''}>${esc(name)}</option>`).join('')}</select></label>` : ''}
    <span class="lane-header"><span>GRAPH</span><span>COMMIT</span></span>`;
  $('#graphPrimaryBranch')?.addEventListener('change', event => { state.graphPrimaryBranch = event.target.value; renderGraph(); });

  // Stash entries are informational pointers, not real DAG commits. Rendering
  // them as their own row used to insert a break in the middle of the vertical
  // chain, making a branch that's just one commit ahead look like it "floats"
  // disconnected from where it actually came from. They're attached instead as
  // a small lateral pill on their base commit's own row — secondary
  // information, never interrupting the parent/child line.
  const stashesByBase = new Map();
  (state.stashes || []).forEach(stash => { if (!stashesByBase.has(stash.base_commit)) stashesByBase.set(stash.base_commit, []); stashesByBase.get(stash.base_commit).push(stash); });

  // When a straight (single-lane) run of commits has a ref-labeled commit at
  // the top and another, differently-labeled one further down with nothing
  // labeled in between, that gap between them is a branch point: the lower
  // commit is the common ancestor both refs share, the upper one(s) exist
  // only on the newer ref. Marked on both ends, kept to one short line each —
  // the tip gets "N commit(s) ahead", the base gets a distinct marker instead
  // of trying to bend a single real lane into a decorative fork.
  const aheadAnnotations = new Map(); // row index -> short text
  const branchPointRows = new Set(); // row indices that are a shared-ancestor base
  for (let i = 0; i < model.length; i++) {
    const from = model[i]; const fromRefs = (from.refs || []).filter(r => r !== 'HEAD');
    if (!fromRefs.length) continue;
    let j = i + 1;
    while (j < model.length && model[j].lane === from.lane && !(model[j].refs || []).filter(r => r !== 'HEAD').length) j++;
    if (j >= model.length || model[j].lane !== from.lane) continue;
    const toRefs = (model[j].refs || []).filter(r => r !== 'HEAD');
    if (!toRefs.length || toRefs.join(',') === fromRefs.join(',')) continue;
    const distance = j - i;
    aheadAnnotations.set(i, `↳ ${distance} commit${distance === 1 ? '' : 's'} ahead of ${toRefs.join('/')}`);
    branchPointRows.add(j);
  }
  // More generally: wherever an edge actually crosses lanes (a real diagonal
  // fork/merge line, not just a straight same-lane continuation), the commit
  // it lands on is a genuine branch/merge point — mark it the same way.
  const commitIdToRow = new Map(commits.map((c, i) => [c.id, i]));
  model.forEach(node => node.parents.forEach(parent => {
    if (parent.targetLane === node.lane) return;
    const targetRow = commitIdToRow.get(parent.commitId);
    if (targetRow != null) branchPointRows.add(targetRow);
  }));

  const rows = commits.map((commit, index) => {
    const node = model[index]; const color = palette[node.lane % palette.length];
    const stashPills = (stashesByBase.get(commit.id) || []).map(stash => `<b class="stash-pill" data-toggle-stash="${stash.index}" data-tooltip="stash@{${stash.index}} — click for details">⇕ stash</b>`).join('');
    const stashDetails = (stashesByBase.get(commit.id) || []).map(stash => `<div class="stash-internals" data-stash-detail="${stash.index}" hidden>
      <div>stash@{${stash.index}}: ${esc(stash.message.replace(/^WIP on [^:]+:\s*[0-9a-f]+\s*/, 'WIP on ') || 'Saved work')} — bundles working-tree changes, staged index${stash.message.includes('untracked') ? ', untracked files' : ''}</div>
      <div class="stash-file-list" data-stash-file-list="${stash.index}"></div>
    </div>`).join('');
    const ahead = aheadAnnotations.get(index);
    const isBranchPoint = branchPointRows.has(index);

    return `<article class="commit-row ${node.isHead ? 'is-head' : ''} ${isBranchPoint ? 'is-branch-point' : ''}" data-id="${esc(commit.id)}" data-lane="${node.lane}">
      <div class="graph-cell" style="width:${lanesWidth}px"></div>
      <div class="commit-body">
        <div class="commit-card"><div class="commit-main">${isBranchPoint ? '<b class="branch-point-pill" data-tooltip="Common ancestor — where the newer branch above split off">⑂</b>' : ''}<span class="commit-title">${commitSubjectHtml(commit.subject)}</span>${refsBadges(node.refs, color, node.isHead)}${stashPills}</div><span class="commit-id">${esc(commit.id.slice(0, 8))}</span>
        <span class="topology-badges">${commit.parents?.length > 1 ? `<b class="merge-badge">MERGE</b>` : ''}</span><span class="commit-author">${esc(commit.author)}</span></div>
        ${ahead ? `<div class="ahead-annotation">${esc(ahead)}</div>` : ''}${stashDetails}
      </div>
    </article>`;
  }).join('') || '<div class="empty-change">No commits match this filter</div>';

  refs.graph.innerHTML = `<svg class="graph-overlay"></svg>` + rows;
  refs.graph.querySelectorAll('.commit-row[data-id]').forEach(row => row.addEventListener('click', () => selectCommit(row.dataset.id)));
  refs.graph.querySelectorAll('[data-toggle-stash]').forEach(pill => pill.addEventListener('click', event => {
    event.stopPropagation();
    const index = pill.dataset.toggleStash;
    const detail = pill.closest('.commit-card').parentElement.querySelector(`[data-stash-detail="${index}"]`);
    if (!detail) return;
    detail.toggleAttribute('hidden');
    const fileList = detail.querySelector(`[data-stash-file-list="${index}"]`);
    // Load the file list lazily, only the first time this stash is expanded
    // — "what's actually in there?" answered without needing to pop it first.
    if (!detail.hidden && fileList && !fileList.dataset.loaded) {
      fileList.dataset.loaded = '1';
      fileList.innerHTML = '<i class="spinner"></i>';
      if (!invoke) { fileList.innerHTML = '<div class="stash-file">preview.txt</div>'; return; }
      invoke('stash_entry_files', { repositoryPath: state.repository.path, stashIndex: Number(index) })
        .then(files => { fileList.innerHTML = files.length ? files.map(file => `<div class="stash-file">${esc(file)}</div>`).join('') : '<div class="stash-file">(no files — this stash is empty)</div>'; })
        .catch(error => { fileList.innerHTML = `<div class="stash-file">${esc(String(error))}</div>`; });
    }
  }));
  if (commits.length) drawGraphOverlay(model, lanesWidth);
}

function drawGraphOverlay(model, lanesWidth) {
  const container = refs.graph;
  const positions = new Map(); // commitId -> {x, y}
  const branchPointIds = new Set();
  container.querySelectorAll('.commit-row[data-id]').forEach((row, index) => {
    const node = model[index];
    positions.set(node.commitId, { x: laneX(node.lane), y: row.offsetTop + row.offsetHeight / 2 });
    if (row.classList.contains('is-branch-point')) branchPointIds.add(node.commitId);
  });

  const parts = [];
  model.forEach(node => {
    const from = positions.get(node.commitId); if (!from) return;
    node.parents.forEach(parent => {
      const to = positions.get(parent.commitId); if (!to) return;
      const color = palette[node.lane % palette.length];
      parts.push(from.x === to.x
        ? `<line x1="${from.x}" y1="${from.y}" x2="${to.x}" y2="${to.y}" stroke="${color}" stroke-width="3" stroke-linecap="round"/>`
        : `<path d="M${from.x} ${from.y} C${from.x} ${(from.y + to.y) / 2} ${to.x} ${(from.y + to.y) / 2} ${to.x} ${to.y}" stroke="${color}" stroke-width="3" fill="none" stroke-linecap="round"/>`);
    });
  });
  model.forEach(node => {
    const pos = positions.get(node.commitId); if (!pos) return;
    const color = palette[node.lane % palette.length];
    if (node.isHead) {
      parts.push(`<circle cx="${pos.x}" cy="${pos.y}" r="8" fill="${color}" stroke="#0d1117" stroke-width="2.5"/><circle cx="${pos.x}" cy="${pos.y}" r="8" fill="none" stroke="#e8eef5" stroke-width="1.6"/>`);
    } else if (branchPointIds.has(node.commitId)) {
      // The common ancestor two differently-labeled refs share — a distinct
      // ring marks it as the split point, without bending the (real, single)
      // lane into a decorative fork it doesn't topologically have yet.
      parts.push(`<circle cx="${pos.x}" cy="${pos.y}" r="6" fill="${color}" stroke="#0d1117" stroke-width="2.5"/><circle cx="${pos.x}" cy="${pos.y}" r="10" fill="none" stroke="${color}" stroke-width="1.4" stroke-dasharray="2 2"/>`);
    } else {
      parts.push(`<circle cx="${pos.x}" cy="${pos.y}" r="6" fill="${color}" stroke="#0d1117" stroke-width="2.5"/>`);
    }
  });

  const svg = container.querySelector('.graph-overlay');
  const height = container.scrollHeight;
  svg.setAttribute('width', lanesWidth); svg.setAttribute('height', height);
  svg.setAttribute('viewBox', `0 0 ${lanesWidth} ${height}`);
  svg.innerHTML = parts.join('');
}

function selectCommit(id) {
  state.selectedCommit = state.commits.find(commit => commit.id === id);
  refs.graph.querySelectorAll('.commit-row').forEach(row => row.classList.toggle('selected', row.dataset.id === id));
  const c = state.selectedCommit;
  refs.details.innerHTML = `<div class="commit-details"><div class="large-node"></div><h2>${commitSubjectHtml(c.subject)}</h2><div class="hash"><a href="#" class="commit-server-link" data-commit-id="${esc(c.id)}" title="Open this commit on the server">${esc(c.id)} ↗</a></div>
    <div class="detail-grid"><span>Author</span><strong>${esc(c.author)}</strong><span>Date</span><strong>${esc(c.date)}</strong>
    <span>Parents</span><strong>${esc(c.parents?.join(', ') || 'First commit')}</strong><span>Refs</span><strong>${esc(c.refs?.join(', ') || '—')}</strong></div></div>`;
}

function renderChanges() {
  refs.changeBadge.textContent = state.changes.length;
  refs.workspaceSubtitle.textContent = state.repository ? (state.changes.length ? `${state.changes.length} changed files` : 'Everything committed') : 'No repository loaded';
  const scope = state.changesScope === 'folder' ? state.currentPath : ''; const scopedChanges = state.changes.filter(change => !scope || change.path === scope || change.path.startsWith(`${scope}/`));
  refs.drawerScopeTitle.textContent = state.changesScope === 'folder' ? `Changes in folder · /${scope}` : 'Working tree · entire repository';
  refs.changesSummary.textContent = scopedChanges.length ? `${scopedChanges.length} file${scopedChanges.length === 1 ? '' : 's'} available for staging` : `No changes in ${scope ? `/${scope}` : 'the repository'}`;
  refs.changes.innerHTML = scopedChanges.map(change => `<div class="change-row-wrap"><label class="change-row"><input type="checkbox" data-change-path="${esc(change.path)}" ${change.staged ? 'checked' : ''}>
    <span class="status-code">${esc(change.status)}</span><span class="change-path">${esc(change.path)}</span><span class="change-state">${change.staged ? 'Staged' : 'Modified'}</span></label>
    <button class="stash-file-btn" data-stash-path="${esc(change.path)}" title="Set aside just this file for now — moves it to a temporary holding area (the stash) so it's left out of any commit until you bring it back with Pop stash">⇕ Stash</button></div>`).join('') || `<div class="empty-change">No changes inside /${esc(scope)}</div>`;
  refs.changes.querySelectorAll('[data-stash-path]').forEach(button => button.addEventListener('click', () => stashOneFile(button.dataset.stashPath)));
  refs.changes.querySelectorAll('[data-change-path]').forEach(input => {
    // Belt-and-suspenders against a real Chromium/WebView2 quirk: a checkbox
    // just toggled by the user can have its *rendered* `checked` attribute
    // silently overridden by the browser's own form-state-restoration when
    // the surrounding HTML is replaced right after — the row would then show
    // the state from before the click. Setting the property explicitly right
    // after building the HTML always wins over that.
    const change = scopedChanges.find(item => item.path === input.dataset.changePath);
    input.checked = Boolean(change?.staged);
    input.addEventListener('change', () => toggleStage(input.dataset.changePath, input.checked));
  });
  const staged = scopedChanges.filter(change => change.staged).length;
  refs.selectionText.textContent = `${staged} file${staged === 1 ? '' : 's'} in staging area`;
  refs.commitButton.disabled = !staged || !refs.commitMessage.value.trim();
}

async function openPublish() {
  if (!state.repository) return;
  // "Unpublished commits" only ever means the MAIN project — it has no
  // concept of a selected submodule, so clicking it while a submodule is
  // selected silently shows the main project's own history instead, which
  // reads as if the submodule's branch dumped its whole history "to push".
  // Make the scope explicit instead of guessing.
  if (state.selectedEntry?.kind === 'submodule') {
    const entry = state.selectedEntry;
    const showMain = await customConfirm(`"${entry.name}" (a submodule) is currently selected, but "Unpublished commits" always shows the MAIN project — not the submodule. To push the submodule's own commits, cancel this and use "Push submodule" in ${entry.name}'s own details panel instead.`, { title: 'Unpublished commits — main project only', okLabel: 'Show main project anyway' });
    if (!showMain) return pushSubmodule(entry);
  }
  if (!state.remotes.length && invoke) state.remotes = await invoke('list_remotes', { repositoryPath: state.repository.path });
  if (!invoke && !state.remotes.length) state.remotes = [{ name: 'origin', fetch_url: 'git@example.com:vehicle-control.git', push_url: 'git@example.com:vehicle-control.git' }];
  const locals = state.branches.filter(branch => !branch.remote); refs.publishBranch.innerHTML = locals.map(branch => `<option value="${esc(branch.name)}" ${branch.current ? 'selected' : ''}>${esc(branch.name)}${branch.current ? ' (current)' : ''}</option>`).join('');
  refs.publishRemote.innerHTML = state.remotes.map(remote => `<option value="${esc(remote.name)}">${esc(remote.name)}</option>`).join(''); refs.publishDialog.showModal(); await refreshPublish();
}

// Git can only push a contiguous range — there's no way to publish a newer
// commit while holding back an older one it depends on. So "leave this one
// out" can only mean "stop pushing at the commit before it": clicking a
// commit sets it as the cutoff (included, along with everything older);
// everything newer than it is left unpublished for now.
function renderPublishCommits() {
  const commits = state.publish?.commits || [];
  const uptoIndex = state.publishUpto ? commits.findIndex(commit => commit.id === state.publishUpto) : commits.length - 1;
  refs.publishCommits.innerHTML = commits.map((commit, index) => {
    const willPush = index <= uptoIndex;
    return `<div class="publish-commit ${willPush ? '' : 'excluded'}" data-commit-id="${esc(commit.id)}">
      <span>${index + 1}</span><input type="checkbox" class="publish-check" data-index="${index}" ${willPush ? 'checked' : ''}>
      <div><strong>${esc(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author)} · ${esc(commit.date)}</small></div>
      ${willPush ? (index === uptoIndex && index < commits.length - 1 ? '<b class="publish-cutoff-badge" data-tooltip="Everything above stays local for now">WILL PUSH · stop here</b>' : '<b>WILL PUSH</b>') : '<b class="publish-held-back">STAYS LOCAL</b>'}
    </div>`;
  }).join('') || '<div class="publish-empty">This branch is already up to date on the server.</div>';
  // Git can only push a contiguous range from the oldest pending commit
  // forward — unchecking one always means "and everything newer than it
  // too" (they were built on top of it), checking one always means "and
  // everything older than it too" (it needs them). So every checkbox here
  // really sets the same single cutoff point; ticking any box just moves it.
  refs.publishCommits.querySelectorAll('.publish-check').forEach(box => box.addEventListener('change', () => {
    const index = Number(box.dataset.index);
    // '__none__' is a deliberately non-matching id — distinct from `null`,
    // which means "no cutoff set yet, default to a full push".
    state.publishUpto = box.checked ? commits[index].id : (index > 0 ? commits[index - 1].id : '__none__');
    renderPublishCommits(); updatePublishSummary();
  }));
}

function updatePublishSummary() {
  const commits = state.publish?.commits || [];
  const uptoIndex = state.publishUpto ? commits.findIndex(commit => commit.id === state.publishUpto) : commits.length - 1;
  const willPushCount = uptoIndex + 1;
  const heldBack = commits.length - willPushCount;
  refs.publishSummary.textContent = `${willPushCount} commit${willPushCount === 1 ? '' : 's'} to publish${heldBack ? ` · ${heldBack} staying local for now` : ''}`;
  $('#confirmPublish').disabled = !willPushCount;
}

async function refreshPublish() {
  const branch = refs.publishBranch.value, remote = refs.publishRemote.value;
  state.publishUpto = null;
  if (!branch || !remote) { refs.publishCommits.innerHTML = '<div class="publish-empty">Configure a remote before publishing.</div>'; refs.publishSummary.textContent = 'Nothing to publish'; $('#confirmPublish').disabled = true; return; }
  refs.publishDestination.textContent = `${branch} → ${remote}/${branch}`; refs.publishCommits.innerHTML = '<div class="loading-row"><i class="spinner"></i>Checking server state…</div>';
  if (!invoke) state.publish = { branch, remote, commits: previewData.commits.slice(0, 2) }; else state.publish = await invoke('publish_status', { repositoryPath: state.repository.path, branch, remote });
  renderPublishCommits();
  refs.publishBadge.textContent = state.publish.commits.length; refs.publishSubtitle.textContent = state.publish.commits.length ? `${state.publish.commits.length} local commits not on ${remote}` : 'Everything is on the server';
  updatePublishSummary();
}

async function confirmPublish(event) {
  event.preventDefault(); if (!state.publish?.commits.length) return;
  const operation = $('#publishOperationStatus'); operation.textContent = `Publishing ${state.publish.branch}…`; operation.className = 'submodule-operation-status busy';
  try { $('#confirmPublish').disabled = true; status(`Publishing ${state.publish.branch}…`, 'busy'); await invoke('publish_branch', { repositoryPath: state.repository.path, branch: state.publish.branch, remote: state.publish.remote, username: $('#publishUsername').value.trim(), accessToken: $('#publishToken').value, uptoCommit: state.publishUpto || '' }); $('#publishToken').value = ''; refs.publishDialog.close(); await loadRepository(state.repository.path, { keepPath: true }); const msg = state.publishUpto ? `Published part of ${state.publish.branch} to ${state.publish.remote} (up to your chosen commit).` : `Published ${state.publish.branch} to ${state.publish.remote}`; status(msg); showOperationToast(msg); }
  catch (error) { const message = String(error); operation.textContent = message; operation.className = 'submodule-operation-status error'; status(message, 'error'); $('#confirmPublish').disabled = false; }
}

async function toggleStage(path, checked) {
  if (!invoke) return;
  // Reflect the click immediately in state — the round trip to the backend
  // and back through a full repository reload takes a moment, and the
  // checkbox should never visibly sit in a stale state (still "Staged" right
  // after unchecking it) while that's in flight.
  const change = state.changes.find(item => item.path === path); if (change) change.staged = checked;
  renderChanges();
  try { const folder = state.currentPath; await invoke(checked ? 'stage_files' : 'unstage_files', { path: state.repository.path, files: [path] }); await loadRepository(state.repository.path); if (folder) await openDirectory(folder); }
  catch (error) {
    // The backend call failed — the optimistic flip above never actually
    // happened, so undo it rather than leave the checkbox showing a state
    // that isn't real.
    if (change) change.staged = !checked;
    renderChanges();
    handleError(error);
  }
}

async function switchBranch(branch) {
  if (!invoke || !state.repository) return;
  try { status(`Switching to ${branch}…`, 'busy'); await invoke('switch_branch', { path: state.repository.path, branch }); await loadRepository(state.repository.path); }
  catch (error) { handleError(error); }
}

$('#openRepo').addEventListener('click', openRepository); $('#emptyOpen').addEventListener('click', openRepository);
$('#cloneRepo').addEventListener('click', openCloneDialog); $('#chooseCloneParent').addEventListener('click', () => chooseCloneParent().catch(error => status(String(error), 'error'))); refs.confirmClone.addEventListener('click', confirmClone);
refs.cloneUrl.addEventListener('input', () => { if (!refs.cloneName.dataset.edited) { const inferred = refs.cloneUrl.value.trim().split(/[\\/]/).pop()?.replace(/\.git$/, '') || ''; refs.cloneName.value = inferred; } validateCloneForm(); }); refs.cloneParent.addEventListener('input', validateCloneForm); refs.cloneName.addEventListener('input', () => { refs.cloneName.dataset.edited = refs.cloneName.value ? '1' : ''; validateCloneForm(); });
$('#addSubmodule').addEventListener('click', openAddSubmoduleDialog); refs.confirmAddSubmodule.addEventListener('click', confirmAddSubmodule);
refs.submoduleUrl.addEventListener('input', () => { if (!refs.submoduleName.dataset.edited) refs.submoduleName.value = suggestedRepositoryName(refs.submoduleUrl.value); validateSubmoduleForm(); });
refs.submoduleName.addEventListener('input', () => { refs.submoduleName.dataset.edited = refs.submoduleName.value ? '1' : ''; validateSubmoduleForm(); });
$('#refresh').addEventListener('click', () => state.repository && loadRepository(state.repository.path, { keepPath: true, force: true }));
// Opening the drawer with everything already selected (staged) is what most
// people expect from "here's what changed, commit it" — having to manually
// tick every file first before Commit even becomes clickable read as "commit
// doesn't work". This only stages what's genuinely unstaged in scope (real
// `git add`, so the checkbox state stays truthful); anything you uncheck
// afterward gets unstaged again, same as before.
async function stageAllInScope(scope) {
  if (!invoke || !state.repository) return;
  const files = state.changes.filter(change => !change.staged && (!scope || change.path === scope || change.path.startsWith(`${scope}/`))).map(change => change.path);
  if (!files.length) return;
  try { await invoke('stage_files', { path: state.repository.path, files }); await loadRepository(state.repository.path, { keepPath: true }); renderChanges(); }
  catch (error) { handleError(error); }
}
// Pre-fills the commit message box with whatever is in the "default commit
// message" field up top — e.g. a Polarion ID you're committing several
// pieces of work against — every time the Working tree drawer is opened.
// Only fills it in when the box is empty, so it never clobbers a message
// you're already partway through typing in a still-open drawer.
function applyDefaultCommitMessage() { if (!refs.commitMessage.value.trim() && refs.defaultCommitMessage.value.trim()) refs.commitMessage.value = refs.defaultCommitMessage.value.trim(); }

// Remembers every default commit message you've typed in, most recent
// first, capped at 10 — persisted in this browser profile so it survives a
// restart. Shown as a native dropdown under the field (no extra UI needed).
const COMMIT_MESSAGE_HISTORY_KEY = 'git-integrity-default-commit-message-history';
function loadCommitMessageHistory() { try { const list = JSON.parse(localStorage.getItem(COMMIT_MESSAGE_HISTORY_KEY) || '[]'); return Array.isArray(list) ? list.slice(0, 10) : []; } catch { return []; } }
state.commitMessageHistory = loadCommitMessageHistory();
function renderCommitMessageHistory() { $('#defaultCommitMessageHistory').innerHTML = state.commitMessageHistory.map(msg => `<option value="${esc(msg)}"></option>`).join(''); }
function recordDefaultCommitMessage() {
  const trimmed = refs.defaultCommitMessage.value.trim(); if (!trimmed) return;
  state.commitMessageHistory = [trimmed, ...state.commitMessageHistory.filter(item => item !== trimmed)].slice(0, 10);
  try { localStorage.setItem(COMMIT_MESSAGE_HISTORY_KEY, JSON.stringify(state.commitMessageHistory)); } catch { /* private-browsing or storage disabled — history just won't persist */ }
  renderCommitMessageHistory();
}
renderCommitMessageHistory();

$('#showChanges').addEventListener('click', async () => { state.changesScope = 'global'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); await stageAllInScope(''); });
$('#showFolderChanges').addEventListener('click', async () => { state.changesScope = 'folder'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); await stageAllInScope(state.currentPath); });
refs.defaultCommitMessage.addEventListener('input', () => {
  const match = refs.defaultCommitMessage.value.match(/P:([A-Za-z0-9][A-Za-z0-9_]*-\d+)/);
  if (match) { const workitemId = match[1]; const project = workitemId.split('-')[0]; refs.defaultCommitPolarionLink.href = `https://polarion.vitesco.io/polarion/#/project/${project}/workitem?id=${workitemId}`; refs.defaultCommitPolarionLink.hidden = false; }
  else { refs.defaultCommitPolarionLink.hidden = true; }
});
refs.defaultCommitMessage.addEventListener('change', recordDefaultCommitMessage);
$('#showPublish').addEventListener('click', () => openPublish().catch(error => status(String(error), 'error')));
$('#stashWork').addEventListener('click', stashWork);
$('#popStash').addEventListener('click', popStash);
$('#refreshStashes').addEventListener('click', () => refreshStashesList());
$('#closeChanges').addEventListener('click', () => refs.changesDrawer.classList.remove('open'));
let searchTimeout; refs.search.addEventListener('input', () => { clearTimeout(searchTimeout); searchTimeout = setTimeout(() => { state.view === 'explorer' ? renderExplorer() : state.view === 'commander' ? renderCommander() : renderGraph(); }, 200); }); refs.commitMessage.addEventListener('input', renderChanges);
function returnToProjectNavigator() { const selectedPath = state.selectedEntry?.relative_path; state.view = 'explorer'; state.selectedCommit = null; refs.search.value = ''; render(); if (selectedPath) selectEntry(selectedPath); else clearDetails('Select a file or folder'); }
$('#navExplorer').addEventListener('click', returnToProjectNavigator);
$('#navCommander').addEventListener('click', () => { const selected = state.selectedEntry; state.commanderFocus = selected?.kind === 'file' ? selected.relative_path : ''; state.commanderPath = selected?.kind === 'file' ? selected.relative_path.split('/').slice(0, -1).join('/') : selected?.kind === 'folder' ? selected.relative_path : state.currentPath; state.commanderRows = []; state.view = 'commander'; refs.search.value = ''; render(); openCommanderDirectory(state.commanderPath); });
$('#navGraph').addEventListener('click', () => { state.commits = state.allCommits.length ? state.allCommits : state.commits; state.historyScope = ''; state.selectedEntry = null; state.selectedCommit = null; state.view = 'graph'; refs.search.value = ''; clearDetails('Select a commit'); render(); });
$('#navRemotes').addEventListener('click', loadRemotes);
refs.leaveSubmoduleGraph.addEventListener('click', leaveSubmoduleGraph);
(() => {
  const mainLayout = document.querySelector('.main-layout'); const toggle = $('#toggleDetailsPanel');
  const collapsed = localStorage.getItem('detailsPanelCollapsed') === '1';
  if (collapsed) { mainLayout.classList.add('details-collapsed'); toggle.title = 'Expand panel'; }
  toggle.addEventListener('click', () => {
    const isCollapsed = mainLayout.classList.toggle('details-collapsed');
    localStorage.setItem('detailsPanelCollapsed', isCollapsed ? '1' : '0');
    toggle.title = isCollapsed ? 'Expand panel' : 'Collapse panel';
    toggle.setAttribute('data-tooltip', isCollapsed ? 'Expand details panel' : 'Collapse details panel');
  });
})();
(() => {
  const heading = $('#toggleBranchList'); const arrow = heading.querySelector('.branch-heading-arrow');
  const expanded = localStorage.getItem('branchListExpanded') === '1';
  if (expanded) { refs.branches.classList.remove('collapsed'); arrow.textContent = '▾'; }
  heading.addEventListener('click', event => {
    if (event.target.closest('#newBranch')) return;
    const isExpanded = refs.branches.classList.toggle('collapsed') === false;
    localStorage.setItem('branchListExpanded', isExpanded ? '1' : '0');
    arrow.textContent = isExpanded ? '▾' : '▸';
  });
})();
$('#saveFile').addEventListener('click', saveEditor); $('#closeEditor').addEventListener('click', () => refs.editorDialog.close()); $('#cancelEditor').addEventListener('click', () => refs.editorDialog.close());
refs.editorContent.addEventListener('input', updateEditorSaveState);
$('#fetchCurrent').addEventListener('click', () => { const first = state.remotes[0]?.name || state.branches.find(branch => branch.remote)?.name.split('/')[0]; if (first) fetchRemote(first); else status('No remote is configured', 'error'); });
$('#fetchAll').addEventListener('click', () => fetchAllRemotes());
async function syncCurrent(action) {
  if (!invoke || !state.repository) return;
  try { status(`${action === 'pull' ? 'Pulling' : 'Pushing'} ${state.repository.current_branch}…`, 'busy'); await invoke('sync_repository', { repositoryPath: state.repository.path, action }); await loadRepository(state.repository.path, { keepPath: true }); status(`${action === 'pull' ? 'Pull' : 'Push'} complete`); }
  catch (error) {
    const message = handleError(error);
    if (action === 'pull' && String(error).toLowerCase().includes('requires a merge')) {
      showOperationToast(`${message}\nUse "Merge branch…" (⑂) instead — it can resolve conflicts here.`, 'error');
    } else { showOperationToast(message, 'error'); }
  }
}
$('#pullCurrent').addEventListener('click', () => syncCurrent('pull')); $('#pushCurrent').addEventListener('click', () => syncCurrent('push'));
refs.publishBranch.addEventListener('change', refreshPublish); refs.publishRemote.addEventListener('change', refreshPublish); $('#confirmPublish').addEventListener('click', confirmPublish);
[['#compareRestoreRemote','remote'],['#compareRestoreHead','head'],['#compareStage','stage'],['#compareUnstage','unstage']].forEach(([selector, action]) => $(selector)?.addEventListener('click', event => { event.preventDefault(); updateRecoveryHelp(action); applyFileRecovery(action).catch(error => handleError(error)); }));
document.addEventListener('click', event => { const link = event.target.closest('.polarion-link'); if (!link) return; event.preventDefault(); if (invoke) invoke('open_external_url', { url: link.href }).catch(error => status(String(error), 'error')); else window.open(link.href, '_blank', 'noopener'); });
document.addEventListener('click', event => {
  const link = event.target.closest('.commit-server-link'); if (!link || !state.repository) return; event.preventDefault();
  const commitId = link.dataset.commitId;
  const subPath = link.dataset.submodulePath;
  const repositoryPath = subPath ? `${state.repository.path}/${subPath}` : state.repository.path;
  if (!invoke) return status(`Preview: open commit ${commitId.slice(0, 8)} on server`);
  invoke('open_commit_on_server', { repositoryPath, commitId }).catch(error => handleError(error));
});
refs.goUp.addEventListener('click', () => { const commander = state.view === 'commander'; const parts = (commander ? state.commanderPath : state.currentPath).split('/').filter(Boolean); parts.pop(); commander ? openCommanderDirectory(parts.join('/')) : openDirectory(parts.join('/')); });
refs.reloadFolder.addEventListener('click', () => state.view === 'commander' ? openCommanderDirectory(state.commanderPath) : openDirectory(state.currentPath, { force: true }));
document.addEventListener('keydown', event => { if (event.key !== 'Escape' || state.view !== 'commander' || document.querySelector('dialog[open]')) return; event.preventDefault(); returnToProjectNavigator(); });
refs.remoteRef.addEventListener('change', () => { state.remoteRef = refs.remoteRef.value; openCommanderDirectory(state.commanderPath); });
$('#closeSubmoduleMenu').addEventListener('click', () => { refs.submoduleMenu.hidden = true; });
document.querySelectorAll('[data-version-filter]').forEach(button => button.addEventListener('click', () => {
  versionFilter = button.dataset.versionFilter; document.querySelectorAll('[data-version-filter]').forEach(item => item.classList.toggle('active', item === button)); renderSubmoduleVersions();
}));
document.addEventListener('click', event => { if (!refs.submoduleMenu.hidden && !refs.submoduleMenu.contains(event.target) && !event.target.closest('[data-entry]') && !event.target.closest('[data-detail-action="versions"]')) refs.submoduleMenu.hidden = true; });
refs.commitScope.addEventListener('click', openScopeCommit);
refs.showPathHistory.addEventListener('click', showSelectedHistory);
refs.scopeCommitMessage.addEventListener('input', () => { refs.confirmScopeCommit.disabled = !refs.scopeCommitMessage.value.trim(); });
refs.confirmScopeCommit.addEventListener('click', commitSelectedScope);
$('#initRepo').addEventListener('click', async () => {
  if (!invoke) return refs.browserDialog.showModal();
  try { const path = await invoke('choose_folder'); if (path) { await invoke('init_repository', { path }); await loadRepository(path); } } catch (error) { handleError(error); }
});
$('#newBranch').addEventListener('click', async () => {
  if (!state.repository) return;
  // This button is easy to confuse with a per-submodule action when a
  // submodule happens to be selected — it always creates the branch (and
  // switches) on the MAIN project, never the submodule, so make that
  // unambiguous instead of silently doing the wrong-scope thing.
  if (state.selectedEntry?.kind === 'submodule') {
    const entry = state.selectedEntry;
    const onMain = await customConfirm(`"${entry.name}" (a submodule) is currently selected. This button creates the branch on the MAIN project — the whole project will switch to it. To create a branch inside the submodule instead, cancel this and use "＋ New branch…" in ${entry.name}'s own details panel.`, { title: 'Create branch — choose scope', okLabel: `Create on main project` });
    if (!onMain) return createSubmoduleBranch(entry);
  }
  openNewBranchDialog();
});

async function openNewBranchDialog() {
  refs.newBranchName.value = ''; refs.newBranchStatus.textContent = ''; refs.confirmNewBranch.disabled = true;
  refs.newBranchFrom.textContent = state.repository.current_branch || 'HEAD';
  refs.newBranchOriginStatus.textContent = 'Checking origin/main…';
  refs.newBranchDialog.showModal();
  refs.newBranchName.focus();
  if (!invoke) { refs.newBranchOriginStatus.textContent = 'In sync with origin/main.'; return; }
  try {
    const context = await invoke('branch_creation_context', { repositoryPath: state.repository.path });
    refs.newBranchFrom.textContent = `${context.current_branch} @ ${context.current_commit}`;
    if (!context.main_remote_branch) { refs.newBranchOriginStatus.textContent = 'No remote-tracking branch found — fetch first to compare.'; return; }
    if (context.ahead === 0 && context.behind === 0) { refs.newBranchOriginStatus.textContent = `✓ In sync with ${context.main_remote_branch} — the new branch will start from the latest.`; return; }
    const parts = [];
    if (context.ahead) parts.push(`${context.ahead} commit${context.ahead === 1 ? '' : 's'} ahead`);
    if (context.behind) parts.push(`${context.behind} commit${context.behind === 1 ? '' : 's'} behind`);
    refs.newBranchOriginStatus.textContent = `⚠ ${parts.join(', ')} of ${context.main_remote_branch} — the new branch starts from here, not from the latest ${context.main_remote_branch}.`;
  } catch (error) { refs.newBranchOriginStatus.textContent = String(error); }
}

refs.newBranchName.addEventListener('input', () => { refs.confirmNewBranch.disabled = !refs.newBranchName.value.trim(); });
refs.confirmNewBranch.addEventListener('click', async () => {
  const name = refs.newBranchName.value.trim(); if (!name) return;
  refs.confirmNewBranch.disabled = true; refs.confirmNewBranch.textContent = 'Creating…';
  try {
    await invoke('create_branch', { path: state.repository.path, branch: name });
    refs.newBranchDialog.close();
    await loadRepository(state.repository.path, { keepPath: true });
    status(`Branch "${name}" created`); showOperationToast(`Branch "${name}" created and checked out.`, 'success');
  } catch (error) { refs.newBranchStatus.textContent = String(error); refs.confirmNewBranch.disabled = false; }
  finally { refs.confirmNewBranch.textContent = 'Create branch'; }
});
refs.commitButton.addEventListener('click', async () => {
  try { const folder = state.changesScope === 'folder' ? state.currentPath : ''; const files = state.changes.filter(change => change.staged && (!folder || change.path === folder || change.path.startsWith(`${folder}/`))).map(change => change.path); await invoke('commit_files', { repositoryPath: state.repository.path, files, message: refs.commitMessage.value }); refs.commitMessage.value = ''; await loadRepository(state.repository.path); if (folder) await openDirectory(folder); }
  catch (error) { handleError(error); }
});

// Command Console — two tabs sharing one input:
//  - "Commands" is a context-aware palette over the app's own already-tested
//    actions (no shell access) — suggestions depend on where you are (a
//    submodule selected, a folder open…), each with a short explanation.
//  - "Git Console" is a real, persistent terminal-style transcript: every git
//    command you run (and its actual stdout/stderr) stays visible as a scrolling
//    log, with ↑↓ command history, and an explicit, always-visible, cyclable
//    scope pill so "runs on the current location" is never a guess.
function currentConsoleContext() {
  const entry = state.selectedEntry;
  if (!state.repository) return { label: 'No repository open', tags: ['no-repo'] };
  if (entry?.kind === 'submodule') return { label: `Submodule: ${entry.relative_path}`, tags: ['submodule', 'has-selection'] };
  if (entry) return { label: `Selected: ${entry.relative_path}`, tags: ['file', 'has-selection'] };
  if (state.view === 'graph') return { label: `Branch Map · ${state.repository.current_branch || 'detached'}`, tags: ['graph'] };
  if (state.view === 'commander') return { label: 'Local ↔ Remote', tags: ['commander'] };
  return { label: `${state.currentPath || state.repository.name} · branch ${state.repository.current_branch || 'detached'}`, tags: ['explorer'] };
}

function buildCommands() {
  const entry = state.selectedEntry;
  const list = [
    { id: 'open-repo', name: 'Open Repository', description: 'Choose a local Git folder to open', keys: 'Ctrl+O', tags: ['no-repo'], fn: openRepository },
    { id: 'new-branch', name: 'New Branch', description: 'Create a new local branch from the current one', keys: 'Ctrl+Shift+B', tags: ['explorer', 'graph'], fn: () => $('#newBranch').click() },
    { id: 'commit', name: 'Commit Changes', description: 'Open the Working tree drawer and write a commit message for staged files', keys: 'Ctrl+Shift+C', keywords: 'save record', tags: ['explorer'], fn: () => { state.changesScope = 'global'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); stageAllInScope(''); refs.commitMessage.focus(); } },
    { id: 'push', name: 'Push Current Branch', description: 'Send your local commits on this branch to the server', keys: 'Ctrl+Shift+P', tags: ['explorer', 'graph'], fn: () => $('#pushCurrent').click() },
    { id: 'pull', name: 'Pull Current Branch', description: 'Fetch and fast-forward the current branch from the server', keys: '', tags: ['explorer', 'graph'], fn: () => $('#pullCurrent').click() },
    { id: 'merge', name: 'Merge Branch…', description: 'Bring another branch\'s commits into your current one — stays local, resolves conflicts here if any', keys: '', keywords: 'combine join', tags: ['explorer', 'graph'], fn: () => state.repository && openMergeBranchDialog(mergeTargetForMain()) },
    { id: 'fetch', name: 'Fetch Remote', description: 'Download new commits/refs from the server without changing your branch', keys: 'Ctrl+Shift+F', tags: ['explorer', 'graph'], fn: () => $('#fetchCurrent').click() },
    { id: 'fetchall', name: 'Fetch All Remotes', description: 'Download new commits/refs from every configured remote, not just the first one', keywords: 'multiple upstream mirror', tags: ['explorer', 'graph'], fn: () => fetchAllRemotes() },
    { id: 'stash', name: 'Stash Work in Progress', description: 'Temporarily set aside uncommitted changes, restore them later', keys: 'Ctrl+Shift+S', tags: ['explorer'], fn: stashWork },
    { id: 'pop', name: 'Restore Stashed Work', description: 'Bring back the changes you last stashed', keys: '', tags: ['explorer'], fn: popStash },
    { id: 'conflicts', name: 'Resolve Merge Conflicts', description: 'Open the conflict resolution dialog for a merge in progress', keys: '', keywords: 'merge conflict resolve', tags: state.pendingMainConflicts?.length ? ['explorer', 'graph', 'relevant'] : [], fn: () => openConflictsDialog(mergeTargetForMain(), state.pendingMainConflicts || []) },
    { id: 'search', name: 'Search Repository', description: 'Filter the current view by name, author or commit id', keys: 'Ctrl+F', tags: ['explorer', 'graph', 'commander'], fn: () => refs.search.focus() },
    { id: 'explorer', name: 'Go to Project Explorer', description: 'Browse files, folders and submodules', keys: '', keywords: 'files browse', tags: [], fn: () => $('#navExplorer').click() },
    { id: 'commander', name: 'Go to Local ↔ Remote', description: 'Compare your working copy against a remote snapshot, file by file', keys: 'Ctrl+Shift+L', keywords: 'diff compare', tags: [], fn: () => $('#navCommander').click() },
    { id: 'graph', name: 'Go to Branch Map', description: 'See commit history and branches as a graph', keys: 'Ctrl+Shift+G', keywords: 'log history commits', tags: [], fn: () => $('#navGraph').click() },
    { id: 'remotes', name: 'Go to Remotes', description: 'View and fetch configured server locations', keys: '', tags: [], fn: () => $('#navRemotes').click() },
    { id: 'refresh', name: 'Refresh Repository', description: 'Re-read branches, commits and status from disk (e.g. after external Git commands)', keys: '', keywords: 'reload', tags: [], fn: () => $('#refresh').click() },
  ];
  if (entry?.kind === 'submodule') {
    list.push(
      { id: 'sub-pull', name: `Pull Submodule (${entry.name})`, description: 'Fast-forward this submodule from its own remote', tags: ['submodule', 'relevant'], keywords: 'submodule update', fn: () => pullSubmodule(entry) },
      { id: 'sub-push', name: `Push Submodule (${entry.name})`, description: 'Send this submodule\'s local commits to its own remote', tags: ['submodule', 'relevant'], fn: () => pushSubmodule(entry) },
      { id: 'sub-merge', name: `Merge Branch into Submodule (${entry.name})`, description: 'Bring another branch into this submodule\'s current branch', tags: ['submodule', 'relevant'], keywords: 'merge combine', fn: () => openMergeBranchDialog(mergeTargetForSubmodule(entry)) },
      { id: 'sub-commit', name: `Commit Submodule (${entry.name})`, description: 'Commit uncommitted changes inside this submodule', tags: ['submodule', 'relevant'], fn: () => commitSubmoduleChanges(entry) },
      { id: 'sub-version', name: `Change Submodule Version (${entry.name})`, description: 'Switch this submodule to a different branch, tag or commit', tags: ['submodule', 'relevant'], keywords: 'checkout switch branch', fn: () => openSubmoduleMenu(entry, innerWidth - 480, 110) },
      { id: 'sub-fetch', name: `Fetch Submodule (${entry.name})`, description: 'Download new commits for this submodule without changing its checkout', tags: ['submodule'], fn: () => fetchSubmodule(entry) },
      { id: 'sub-new-branch', name: `New Branch in Submodule (${entry.name})`, description: 'Create and switch to a new branch in this submodule, from its current commit', tags: ['submodule', 'relevant'], keywords: 'checkout create', fn: () => createSubmoduleBranch(entry) },
    );
  } else if (entry?.kind === 'file') {
    list.push(
      { id: 'file-edit', name: `Edit ${entry.name}`, description: 'Open this file in the built-in editor', tags: ['file', 'relevant'], fn: () => openEditor(entry) },
      { id: 'file-compare', name: `Compare ${entry.name} with Remote`, description: 'Side-by-side diff against the server version, with restore options', tags: ['file', 'relevant'], keywords: 'diff', fn: () => compareEntryWithRemote(entry) },
    );
  }
  return list;
}

let commandPaletteOpen = false;
let activeCommands = [];

function scoreCommand(cmd, query, tags) {
  const relevant = cmd.tags?.some(tag => tags.includes(tag)) ? 1 : 0;
  if (!query) return relevant;
  const haystack = `${cmd.name} ${cmd.description || ''} ${cmd.keywords || ''}`.toLowerCase();
  if (!haystack.includes(query)) return -1;
  const nameMatch = cmd.name.toLowerCase().startsWith(query) ? 2 : cmd.name.toLowerCase().includes(query) ? 1 : 0;
  return relevant * 10 + nameMatch;
}

// A word someone very plausibly typed expecting real git output (habit from a
// terminal) rather than an app-action search — offered as a one-click "run
// this as raw git instead" suggestion so forgetting the "$" prefix doesn't
// silently fall back to fuzzy-matching app commands by unrelated description
// text (e.g. "status" matching "Refresh Repository" because its description
// happens to mention "status").
// A small, plain-language git reference — not exhaustive, but covers what
// someone unfamiliar with git actually reaches for. Powers both the "run as
// git command" suggestion and the live "what comes next" flag hints while
// typing in the Git Console.
const GIT_COMMAND_HELP = {
  status: { description: 'Shows what changed in your working folder — modified, staged, untracked files.', flags: [
    { flag: '-s', desc: 'Short format — one compact line per file' },
    { flag: '-b', desc: 'Also show the current branch and how far ahead/behind it is' },
    { flag: '--ignored', desc: 'Also list files excluded by .gitignore' },
  ] },
  log: { description: 'Shows commit history, newest first.', flags: [
    { flag: '--oneline', desc: 'One short line per commit instead of the full message' },
    { flag: '-10', desc: 'Only the last 10 commits' },
    { flag: '--graph', desc: 'Draw the branch/merge lines in text form' },
    { flag: '--author=', desc: 'Only commits by a specific author' },
    { flag: '-- <path>', desc: 'Only commits that touched this file/folder' },
  ] },
  diff: { description: 'Shows the exact line-by-line changes that aren\'t committed yet.', flags: [
    { flag: '--staged', desc: 'Show only what\'s already staged (about to be committed)' },
    { flag: 'HEAD~1', desc: 'Compare against the previous commit instead of the working copy' },
    { flag: '-- <path>', desc: 'Limit the diff to one file/folder' },
  ] },
  add: { description: 'Stages a file — marks it to be included in the next commit.', flags: [
    { flag: '.', desc: 'Stage everything changed in the current folder and below' },
    { flag: '-A', desc: 'Stage everything in the whole repository, including deletions' },
    { flag: '-p', desc: 'Choose which parts (hunks) of a file to stage, interactively' },
  ] },
  commit: { description: 'Records the currently staged changes as a new commit.', flags: [
    { flag: '-m ""', desc: 'Provide the commit message inline instead of opening an editor' },
    { flag: '-am ""', desc: 'Stage every already-tracked modified file AND commit, in one step' },
    { flag: '--amend', desc: 'Edit the message/contents of the last commit instead of making a new one' },
  ] },
  branch: { description: 'Lists, creates, or deletes branches.', flags: [
    { flag: '-a', desc: 'List local AND remote-tracking branches' },
    { flag: '-d <name>', desc: 'Delete a branch that\'s already merged (safe)' },
    { flag: '-D <name>', desc: '⚠ Force-delete a branch even if unmerged (can lose commits)' },
    { flag: '-m <new>', desc: 'Rename the current branch' },
  ] },
  checkout: { description: 'Switches branches, or restores files to a previous state.', flags: [
    { flag: '-b <name>', desc: 'Create a new branch and switch to it in one step' },
    { flag: '-- <path>', desc: '⚠ Discard local edits to this file, restoring it from the last commit' },
  ] },
  switch: { description: 'Switches to a different branch (the modern, safer alternative to checkout).', flags: [
    { flag: '-c <name>', desc: 'Create a new branch and switch to it' },
  ] },
  merge: { description: 'Brings another branch\'s commits into the one you\'re on.', flags: [
    { flag: '--no-ff', desc: 'Always create a merge commit, even if a fast-forward is possible' },
    { flag: '--abort', desc: 'Cancel a merge that has conflicts, back to the pre-merge state' },
  ] },
  rebase: { description: '⚠ Replays your commits on top of another branch, rewriting history. Avoid on commits you\'ve already pushed/shared.', flags: [
    { flag: '--abort', desc: 'Cancel an in-progress rebase, back to how it was before' },
    { flag: '--continue', desc: 'Continue after resolving a conflict' },
  ] },
  reset: { description: 'Moves the current branch pointer — how destructive depends on the flag.', flags: [
    { flag: 'HEAD -- <path>', desc: 'Unstage a file, keep its edits on disk (safe)' },
    { flag: '--soft HEAD~1', desc: 'Undo the last commit, keep everything staged' },
    { flag: '--hard HEAD', desc: '⚠ Discard ALL local edits and staged changes — cannot be undone' },
  ] },
  clean: { description: '⚠ Deletes untracked files from disk — not recoverable from git afterward.', flags: [
    { flag: '-n', desc: 'Dry run — show what WOULD be deleted, without deleting anything' },
    { flag: '-fd', desc: '⚠ Actually delete untracked files AND untracked folders' },
  ] },
  remote: { description: 'Lists or manages configured remotes (like "origin").', flags: [
    { flag: '-v', desc: 'Show the fetch/push URLs for each remote' },
    { flag: 'add <name> <url>', desc: 'Add a new remote' },
  ] },
  fetch: { description: 'Downloads new commits/branches from a remote, without changing your files.', flags: [
    { flag: '--all', desc: 'Fetch from every configured remote' },
    { flag: '--prune', desc: 'Also remove local references to branches deleted on the remote' },
  ] },
  pull: { description: 'Fetches from a remote AND merges/fast-forwards into your current branch.', flags: [
    { flag: '--ff-only', desc: 'Only proceed if it can fast-forward — refuse if it would need a real merge' },
    { flag: '--rebase', desc: 'Replay your local commits on top instead of merging' },
  ] },
  push: { description: 'Uploads your local commits to a remote branch.', flags: [
    { flag: '-u origin <branch>', desc: 'Push and remember this as the branch\'s default upstream' },
    { flag: '--force-with-lease', desc: '⚠ Overwrite the remote branch, but refuse if someone else pushed since your last fetch' },
    { flag: '--force', desc: '⚠⚠ Overwrite the remote branch unconditionally — can destroy others\' work' },
  ] },
  show: { description: 'Shows the full details (message + diff) of one commit.', flags: [
    { flag: 'HEAD', desc: 'Show the most recent commit' },
    { flag: '--stat', desc: 'Show only which files changed and by how much, not the full diff' },
  ] },
  blame: { description: 'Shows who last changed each line of a file, and in which commit.', flags: [
    { flag: '-- <path>', desc: 'The file to blame (required)' },
  ] },
  tag: { description: 'Lists or creates tags (named pointers to a specific commit).', flags: [
    { flag: '-a <name> -m ""', desc: 'Create an annotated tag with a message' },
    { flag: '-d <name>', desc: 'Delete a local tag' },
  ] },
  stash: { description: 'Temporarily sets aside uncommitted changes so your working folder is clean.', flags: [
    { flag: 'list', desc: 'Show all saved stashes' },
    { flag: 'pop', desc: 'Restore the most recent stash and remove it from the list' },
    { flag: 'drop', desc: 'Delete the most recent stash without restoring it' },
  ] },
  reflog: { description: 'Shows a log of everywhere HEAD has pointed — a safety net to recover "lost" commits.', flags: [] },
  submodule: { description: 'Manages submodules (nested repositories inside this one).', flags: [
    { flag: 'status', desc: 'Show the commit each submodule is on, and whether it\'s changed' },
    { flag: 'update --init --recursive', desc: 'Fetch and check out every submodule at the commit the parent expects' },
    { flag: 'foreach "<cmd>"', desc: 'Run a shell command inside every submodule' },
  ] },
  'ls-files': { description: 'Lists files git is tracking.', flags: [] },
};
const GIT_SUBCOMMAND_HINTS = new Set(Object.keys(GIT_COMMAND_HELP));

function renderCommandList(query) {
  const ctx = currentConsoleContext();
  const q = query.trim().toLowerCase();
  const scored = activeCommands.map(cmd => ({ cmd, score: scoreCommand(cmd, q, ctx.tags) })).filter(entry => entry.score >= 0);
  scored.sort((a, b) => b.score - a.score);
  const list = $('#commandList');
  // Recognize both "add ." and "git add ." — either way of typing it should
  // surface the "run as raw git" suggestion.
  const words = q.split(/\s+/).filter(Boolean);
  const gitWords = words[0] === 'git' ? words.slice(1) : words;
  const gitArgsText = words[0] === 'git' ? query.trim().replace(/^git\s+/i, '') : query.trim();
  const gitHint = gitWords[0] && GIT_SUBCOMMAND_HINTS.has(gitWords[0]) ? `<div class="command-item selected raw-git-item" data-raw-git-suggest="${esc(gitArgsText)}">
    <div class="command-item-head"><span class="command-name">▸ Run as git command: git ${esc(gitArgsText)}</span><span class="command-keys">Enter</span></div>
    <span class="command-desc">Looks like a git subcommand — run it directly and see real output</span>
  </div>` : '';
  list.innerHTML = gitHint + (scored.map(({ cmd, score }, i) => `<div class="command-item ${i === 0 && !gitHint ? 'selected' : ''} ${score >= 10 ? 'relevant' : ''}" data-cmd-id="${esc(cmd.id)}">
    <div class="command-item-head"><span class="command-name">${esc(cmd.name)}</span>${cmd.keys ? `<span class="command-keys">${esc(cmd.keys)}</span>` : ''}</div>
    <span class="command-desc">${esc(cmd.description || '')}</span>
  </div>`).join('') || (gitHint ? '' : '<div class="command-item" style="text-align:center;color:#6b7f96;">No matching commands</div>'));
}

// ---- Git Console — persistent terminal-style transcript -------------------
// Real freedom to run any git command, deliberately kept separate from the
// app's own tested actions above (which never touch a shell) so the two are
// never confused with each other.
const RAW_GIT_PREFIX = '$';
function isRawGitQuery(query) { return query.trimStart().startsWith(RAW_GIT_PREFIX); }
function rawGitArgs(query) { return query.trimStart().slice(1).trim(); }
const DESTRUCTIVE_GIT_PATTERN = /(^|\s)(reset\s+--hard|clean\s+-[a-z]*f|push\s+.*(--force|-f\b)|branch\s+-D|checkout\s+.*-f\b|rebase|filter-branch|gc\s+--prune|update-ref\s+-d)/i;

// Every place a git command could plausibly run right now — always at least
// "repository root"; "current folder" and "selected submodule" are added only
// when they actually apply. Explicit and cyclable (⇄ scope button) instead of
// a silent guess, so "does this run where I think it runs" is never in doubt.
function consoleAvailableScopes() {
  if (!state.repository) return [];
  const scopes = [{ key: 'root', path: state.repository.path, label: `${state.repository.name} (repository root)` }];
  if (state.currentPath) scopes.push({ key: 'folder', path: `${state.repository.path}/${state.currentPath}`, label: `${state.currentPath} (current folder)` });
  if (state.selectedEntry?.kind === 'submodule') scopes.push({ key: 'submodule', path: `${state.repository.path}/${state.selectedEntry.relative_path}`, label: `${state.selectedEntry.relative_path} (selected submodule)` });
  return scopes;
}
function consoleDefaultScopeKey() {
  if (state.selectedEntry?.kind === 'submodule') return 'submodule';
  if (state.currentPath) return 'folder';
  return 'root';
}
function consoleGitTarget() {
  const scopes = consoleAvailableScopes();
  if (!scopes.length) return { key: 'root', path: '', label: 'No repository open' };
  const key = scopes.some(s => s.key === state.consoleScopeOverride) ? state.consoleScopeOverride : consoleDefaultScopeKey();
  return scopes.find(s => s.key === key) || scopes[0];
}
function cycleConsoleScope() {
  const scopes = consoleAvailableScopes(); if (scopes.length < 2) return;
  const idx = scopes.findIndex(s => s.key === consoleGitTarget().key);
  state.consoleScopeOverride = scopes[(idx + 1) % scopes.length].key;
  updateConsoleScopeLabel();
}
function updateConsoleScopeLabel() { $('#commandScope').textContent = state.repository ? `📍 ${consoleGitTarget().label}` : ''; }

// Lightly colorizes raw git output so the shape of the answer is clear at a
// glance — added/removed diff lines, status sections, commit hashes — the
// same way a real terminal with git's own color output would look, without
// needing to parse or understand the command itself.
function colorizeGitOutput(text) {
  return text.split('\n').map(line => {
    const escaped = esc(line);
    if (/^\+\+\+ /.test(line) || /^--- /.test(line)) return `<span class="git-out-meta">${escaped}</span>`;
    if (/^\+/.test(line)) return `<span class="git-out-add">${escaped}</span>`;
    if (/^-/.test(line)) return `<span class="git-out-del">${escaped}</span>`;
    if (/^@@.*@@/.test(line)) return `<span class="git-out-hunk">${escaped}</span>`;
    if (/^(diff --git|index [0-9a-f])/.test(line)) return `<span class="git-out-meta">${escaped}</span>`;
    if (/^\s*(modified|new file|deleted|renamed|copied):/.test(line)) return `<span class="git-out-changed">${escaped}</span>`;
    if (/^\s*\(use "git/.test(line)) return `<span class="git-out-hint">${escaped}</span>`;
    if (/^(On branch |Your branch)/.test(line)) return `<span class="git-out-branch">${escaped}</span>`;
    if (/^(Untracked files:|Changes (to be committed|not staged for commit):)/.test(line)) return `<span class="git-out-section">${escaped}</span>`;
    if (/^[0-9a-f]{7,40}\b/.test(line)) return `<span class="git-out-hash">${escaped}</span>`;
    return escaped;
  }).join('\n');
}

function renderConsoleTranscript() {
  const list = $('#commandList'); list.classList.add('console-transcript');
  list.innerHTML = state.consoleTranscript.map(entry => `<div class="raw-git-output">
    <div class="raw-git-cmd">$ git ${esc(entry.args)} <span class="raw-git-cwd">(in ${esc(entry.targetLabel)})</span> <b class="raw-git-status ${entry.result.success ? 'ok' : 'fail'}">${entry.result.success ? 'OK' : 'FAILED'}</b></div>
    ${entry.result.stdout ? `<pre class="raw-git-stdout">${colorizeGitOutput(entry.result.stdout)}</pre>` : ''}
    ${entry.result.stderr ? `<pre class="raw-git-stderr">${colorizeGitOutput(entry.result.stderr)}</pre>` : ''}
    ${!entry.result.stdout && !entry.result.stderr ? '<div class="raw-git-empty">(no output)</div>' : ''}
  </div>`).join('') || '<div class="console-empty">Type a git command below and press Enter — e.g. "status", "log --oneline -10", "diff HEAD~1".</div>';
  $('#commandClearTranscript').hidden = state.consoleTranscript.length === 0;
  list.scrollTop = list.scrollHeight;
}

let consoleHistoryPointer = -1;
function recallConsoleHistory(direction) {
  const history = state.consoleCmdHistory; if (!history.length) return;
  if (consoleHistoryPointer === -1) consoleHistoryPointer = history.length;
  consoleHistoryPointer = Math.max(0, Math.min(history.length - 1, consoleHistoryPointer + direction));
  const input = $('#commandInput'); input.value = history[consoleHistoryPointer]; input.setSelectionRange(input.value.length, input.value.length);
}

async function runRawGitFromConsole(args) {
  if (!args) return;
  setConsoleMode('console');
  if (!state.repository) { state.consoleTranscript.push({ args, targetLabel: '—', result: { success: false, stdout: '', stderr: 'Open a repository first.' } }); renderConsoleTranscript(); return; }
  if (DESTRUCTIVE_GIT_PATTERN.test(args)) {
    const target = consoleGitTarget();
    const ok = await customConfirm(`This looks like a destructive command: "git ${args}" in ${target.label}. It can permanently discard commits, branches or uncommitted work. Continue?`, { title: 'Destructive git command', danger: true, okLabel: 'Run it anyway' });
    if (!ok) return;
  }
  state.consoleCmdHistory.push(args); consoleHistoryPointer = -1;
  const target = consoleGitTarget();
  if (!invoke) { state.consoleTranscript.push({ args, targetLabel: target.label, result: { success: true, stdout: '(preview mode — not actually run)', stderr: '' } }); renderConsoleTranscript(); return; }
  try {
    const result = await invoke('run_git_command', { repositoryPath: target.path, args });
    state.consoleTranscript.push({ args, targetLabel: target.label, result });
    renderConsoleTranscript();
    directoryCache.clear(); await loadRepository(state.repository.path, { keepPath: true });
  } catch (error) { state.consoleTranscript.push({ args, targetLabel: target.label, result: { success: false, stdout: '', stderr: String(error) } }); renderConsoleTranscript(); }
}

// Live "what comes next" helper while typing in the Git Console — shows the
// recognized subcommand's plain-language description plus its common flags
// (click to append), or, while still typing the subcommand itself, matching
// subcommand names to autocomplete. Aimed squarely at someone who doesn't
// already have git's flags memorized.
// Extra subcommand names worth typo-correcting even though they don't have
// their own flag reference above.
const KNOWN_GIT_SUBCOMMANDS = [...Object.keys(GIT_COMMAND_HELP), 'init', 'clone', 'config', 'describe', 'worktree', 'revert', 'bisect', 'cherry-pick', 'archive', 'rm', 'mv', 'gc', 'apply'];

function levenshtein(a, b) {
  const dp = Array.from({ length: a.length + 1 }, (_, i) => [i, ...Array(b.length).fill(0)]);
  for (let j = 0; j <= b.length; j++) dp[0][j] = j;
  for (let i = 1; i <= a.length; i++) for (let j = 1; j <= b.length; j++) {
    dp[i][j] = a[i - 1] === b[j - 1] ? dp[i - 1][j - 1] : 1 + Math.min(dp[i - 1][j], dp[i][j - 1], dp[i - 1][j - 1]);
  }
  return dp[a.length][b.length];
}
function closestGitSubcommand(word) {
  if (word.length < 2) return null;
  let best = null; let bestDist = Infinity;
  for (const name of KNOWN_GIT_SUBCOMMANDS) {
    const dist = levenshtein(word, name);
    if (dist < bestDist) { bestDist = dist; best = name; }
  }
  // Only offer it as a correction when it's plausibly a typo, not just any
  // vaguely-similar word — scales with length so short typos still match.
  return bestDist <= Math.max(1, Math.ceil(word.length / 3)) ? best : null;
}

function renderGitHints() {
  const box = $('#commandGitHints');
  if (state.consoleMode !== 'console') { box.hidden = true; return; }
  const raw = $('#commandInput').value;
  const words = raw.split(/\s+/).filter(Boolean);
  const sub = words[0]?.toLowerCase();
  if (!sub) { box.hidden = true; return; }
  const entry = GIT_COMMAND_HELP[sub];
  if (!entry) {
    const matches = words.length === 1 ? Object.keys(GIT_COMMAND_HELP).filter(name => name.startsWith(sub)) : [];
    if (matches.length) {
      box.hidden = false;
      box.innerHTML = `<div class="hint-subcommands">${matches.map(name => `<button type="button" class="hint-sub" data-fill-sub="${esc(name)}">${esc(name)}</button>`).join('')}</div>`;
      return;
    }
    // Not a known subcommand, and not a prefix of one either — check for a typo.
    const suggestion = !KNOWN_GIT_SUBCOMMANDS.includes(sub) ? closestGitSubcommand(sub) : null;
    if (suggestion) {
      const corrected = [suggestion, ...words.slice(1)].join(' ');
      box.hidden = false;
      box.innerHTML = `<div class="hint-typo">Did you mean <button type="button" class="hint-sub" data-fill-sub="${esc(corrected)}">git ${esc(corrected)}</button>?</div>`;
      return;
    }
    box.hidden = true; return;
  }
  const already = new Set(words.slice(1).map(w => w.split('=')[0]));
  const flags = entry.flags.filter(f => !already.has(f.flag.split(/[\s=]/)[0]));
  box.hidden = false;
  box.innerHTML = `<p class="hint-desc"><b>git ${esc(sub)}</b> — ${esc(entry.description)}</p>${flags.length ? `<div class="hint-flags">${flags.map(f => `<button type="button" class="hint-flag" data-append-flag="${esc(f.flag)}"><b>${esc(f.flag)}</b><span>${esc(f.desc)}</span></button>`).join('')}</div>` : ''}`;
}
$('#commandGitHints').addEventListener('click', (e) => {
  const fillSub = e.target.closest('[data-fill-sub]');
  if (fillSub) { $('#commandInput').value = `${fillSub.dataset.fillSub} `; $('#commandInput').focus(); renderGitHints(); return; }
  const appendFlag = e.target.closest('[data-append-flag]');
  if (appendFlag) {
    const input = $('#commandInput');
    const sep = input.value === '' || input.value.endsWith(' ') ? '' : ' ';
    input.value = `${input.value}${sep}${appendFlag.dataset.appendFlag} `;
    input.focus(); renderGitHints();
  }
});

function setConsoleMode(mode) {
  state.consoleMode = mode;
  document.querySelectorAll('.command-tab').forEach(tab => tab.classList.toggle('active', tab.dataset.mode === mode));
  const input = $('#commandInput');
  if (mode === 'console') {
    input.placeholder = 'Type a git command… (no "$" needed here)'; input.value = '';
    $('#commandHelp').textContent = '↑↓ command history · Enter to run · "⇄ scope" to change where it runs · Esc to close';
    updateConsoleScopeLabel(); renderConsoleTranscript(); renderGitHints();
  } else {
    input.placeholder = 'Type a command… (Cmd/Ctrl+K)';
    $('#commandHelp').textContent = '↑↓ to navigate · Enter to run · Esc to close';
    $('#commandScope').textContent = ''; $('#commandList').classList.remove('console-transcript'); $('#commandGitHints').hidden = true; renderCommandList('');
  }
}

function openCommandPalette() {
  const input = $('#commandInput');
  commandPaletteOpen = true;
  activeCommands = buildCommands();
  input.value = '';
  setConsoleMode('commands');
  $('#commandPalette').showModal();
  input.focus();
}
function filterCommands(query) {
  if (isRawGitQuery(query)) { setConsoleMode('console'); $('#commandInput').value = rawGitArgs(query); renderGitHints(); return; }
  renderCommandList(query);
}
$('#commandInput').addEventListener('input', (e) => { if (state.consoleMode === 'console') { consoleHistoryPointer = -1; renderGitHints(); return; } filterCommands(e.target.value); });
$('#commandInput').addEventListener('keydown', (e) => {
  if (state.consoleMode === 'console') {
    if (e.key === 'Enter') { e.preventDefault(); const args = $('#commandInput').value.trim(); if (args) { $('#commandInput').value = ''; renderGitHints(); runRawGitFromConsole(args); } }
    else if (e.key === 'ArrowUp') { e.preventDefault(); recallConsoleHistory(-1); renderGitHints(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); recallConsoleHistory(1); renderGitHints(); }
    return;
  }
  const items = Array.from($('#commandList').querySelectorAll('.command-item'));
  const selected = items.find(i => i.classList.contains('selected'));
  if (e.key === 'ArrowDown') { e.preventDefault(); const next = selected?.nextElementSibling || items[0]; items.forEach(i => i.classList.remove('selected')); next?.classList.add('selected'); next?.scrollIntoView({ block: 'nearest' }); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); const prev = selected?.previousElementSibling || items[items.length - 1]; items.forEach(i => i.classList.remove('selected')); prev?.classList.add('selected'); prev?.scrollIntoView({ block: 'nearest' }); }
  else if (e.key === 'Enter') {
    e.preventDefault();
    if (selected?.dataset.rawGitSuggest !== undefined) { runRawGitFromConsole(selected.dataset.rawGitSuggest); return; }
    const cmd = activeCommands.find(c => c.id === selected?.dataset.cmdId); if (cmd) { $('#commandPalette').close(); cmd.fn(); commandPaletteOpen = false; }
  }
});
$('#commandList').addEventListener('click', (e) => {
  const suggest = e.target.closest('[data-raw-git-suggest]');
  if (suggest) { runRawGitFromConsole(suggest.dataset.rawGitSuggest); return; }
  const item = e.target.closest('.command-item');
  if (item && item.dataset.cmdId) {
    const cmd = activeCommands.find(c => c.id === item.dataset.cmdId);
    if (cmd) { $('#commandPalette').close(); cmd.fn(); commandPaletteOpen = false; }
  }
});
document.querySelectorAll('.command-tab').forEach(tab => tab.addEventListener('click', () => { setConsoleMode(tab.dataset.mode); $('#commandInput').focus(); }));
$('#commandScopeCycle').addEventListener('click', () => { cycleConsoleScope(); if (state.consoleMode === 'console') renderConsoleTranscript(); });
$('#commandClearTranscript').addEventListener('click', () => { state.consoleTranscript = []; renderConsoleTranscript(); });
$('#commandPalette').addEventListener('close', () => { commandPaletteOpen = false; });
$('#openConsole').addEventListener('click', () => openCommandPalette());
$('#openHelp').addEventListener('click', () => $('#helpDialog').showModal());
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === 'k') { e.preventDefault(); commandPaletteOpen ? $('#commandPalette').close() : openCommandPalette(); }
  else if ((e.ctrlKey || e.metaKey) && e.key === 'o') { e.preventDefault(); openRepository(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'C')) { e.preventDefault(); state.changesScope = 'global'; applyDefaultCommitMessage(); renderChanges(); refs.changesDrawer.classList.add('open'); stageAllInScope(''); refs.commitMessage.focus(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'P')) { e.preventDefault(); $('#pushCurrent').click(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'F')) { e.preventDefault(); $('#fetchCurrent').click(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'S')) { e.preventDefault(); state.hasStash ? popStash() : stashWork(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'L')) { e.preventDefault(); $('#navCommander').click(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'G')) { e.preventDefault(); $('#navGraph').click(); }
  else if ((e.ctrlKey || e.metaKey) && e.key === 'f' && !['input','textarea'].includes(document.activeElement.tagName.toLowerCase())) { e.preventDefault(); refs.search.focus(); }
});


if (!invoke) {
  refs.browserNotice.hidden = false;
  if (new URLSearchParams(location.search).has('demo')) Object.assign(state, previewData);
}
renderRecentRepos();
render();
