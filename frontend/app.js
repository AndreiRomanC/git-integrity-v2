const invoke = window.__TAURI__?.core?.invoke;
const $ = selector => document.querySelector(selector);
const palette = ['#58a6ff', '#f0b65a', '#b294ff', '#48cc7e'];
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

const state = { repository: null, branches: [], commits: [], allCommits: [], changes: [], selectedCommit: null, view: 'explorer', currentPath: '', entries: [], selectedEntry: null, historyScope: '', commanderPath: '', commanderRows: [], remoteRef: '', remotes: [], graphContext: null, editingPath: '', editorOriginal: '', publish: null, changesScope: 'global', commanderFocus: '', comparingRow: null, hasStash: false };
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
  commitButton: $('#commitButton'), selectionText: $('#selectionText'), browserDialog: $('#browserDialog'),
  browserNotice: $('#browserNotice'), explorerView: $('#explorerView'), fileList: $('#fileList'),
  breadcrumbs: $('#breadcrumbs'), viewTitle: $('#viewTitle'), goUp: $('#goUp'), reloadFolder: $('#reloadFolder'),
  submoduleMenu: $('#submoduleMenu'), submoduleVersions: $('#submoduleVersions'), submoduleMenuName: $('#submoduleMenuName'), currentSubmoduleVersion: $('#currentSubmoduleVersion'),
  commitScope: $('#commitScope'), showPathHistory: $('#showPathHistory'), commitScopeDialog: $('#commitScopeDialog'), commitScopeName: $('#commitScopeName'), scopeCommitMessage: $('#scopeCommitMessage'), confirmScopeCommit: $('#confirmScopeCommit'),
  commanderView: $('#commanderView'), commanderRows: $('#commanderRows'), commanderBreadcrumbs: $('#commanderBreadcrumbs'), remoteRef: $('#remoteRef'), compareDialog: $('#compareDialog'), compareTitle: $('#compareTitle'), compareSubtitle: $('#compareSubtitle'), localCompare: $('#localCompare'), remoteCompare: $('#remoteCompare'),
  remotesView: $('#remotesView'), remoteCards: $('#remoteCards'), editorDialog: $('#editorDialog'), editorTitle: $('#editorTitle'), editorPath: $('#editorPath'), editorContent: $('#editorContent'), locationRepository: $('#locationRepository'), locationBranch: $('#locationBranch'), locationPath: $('#locationPath'), leaveSubmoduleGraph: $('#leaveSubmoduleGraph'), publishDialog: $('#publishDialog'), publishBranch: $('#publishBranch'), publishRemote: $('#publishRemote'), publishCommits: $('#publishCommits'), publishSummary: $('#publishSummary'), publishDestination: $('#publishDestination'), publishBadge: $('#publishBadge'), publishSubtitle: $('#publishSubtitle'), cloneDialog: $('#cloneDialog'), cloneUrl: $('#cloneUrl'), cloneParent: $('#cloneParent'), cloneName: $('#cloneName'), confirmClone: $('#confirmClone'), submoduleDialog: $('#submoduleDialog'), submoduleUrl: $('#submoduleUrl'), submoduleParent: $('#submoduleParent'), submoduleName: $('#submoduleName'), submoduleUsername: $('#submoduleUsername'), submoduleToken: $('#submoduleToken'), submoduleAddStatus: $('#submoduleAddStatus'), confirmAddSubmodule: $('#confirmAddSubmodule'), operationToast: $('#operationToast'), drawerScopeTitle: $('#drawerScopeTitle')
};

function esc(value = '') { return String(value).replace(/[&<>'"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;',"'":'&#39;','"':'&quot;'}[c])); }
function commitSubjectHtml(subject = '') { let cursor = 0; const parts = []; for (const match of subject.matchAll(/P:(\d+)/g)) { parts.push(esc(subject.slice(cursor, match.index))); const id = match[1]; parts.push(`<a class="polarion-link" href="https://www.polarion.com/ID-${id}" target="_blank" rel="noreferrer" title="Open Polarion ID-${id}">${esc(match[0])}</a>`); cursor = match.index + match[0].length; } parts.push(esc(subject.slice(cursor))); return parts.join(''); }
function status(message, kind = '') { refs.statusText.textContent = message; refs.statusDot.className = `status-dot ${kind}`; }
let toastTimer; function showOperationToast(message, kind = '') { clearTimeout(toastTimer); refs.operationToast.textContent = message; refs.operationToast.className = `operation-toast ${kind}`; refs.operationToast.hidden = false; toastTimer = setTimeout(() => { refs.operationToast.hidden = true; }, 7000); }

function handleError(error) {
  const msg = String(error).toLowerCase();
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

async function loadRepository(path) {
  status('Reading repository…', 'busy');
  try {
    const data = await invoke('load_repository', { path });
    directoryCache.clear(); Object.assign(state, data); state.allCommits = data.commits; state.historyScope = ''; state.view = 'explorer'; state.commanderPath = ''; state.commanderRows = [];
    state.remoteRef = data.branches.find(branch => branch.remote)?.name || ''; await openDirectory(''); status(`${data.commits.length} commits loaded`);
    addRecentRepo(path, data.repository.name);
    updatePublishIndicator();
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
  const descriptions = { remote: `replace the working file with the snapshot from ${state.remoteRef}; staging is not changed`, head: 'discard working-file edits and restore the last local commit (HEAD)', stage: 'add the current working-file content to the staging area', unstage: 'reset only the staging-area entry to HEAD while keeping working-file edits' };
  if (['remote','head'].includes(action) && !await customConfirm(`This will ${descriptions[action]} for ${row.relative_path}. Continue?`, { title: action === 'head' ? 'Hard reset' : 'Restore from remote', danger: true, okLabel: 'Continue' })) return;
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
function updateRecoveryHelp(action = '') { const help = { stage: '`git add` – Stage the current file content', unstage: 'SOFT/MIXED RESET (`git restore --staged`) – unstages only, your edits on disk stay exactly as they are', head: 'HARD RESET (`git checkout HEAD -- file`) – permanently discards all edits, file becomes identical to the last commit', remote: `Fetch from ${state.remoteRef || 'remote'} then HARD RESET from it – overwrites the file on disk with the remote version` }; if (action) { $('#recoveryHelp').textContent = help[action]; } else { const allOptions = `Stage: ${help.stage} • Soft/Mixed reset: ${help.unstage} • Hard reset: ${help.head} • Remote: ${help.remote}`; $('#recoveryHelp').textContent = allOptions; } }

function renderComparisonContents(localText, remoteText) {
  const localLines = localText.split('\n'); const remoteLines = remoteText.split('\n'); const count = Math.max(localLines.length, remoteLines.length);
  const renderSide = (lines, other, remoteSide) => Array.from({ length: count }, (_, index) => {
    const line = lines[index] ?? ''; const different = line !== (other[index] ?? '');
    return `<span class="${different ? `diff-line${remoteSide ? ' remote-line' : ''}` : 'same-line'}"><i class="line-number">${index + 1}</i>${esc(line) || ' '}</span>`;
  }).join('');
  refs.localCompare.innerHTML = renderSide(localLines, remoteLines, false); refs.remoteCompare.innerHTML = renderSide(remoteLines, localLines, true);
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
  if (entry.status) return `<span class="git-state changed"><i class="git-dot"></i>${esc(labels[entry.status] || entry.status)}</span>`;
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

function renderExplorer() {
  if (!state.repository) return;
  renderBreadcrumbs();
  const query = refs.search.value.trim().toLowerCase();
  const entries = state.entries.filter(entry => !query || entry.name.toLowerCase().includes(query));
  const upRow = state.currentPath ? `<button class="file-row file-grid up-row" data-go-up="1">
    <span class="file-main"><span class="entry-icon folder">▲</span><span class="entry-copy"><span class="entry-name">..</span><span class="entry-hint">Parent folder</span></span></span>
    <span></span><span></span><span></span>
  </button>` : '';
  refs.fileList.innerHTML = upRow + entries.map(entry => `<button class="file-row file-grid ${entry.status || !entry.tracked ? 'has-change' : ''} ${state.selectedEntry?.relative_path === entry.relative_path ? 'selected' : ''}" data-entry="${esc(entry.relative_path)}">
    <span class="file-main">${iconFor(entry)}<span class="entry-copy"><span class="entry-name">${esc(entry.name)}${entry.kind === 'submodule' ? '<b class="inline-submodule-badge">SUBMODULE</b>' : ''}</span><span class="entry-hint">${entry.kind === 'submodule' ? 'Independent Git repository' : entry.kind}</span></span>${['folder','submodule'].includes(entry.kind) ? '<span class="folder-arrow">›</span>' : ''}</span>
    ${gitState(entry)}<span class="file-size">${entry.kind === 'file' ? formatSize(entry.size) : '—'}</span><span class="file-modified">${formatModified(entry.modified)}</span>
  </button>`).join('') || (state.currentPath ? '' : '<div class="empty-change">This folder is empty</div>');
  refs.fileList.querySelector('[data-go-up]')?.addEventListener('click', () => { const parent = state.currentPath.split('/').slice(0, -1).join('/'); openDirectory(parent); });
  refs.fileList.querySelectorAll('[data-entry]').forEach(row => {
    row.addEventListener('click', () => selectEntry(row.dataset.entry));
    row.addEventListener('dblclick', () => { const entry = state.entries.find(item => item.relative_path === row.dataset.entry); if (entry && ['folder','submodule'].includes(entry.kind)) openDirectory(entry.relative_path); });
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
    const folder = state.currentPath; const data = await invoke('load_repository', { path: state.repository.path });
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

function openScopeCommit() {
  const scope = selectedScope(); refs.commitScopeName.textContent = scope.name; refs.scopeCommitMessage.value = ''; refs.confirmScopeCommit.disabled = true; refs.commitScopeDialog.showModal(); refs.scopeCommitMessage.focus();
}

async function commitSelectedScope(event) {
  event.preventDefault(); const scope = selectedScope(); const message = refs.scopeCommitMessage.value.trim(); if (!message) return;
  if (!invoke) { refs.commitScopeDialog.close(); status(`Preview: committed ${scope.name}`); return; }
  refs.confirmScopeCommit.disabled = true; refs.confirmScopeCommit.textContent = 'Committing…';
  try {
    await invoke('commit_path', { repositoryPath: state.repository.path, relativePath: scope.path, message });
    const folder = state.currentPath; refs.commitScopeDialog.close(); const data = await invoke('load_repository', { path: state.repository.path });
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
    ${entry.status || !entry.tracked ? `<div class="local-change-banner"><i></i><div><strong>${entry.tracked ? 'Modified locally' : 'New local file'}</strong><span>This item differs from the committed repository state.</span></div></div>` : ''}
    <div class="context-actions">${entry.kind === 'file' ? '<button data-detail-action="edit">Edit local file</button>' : '<button data-detail-action="open">Open folder</button>'}<button data-detail-action="server">Open on server ↗</button><button data-detail-action="history">View history</button>${entry.status ? '<button data-detail-action="commit">Commit this item</button>' : ''}${entry.kind === 'file' && (entry.status || !entry.tracked) ? `<button data-detail-action="stage" data-tooltip="git add — add this file's current content to staging">＋ Stage this file</button><button data-detail-action="unstage" data-tooltip="Soft/Mixed reset — git restore --staged. Unstages only; your edits on disk are kept exactly as they are.">− Soft/Mixed reset (unstage)</button><button data-detail-action="head" class="danger-action-soft" data-tooltip="Hard reset — git checkout HEAD -- file. Permanently discards ALL edits; the file on disk becomes identical to the last commit. Cannot be undone.">↶ Hard reset (discard all edits)</button><button data-detail-action="compare" data-tooltip="Open side-by-side compare with reset options">⇄ Compare with remote</button>` : ''}${entry.kind === 'submodule' ? `<button data-detail-action="subserver">Open submodule repository ↗</button><button data-detail-action="subgraph">Submodule branch map</button><button data-detail-action="versions">Change version</button><button data-detail-action="subcommit" ${entry.status ? '' : 'disabled'} data-tooltip="${entry.status ? 'Commit uncommitted changes inside the submodule' : 'Nothing to commit — no uncommitted changes inside this submodule'}">Commit submodule</button><button data-detail-action="subpull" data-tooltip="Fast-forward pull — brings in new commits from the submodule's remote. Refuses if it would require a manual merge.">Pull submodule</button><button data-detail-action="subpush">Push submodule</button><button data-detail-action="subforcepush" class="danger-action-soft" data-tooltip="⚠️ Overwrites the remote branch with your local history, discarding any commits there aren't in yours. Only safe if nobody else uses that remote.">Force push submodule…</button><button data-detail-action="subfetch">Fetch submodule</button><button data-detail-action="location">Replace repository URL</button>` : ''}<button class="danger-action" data-detail-action="delete">Delete…</button></div>
    <div class="detail-section"><h3>GENERAL</h3><div class="detail-grid"><span>Type</span><strong>${kindLabel}</strong><span>Git</span><strong>${entry.tracked ? (entry.status || 'Tracked, clean') : 'Untracked'}</strong>
    ${entry.item_count != null ? `<span>Items</span><strong>${entry.item_count}</strong>` : `<span>Size</span><strong>${formatSize(entry.size)}</strong>`}<span>Modified</span><strong>${formatModified(entry.modified)}</strong></div></div>
    ${entry.kind === 'submodule' ? `<div class="detail-section"><h3>SUBMODULE</h3><div class="detail-grid"><span>Remote</span><strong>${esc(entry.submodule_url || 'Not configured')}</strong><span>Branch</span><strong>${esc(entry.submodule_branch || 'Default')}</strong><span>Status</span><strong>${entry.status ? (entry.status === 'M' ? 'Has local changes' : 'Modified') : 'Clean'}</strong></div>${entry.submodule_push_status ? `<div class="submodule-push-banner"><i></i><span>${esc(entry.submodule_push_status)}</span></div>` : ''}</div>` : ''}
    <div class="detail-section"><h3>LAST COMMIT</h3><div class="detail-grid"><span>Commit</span><strong>${esc(entry.last_commit_id?.slice(0, 8) || 'No commit')}</strong><span>Message</span><strong>${commitSubjectHtml(entry.last_commit_subject || '—')}</strong><span>Author</span><strong>${esc(entry.last_commit_author || '—')}</strong><span>Date</span><strong>${esc(entry.last_commit_date || '—')}</strong></div></div></div>`;
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
  if (action === 'subpush') return pushSubmodule(entry);
  if (action === 'subforcepush') return forcePushSubmodule(entry);
  if (action === 'subfetch') return fetchSubmodule(entry);
  if (['head','stage','unstage'].includes(action)) return runEntryFileAction(entry, action);
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
    const successMsg = `${entry.name}: committed inside the submodule. The parent project's link to it is unchanged — use "Change version" if you want the parent to point at this new commit.`;
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
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

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
    const successMsg = `${entry.name}: force pushed to branch "${result?.branch}". The remote now matches your local history exactly (commit ${shortSha}). The project was updated too.`;
    status(successMsg); showOperationToast(successMsg, 'success');
  }
  catch (error) { const message = handleError(error); showOperationToast(message, 'error'); }
}

async function pushSubmodule(entry) {
  if (entry.kind !== 'submodule') return;
  if (!await customConfirm(`Push commits in submodule ${entry.name} to its own remote repository?`, { title: 'Push submodule', okLabel: 'Push' })) { status('Push cancelled'); return; }
  if (!invoke) return status(`Preview: pushed ${entry.name}`);
  try {
    status(`Pushing ${entry.name} to its remote…`, 'busy');
    const result = await invoke('push_submodule', { repositoryPath: state.repository.path, relativePath: entry.relative_path });
    directoryCache.clear(); await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true });
    const shortSha = (result?.revision || '').slice(0, 8);
    const successMsg = `${entry.name}: pushed to branch "${result?.branch}" on its remote. The project was updated to commit ${shortSha}.`;
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
  const explanations = { head: 'HARD RESET: this permanently discards ALL uncommitted edits in this file. The file on disk will become identical to the last commit (HEAD). This cannot be undone. Continue?', stage: 'Add this file to the staging area?', unstage: 'SOFT/MIXED RESET: remove this file from staging. Your edits on disk are kept exactly as they are — only the staging entry is reset. Continue?' };
  const successMessages = { head: `${entry.name}: HARD RESET complete — working copy reverted to the last commit (HEAD). The project on disk was updated.`, stage: `${entry.name}: staged. It will be included in the next commit.`, unstage: `${entry.name}: SOFT/MIXED RESET complete — removed from staging. Your edits on disk were kept unchanged.` };
  const titles = { head: 'Hard reset', stage: 'Stage file', unstage: 'Soft/Mixed reset' };
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
  try { status(`Saving ${state.editingPath}…`, 'busy'); await saveEditorContent(); refs.editorDialog.close(); directoryCache.clear(); await loadRepository(state.repository.path); status(`Saved ${state.editingPath}`); }
  catch (error) { handleError(error); }
}

async function saveEditorContent() { if (!invoke) { state.editorOriginal = refs.editorContent.value; updateEditorSaveState(); return; } await invoke('write_text_file', { repositoryPath: state.repository.path, relativePath: state.editingPath, content: refs.editorContent.value }); state.editorOriginal = refs.editorContent.value; updateEditorSaveState(); }
function updateEditorSaveState() { const dirty = refs.editorContent.value !== state.editorOriginal; $('#editorSaveState').textContent = dirty ? 'Unsaved local edits' : 'Saved locally · UTF-8 · maximum 2 MB'; $('#editorSaveState').classList.toggle('warning', dirty); }

async function openSubmoduleGraph(entry) {
  clearDetails('Select a submodule commit');
  if (!invoke) { state.graphContext = { name: entry.name }; state.view = 'graph'; render(); return; }
  try { const data = await invoke('submodule_repository', { repositoryPath: state.repository.path, relativePath: entry.relative_path }); state.graphContext = { name: entry.name, parent: { repository: state.repository, branches: state.branches, commits: state.allCommits, changes: state.changes } }; state.repository = data.repository; state.branches = data.branches; state.commits = data.commits; state.allCommits = data.commits; state.changes = data.changes; state.view = 'graph'; render(); }
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
  try { status('Renaming branch…', 'busy'); await invoke('rename_branch', { repositoryPath: state.repository.path, oldName, newName }); await loadRepository(state.repository.path); status(`Branch renamed to ${newName}`); }
  catch (error) { handleError(error); }
}

async function deleteBranch(branchName) {
  if (!state.repository) return;
  if (!invoke) { status(`Preview: deleted ${branchName}`); return; }
  try { status('Deleting branch…', 'busy'); await invoke('delete_branch', { repositoryPath: state.repository.path, branchName }); await loadRepository(state.repository.path); status(`Branch ${branchName} deleted`); }
  catch (error) { handleError(error); }
}

function renderRemotes() {
  refs.remoteCards.innerHTML = state.remotes.map(remote => `<article class="remote-card"><div class="remote-symbol">◎</div><div><h2>${esc(remote.name)}</h2><span>FETCH URL</span><code>${esc(remote.fetch_url)}</code><span>PUSH URL</span><code>${esc(remote.push_url)}</code></div><button data-fetch-remote="${esc(remote.name)}">Fetch now</button></article>`).join('') || '<div class="remote-empty">No remote is configured for this repository.</div>';
  refs.remoteCards.querySelectorAll('[data-fetch-remote]').forEach(button => button.addEventListener('click', () => fetchRemote(button.dataset.fetchRemote)));
}

async function loadRemotes() { state.view = 'remotes'; refs.search.value = ''; clearDetails('Remote configuration'); if (invoke) { try { state.remotes = await invoke('list_remotes', { repositoryPath: state.repository.path }); } catch (error) { handleError(error); } } else state.remotes = [{ name: 'origin', fetch_url: 'git@example.com:vehicle-control.git', push_url: 'git@example.com:vehicle-control.git' }]; render(); }
async function fetchRemote(name) { try { status(`Fetching ${name}…`, 'busy'); await invoke('fetch_remote', { repositoryPath: state.repository.path, remote: name }); await loadRepository(state.repository.path); await loadRemotes(); status(`${name} updated`); } catch (error) { handleError(error); } }

async function stashWork() {
  if (!state.repository) return;
  if (!invoke) { state.hasStash = true; updateStashUI(); status('Preview: work stashed'); return; }
  try { status('Saving work in progress…', 'busy'); await invoke('stash_changes', { repositoryPath: state.repository.path }); state.hasStash = true; updateStashUI(); refs.commitMessage.value = ''; await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true }); status('Work saved to stash'); showOperationToast('Work stashed. Use "Pop stash" to restore it.'); }
  catch (error) { handleError(error); }
}

async function popStash() {
  if (!state.repository || !state.hasStash) return;
  if (!invoke) { state.hasStash = false; updateStashUI(); status('Preview: stash restored'); return; }
  try { status('Restoring stashed work…', 'busy'); await invoke('pop_stash', { repositoryPath: state.repository.path }); state.hasStash = false; updateStashUI(); await loadRepository(state.repository.path); await openDirectory(state.currentPath, { force: true }); status('Stashed work restored'); }
  catch (error) { const msg = String(error); status(msg, 'error'); if (msg.includes('conflict')) showOperationToast('Conflicts while restoring stash. Resolve manually.', 'error'); }
}

function updateStashUI() { $('#stashWork').hidden = state.hasStash; $('#popStash').hidden = !state.hasStash; }

function graphLayouts(commits) {
  const active = []; return commits.map(commit => {
    let lane = active.indexOf(commit.id); if (lane < 0) { lane = active.findIndex(item => !item); if (lane < 0) lane = active.length; active[lane] = commit.id; }
    const before = [...active]; active.splice(lane, 1, ...(commit.parents || []).filter((id, index, all) => id && all.indexOf(id) === index && !active.includes(id)));
    const after = [...active]; return { lane, before, after, parentLanes: (commit.parents || []).map(id => after.indexOf(id)).filter(value => value >= 0) };
  });
}

function graphSvg(commit, layout, maxLanes) {
  const laneWidth = 34; const height = 64;
  const viewWidth = Math.max(maxLanes, 1) * laneWidth;
  const lane = layout.lane; const x = laneWidth / 2 + lane * laneWidth; const color = palette[lane % palette.length];
  const laneX = index => laneWidth / 2 + index * laneWidth;

  // Every other lane that simply passes through this row (not this commit's own lane).
  const continuations = layout.after.map((id, targetLane) => {
    if (targetLane === lane) return '';
    const sourceLane = layout.before.indexOf(id); if (sourceLane < 0) return '';
    const sx = laneX(sourceLane), tx = laneX(targetLane);
    const lineColor = palette[sourceLane % palette.length];
    return sourceLane === targetLane
      ? `<line x1="${sx}" y1="0" x2="${tx}" y2="${height}" stroke="${lineColor}" stroke-width="3" stroke-linecap="round"/>`
      : `<path d="M${sx} 0 C${sx} ${height * 0.4} ${tx} ${height * 0.6} ${tx} ${height}" stroke="${lineColor}" stroke-width="3" fill="none" stroke-linecap="round"/>`;
  }).join('');

  // This commit's own edges: up to where it was referenced from, and down to each parent.
  const hasIncoming = layout.before[lane];
  const incoming = hasIncoming ? `<line x1="${x}" y1="0" x2="${x}" y2="${height / 2}" stroke="${color}" stroke-width="3" stroke-linecap="round"/>` : '';
  const outgoing = layout.parentLanes.map(parentLane => { const px = laneX(parentLane); return parentLane === lane
    ? `<line x1="${x}" y1="${height / 2}" x2="${x}" y2="${height}" stroke="${color}" stroke-width="3" stroke-linecap="round"/>`
    : `<path d="M${x} ${height / 2} C${x} ${height * 0.75} ${px} ${height * 0.75} ${px} ${height}" stroke="${color}" stroke-width="3" fill="none" stroke-linecap="round"/>`; }).join('');

  const tags = (commit.refs || []).filter(ref => ref !== 'HEAD').map(ref => `<span class="branch-tip" style="--lane-color:${color}">${esc(ref)}</span>`).join('');
  return `<svg viewBox="0 0 ${viewWidth} ${height}" preserveAspectRatio="xMinYMid meet">${continuations}${incoming}${outgoing}<circle cx="${x}" cy="${height / 2}" r="6" fill="${color}" stroke="#0d1117" stroke-width="2.5"/></svg><div class="branch-tips">${tags}</div>`;
}

function renderGraph() {
  const query = refs.search.value.trim().toLowerCase();
  const commits = state.commits.filter(c => !query || `${c.subject} ${c.author} ${c.id} ${(c.refs || []).join(' ')}`.toLowerCase().includes(query));
  const layouts = graphLayouts(commits);
  const maxLanes = Math.max(1, ...layouts.map(l => Math.max(l.before.length, l.after.length)));
  refs.laneLegend.innerHTML = '<span class="time-direction"><b>NEWEST</b><i>↓ time</i><b>OLDEST</b></span><span class="graph-help"><b>Git ancestry:</b> every line continues to a parent below. A new lane is a fork; joined lanes are a merge.</span>';
  refs.graph.innerHTML = commits.map((commit, index) => `<article class="commit-row" data-id="${esc(commit.id)}">
    <div class="graph-cell" style="width:${34 * maxLanes}px">${graphSvg(commit, layouts[index], maxLanes)}</div>
    <div class="commit-card"><div class="commit-main"><span class="commit-title">${commitSubjectHtml(commit.subject)}</span><span class="commit-id">${esc(commit.id.slice(0, 8))}</span></div>
    <span class="topology-badges">${commit.parents?.length > 1 ? `<b class="merge-badge">MERGE · ${commit.parents.length} parents</b>` : ''}</span><span class="commit-author">${esc(commit.author)}</span><span class="commit-date">${esc(commit.date)}</span></div>
  </article>`).join('') || '<div class="empty-change">No commits match this filter</div>';
  refs.graph.querySelectorAll('.commit-row').forEach(row => row.addEventListener('click', () => selectCommit(row.dataset.id)));
}

function selectCommit(id) {
  state.selectedCommit = state.commits.find(commit => commit.id === id);
  refs.graph.querySelectorAll('.commit-row').forEach(row => row.classList.toggle('selected', row.dataset.id === id));
  const c = state.selectedCommit;
  refs.details.innerHTML = `<div class="commit-details"><div class="large-node"></div><h2>${commitSubjectHtml(c.subject)}</h2><div class="hash">${esc(c.id)}</div>
    <div class="detail-grid"><span>Author</span><strong>${esc(c.author)}</strong><span>Date</span><strong>${esc(c.date)}</strong>
    <span>Parents</span><strong>${esc(c.parents?.join(', ') || 'First commit')}</strong><span>Refs</span><strong>${esc(c.refs?.join(', ') || '—')}</strong></div></div>`;
}

function renderChanges() {
  refs.changeBadge.textContent = state.changes.length;
  refs.workspaceSubtitle.textContent = state.repository ? (state.changes.length ? `${state.changes.length} changed files` : 'Everything committed') : 'No repository loaded';
  const scope = state.changesScope === 'folder' ? state.currentPath : ''; const scopedChanges = state.changes.filter(change => !scope || change.path === scope || change.path.startsWith(`${scope}/`));
  refs.drawerScopeTitle.textContent = state.changesScope === 'folder' ? `Changes in folder · /${scope}` : 'Working tree · entire repository';
  refs.changesSummary.textContent = scopedChanges.length ? `${scopedChanges.length} file${scopedChanges.length === 1 ? '' : 's'} available for staging` : `No changes in ${scope ? `/${scope}` : 'the repository'}`;
  refs.changes.innerHTML = scopedChanges.map(change => `<label class="change-row"><input type="checkbox" data-change-path="${esc(change.path)}" ${change.staged ? 'checked' : ''}>
    <span class="status-code">${esc(change.status)}</span><span class="change-path">${esc(change.path)}</span><span class="change-state">${change.staged ? 'Staged' : 'Modified'}</span></label>`).join('') || `<div class="empty-change">No changes inside /${esc(scope)}</div>`;
  refs.changes.querySelectorAll('[data-change-path]').forEach(input => input.addEventListener('change', () => toggleStage(input.dataset.changePath, input.checked)));
  const staged = scopedChanges.filter(change => change.staged).length;
  refs.selectionText.textContent = `${staged} file${staged === 1 ? '' : 's'} in staging area`;
  refs.commitButton.disabled = !staged || !refs.commitMessage.value.trim();
}

async function openPublish() {
  if (!state.repository) return;
  if (!state.remotes.length && invoke) state.remotes = await invoke('list_remotes', { repositoryPath: state.repository.path });
  if (!invoke && !state.remotes.length) state.remotes = [{ name: 'origin', fetch_url: 'git@example.com:vehicle-control.git', push_url: 'git@example.com:vehicle-control.git' }];
  const locals = state.branches.filter(branch => !branch.remote); refs.publishBranch.innerHTML = locals.map(branch => `<option value="${esc(branch.name)}" ${branch.current ? 'selected' : ''}>${esc(branch.name)}${branch.current ? ' (current)' : ''}</option>`).join('');
  refs.publishRemote.innerHTML = state.remotes.map(remote => `<option value="${esc(remote.name)}">${esc(remote.name)}</option>`).join(''); refs.publishDialog.showModal(); await refreshPublish();
}

async function refreshPublish() {
  const branch = refs.publishBranch.value, remote = refs.publishRemote.value;
  if (!branch || !remote) { refs.publishCommits.innerHTML = '<div class="publish-empty">Configure a remote before publishing.</div>'; refs.publishSummary.textContent = 'Nothing to publish'; $('#confirmPublish').disabled = true; return; }
  refs.publishDestination.textContent = `${branch} → ${remote}/${branch}`; refs.publishCommits.innerHTML = '<div class="loading-row"><i class="spinner"></i>Checking server state…</div>';
  if (!invoke) state.publish = { branch, remote, commits: previewData.commits.slice(0, 2) }; else state.publish = await invoke('publish_status', { repositoryPath: state.repository.path, branch, remote });
  refs.publishCommits.innerHTML = state.publish.commits.map((commit, index) => `<div class="publish-commit"><span>${index + 1}</span><i></i><div><strong>${esc(commit.subject)}</strong><small>${esc(commit.id.slice(0, 8))} · ${esc(commit.author)} · ${esc(commit.date)}</small></div><b>WILL PUSH</b></div>`).join('') || '<div class="publish-empty">This branch is already up to date on the server.</div>';
  refs.publishSummary.textContent = `${state.publish.commits.length} commit${state.publish.commits.length === 1 ? '' : 's'} to publish`; refs.publishBadge.textContent = state.publish.commits.length; refs.publishSubtitle.textContent = state.publish.commits.length ? `${state.publish.commits.length} local commits not on ${remote}` : 'Everything is on the server'; $('#confirmPublish').disabled = !state.publish.commits.length;
}

async function confirmPublish(event) {
  event.preventDefault(); if (!state.publish?.commits.length) return;
  const operation = $('#publishOperationStatus'); operation.textContent = `Publishing ${state.publish.branch}…`; operation.className = 'submodule-operation-status busy';
  try { $('#confirmPublish').disabled = true; status(`Publishing ${state.publish.branch}…`, 'busy'); await invoke('publish_branch', { repositoryPath: state.repository.path, branch: state.publish.branch, remote: state.publish.remote, username: $('#publishUsername').value.trim(), accessToken: $('#publishToken').value }); $('#publishToken').value = ''; refs.publishDialog.close(); await loadRepository(state.repository.path); status(`Published ${state.publish.branch} to ${state.publish.remote}`); showOperationToast(`Published ${state.publish.branch} to ${state.publish.remote}`); }
  catch (error) { const message = String(error); operation.textContent = message; operation.className = 'submodule-operation-status error'; status(message, 'error'); $('#confirmPublish').disabled = false; }
}

async function toggleStage(path, checked) {
  if (!invoke) return;
  try { const folder = state.currentPath; await invoke(checked ? 'stage_files' : 'unstage_files', { repositoryPath: state.repository.path, files: [path] }); await loadRepository(state.repository.path); if (folder) await openDirectory(folder); }
  catch (error) { handleError(error); }
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
$('#refresh').addEventListener('click', () => state.repository && loadRepository(state.repository.path));
$('#showChanges').addEventListener('click', () => { state.changesScope = 'global'; renderChanges(); refs.changesDrawer.classList.add('open'); });
$('#showFolderChanges').addEventListener('click', () => { state.changesScope = 'folder'; renderChanges(); refs.changesDrawer.classList.add('open'); });
$('#showPublish').addEventListener('click', () => openPublish().catch(error => status(String(error), 'error')));
$('#stashWork').addEventListener('click', stashWork);
$('#popStash').addEventListener('click', popStash);
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
$('#saveFile').addEventListener('click', saveEditor); $('#closeEditor').addEventListener('click', () => refs.editorDialog.close()); $('#cancelEditor').addEventListener('click', () => refs.editorDialog.close());
refs.editorContent.addEventListener('input', updateEditorSaveState);
$('#fetchCurrent').addEventListener('click', () => { const first = state.remotes[0]?.name || state.branches.find(branch => branch.remote)?.name.split('/')[0]; if (first) fetchRemote(first); else status('No remote is configured', 'error'); });
async function syncCurrent(action) { if (!invoke || !state.repository) return; try { status(`${action === 'pull' ? 'Pulling' : 'Pushing'} ${state.repository.current_branch}…`, 'busy'); await invoke('sync_repository', { repositoryPath: state.repository.path, action }); await loadRepository(state.repository.path); status(`${action === 'pull' ? 'Pull' : 'Push'} complete`); } catch (error) { handleError(error); } }
$('#pullCurrent').addEventListener('click', () => syncCurrent('pull')); $('#pushCurrent').addEventListener('click', () => syncCurrent('push'));
refs.publishBranch.addEventListener('change', refreshPublish); refs.publishRemote.addEventListener('change', refreshPublish); $('#confirmPublish').addEventListener('click', confirmPublish);
[['#compareRestoreRemote','remote'],['#compareRestoreHead','head'],['#compareStage','stage'],['#compareUnstage','unstage']].forEach(([selector, action]) => $(selector)?.addEventListener('click', event => { event.preventDefault(); updateRecoveryHelp(action); applyFileRecovery(action).catch(error => handleError(error)); }));
document.addEventListener('click', event => { const link = event.target.closest('.polarion-link'); if (!link) return; event.preventDefault(); if (invoke) invoke('open_external_url', { url: link.href }).catch(error => status(String(error), 'error')); else window.open(link.href, '_blank', 'noopener'); });
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
  if (!state.repository) return; const name = await customPrompt('New branch name', '', { title: 'Create branch' }); if (!name) return;
  try { await invoke('create_branch', { path: state.repository.path, branch: name }); await loadRepository(state.repository.path); status(`Branch "${name}" created`); } catch (error) { handleError(error); }
});
refs.commitButton.addEventListener('click', async () => {
  try { const folder = state.changesScope === 'folder' ? state.currentPath : ''; const files = state.changes.filter(change => change.staged && (!folder || change.path === folder || change.path.startsWith(`${folder}/`))).map(change => change.path); await invoke('commit_files', { repositoryPath: state.repository.path, files, message: refs.commitMessage.value }); refs.commitMessage.value = ''; await loadRepository(state.repository.path); if (folder) await openDirectory(folder); }
  catch (error) { handleError(error); }
});

// Command Palette
const commands = [
  { id: 'open-repo', name: 'Open Repository', keys: 'Ctrl+O', fn: openRepository },
  { id: 'new-branch', name: 'New Branch', keys: 'Ctrl+Shift+B', fn: () => $('#newBranch').click() },
  { id: 'commit', name: 'Commit Changes', keys: 'Ctrl+Shift+C', fn: () => { state.changesScope = 'global'; refs.changesDrawer.classList.add('open'); refs.commitMessage.focus(); } },
  { id: 'push', name: 'Push Current Branch', keys: 'Ctrl+Shift+P', fn: () => $('#pushCurrent').click() },
  { id: 'pull', name: 'Pull Current Branch', keys: '', fn: () => $('#pullCurrent').click() },
  { id: 'fetch', name: 'Fetch Remote', keys: 'Ctrl+Shift+F', fn: () => $('#fetchCurrent').click() },
  { id: 'stash', name: 'Stash Work in Progress', keys: 'Ctrl+Shift+S', fn: stashWork },
  { id: 'pop', name: 'Restore Stashed Work', keys: '', fn: popStash },
  { id: 'search', name: 'Search Repository', keys: 'Ctrl+F', fn: () => refs.search.focus() },
  { id: 'explorer', name: 'Go to Explorer', keys: '', fn: () => $('#navExplorer').click() },
  { id: 'commander', name: 'Go to Local ↔ Remote', keys: 'Ctrl+Shift+L', fn: () => $('#navCommander').click() },
  { id: 'graph', name: 'Go to Branch Map', keys: 'Ctrl+Shift+G', fn: () => $('#navGraph').click() },
  { id: 'remotes', name: 'Go to Remotes', keys: '', fn: () => $('#navRemotes').click() },
];
let commandPaletteOpen = false;
function openCommandPalette() {
  const input = $('#commandInput');
  const list = $('#commandList');
  commandPaletteOpen = true;
  input.value = '';
  list.innerHTML = commands.map((cmd, i) => `<div class="command-item ${i === 0 ? 'selected' : ''}" data-cmd-id="${esc(cmd.id)}">
    <span class="command-name">${esc(cmd.name)}</span>${cmd.keys ? `<span class="command-keys">${esc(cmd.keys)}</span>` : ''}</div>`).join('');
  $('#commandPalette').showModal();
  input.focus();
}
function filterCommands(query) {
  const list = $('#commandList');
  const filtered = commands.filter(c => !query || c.name.toLowerCase().includes(query.toLowerCase()));
  list.innerHTML = filtered.map((cmd, i) => `<div class="command-item ${i === 0 ? 'selected' : ''}" data-cmd-id="${esc(cmd.id)}">
    <span class="command-name">${esc(cmd.name)}</span>${cmd.keys ? `<span class="command-keys">${esc(cmd.keys)}</span>` : ''}</div>`).join('') || '<div class="command-item" style="text-align:center;color:#6b7f96;">No commands found</div>';
}
$('#commandInput').addEventListener('input', (e) => filterCommands(e.target.value));
$('#commandInput').addEventListener('keydown', (e) => {
  const items = Array.from($('#commandList').querySelectorAll('.command-item'));
  const selected = items.find(i => i.classList.contains('selected'));
  if (e.key === 'ArrowDown') { e.preventDefault(); const next = selected?.nextElementSibling || items[0]; items.forEach(i => i.classList.remove('selected')); next?.classList.add('selected'); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); const prev = selected?.previousElementSibling || items[items.length - 1]; items.forEach(i => i.classList.remove('selected')); prev?.classList.add('selected'); }
  else if (e.key === 'Enter') { e.preventDefault(); const cmd = commands.find(c => c.id === selected?.dataset.cmdId); if (cmd) { $('#commandPalette').close(); cmd.fn(); commandPaletteOpen = false; } }
});
$('#commandList').addEventListener('click', (e) => {
  const item = e.target.closest('.command-item');
  if (item && item.dataset.cmdId) {
    const cmd = commands.find(c => c.id === item.dataset.cmdId);
    if (cmd) { $('#commandPalette').close(); cmd.fn(); commandPaletteOpen = false; }
  }
});
$('#commandPalette').addEventListener('close', () => { commandPaletteOpen = false; });
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === 'k') { e.preventDefault(); commandPaletteOpen ? $('#commandPalette').close() : openCommandPalette(); }
  else if ((e.ctrlKey || e.metaKey) && e.key === 'o') { e.preventDefault(); openRepository(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'C')) { e.preventDefault(); commands.find(c => c.id === 'commit')?.fn(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'P')) { e.preventDefault(); commands.find(c => c.id === 'push')?.fn(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'F')) { e.preventDefault(); commands.find(c => c.id === 'fetch')?.fn(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'S')) { e.preventDefault(); state.hasStash ? popStash() : stashWork(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'L')) { e.preventDefault(); commands.find(c => c.id === 'commander')?.fn(); }
  else if ((e.ctrlKey || e.metaKey) && (e.shiftKey && e.key === 'G')) { e.preventDefault(); commands.find(c => c.id === 'graph')?.fn(); }
  else if ((e.ctrlKey || e.metaKey) && e.key === 'f' && !['input','textarea'].includes(document.activeElement.tagName.toLowerCase())) { e.preventDefault(); refs.search.focus(); }
});


if (!invoke) {
  refs.browserNotice.hidden = false;
  if (new URLSearchParams(location.search).has('demo')) Object.assign(state, previewData);
}
renderRecentRepos();
render();
