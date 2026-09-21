const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'frontend/index.html'), 'utf8');
const app = fs.readFileSync(path.join(root, 'frontend/app.js'), 'utf8');
const css = fs.readFileSync(path.join(root, 'frontend/styles.css'), 'utf8');

test('Terminal is the first default command panel and has an explicit close button', () => {
  const terminalTab = html.indexOf('data-mode="console"');
  const actionsTab = html.indexOf('data-mode="commands"');
  const savedTab = html.indexOf('data-mode="saved"');
  assert.ok(terminalTab >= 0 && terminalTab < actionsTab);
  assert.ok(savedTab > actionsTab);
  assert.match(html, /id="closeCommandPalette"[^>]*>×<\/button>/);
  assert.match(html, /id="commandModeOptions"/);
  assert.match(app, /function openCommandPalette\(\)[\s\S]*?setConsoleMode\(state\.consoleMode \|\| 'console'\)/);
  const openPaletteBody = app.match(/function openCommandPalette\(\) \{([\s\S]*?)\n\}/)?.[1] || '';
  assert.doesNotMatch(openPaletteBody, /setCommandInputValue\(''\)/);
});

test('Add submodule starts at the Vitesco engineering namespace', () => {
  assert.match(app, /const defaultSubmoduleUrl = '\.\.\/\.\.\/eng\/'/);
  assert.match(app, /setSelectionRange\(defaultSubmoduleUrl\.length, defaultSubmoduleUrl\.length\)/);
  assert.match(html, /PORTABLE REPOSITORY URL/);
  assert.match(html, /Use <code>\.\.\/\.\.\/ORG\/REPO<\/code>/);
});

test('Vitesco browser URLs are suggested as portable gitmodules URLs', () => {
  assert.match(app, /function portableSubmoduleUrl\(url\)/);
  assert.match(app, /return `\.\.\/\.\.\/\$\{match\[1\]\}\/\$\{match\[2\]\}`/);
  assert.match(app, /Portable \.gitmodules URL for submodule/);
});

test('cancelling a child branch or tag dialog preserves the submodule version selector', () => {
  assert.doesNotMatch(app, /const entry = submoduleMenuEntry;\s*refs\.submoduleMenu\.hidden = true;\s*if \(versionFilter === 'tag'\)/);
  assert.match(app, /const childActionOpen = refs\.newBranchDialog\.open \|\| \$\('#newTagDialog'\)\.open/);
  assert.match(app, /if \(!childActionOpen && !refs\.submoduleMenu\.hidden/);
});

test('slow Git actions expose progress on the exact button that was pressed', () => {
  assert.match(app, /function beginButtonOperation\(button, label\)[\s\S]*?button\.disabled = true;[\s\S]*?aria-busy[\s\S]*?spinner/);
  assert.match(app, /switchSubmoduleVersion\(row\.dataset\.revision, row\.dataset\.versionKind, row\.dataset\.name, button\)/);
  assert.match(app, /const finishButton = beginButtonOperation\(button, 'Switching…'\)/);
  assert.match(app, /async function refreshRepository\(button = null\)/);
  assert.match(app, /const finishButton = beginButtonOperation\(button, 'Refreshing…'\)/);
  assert.match(app, /Refreshing repository from disk…/);
  assert.match(app, /Repository refreshed — \$\{state\.commits\.length\} commits loaded\./);
  assert.match(html, /Refresh repository from disk — re-read branches, commits and working tree status; does not fetch from the server/);
  assert.match(app, /finally \{ finishButton\(\); \}/);
});

test('top bar has a safe project fetch that updates parent and submodule refs without pulling', () => {
  assert.match(html, /id="fetchProject"/);
  assert.match(html, /Fetch parent repository and initialized submodules — safe update only, no pull, checkout or branch change/);
  assert.match(app, /'fetch_project'/);
  assert.match(app, /async function fetchProjectAndSubmodules\(button = null\)/);
  assert.match(app, /invoke\('fetch_project', \{ repositoryPath: state\.repository\.path \}\)/);
  assert.match(app, /Parent updated/);
  assert.match(app, /submodule\$\{result\.submodules_total === 1 \? '' : 's'\} fetched/);
  assert.match(app, /id: 'fetch-project'/);
  assert.match(app, /id: 'init-update-submodules'/);
  assert.match(app, /git submodule update --init --recursive/);
  assert.match(css, /\.repo-picker \{ flex: 0 1 360px;/);
});

test('fast repository open keeps folder navigation filesystem-only until the first status scan completes', () => {
  assert.match(app, /async function openDirectory\(path, options = \{\}\)/);
  assert.match(app, /if \(!state\.statusReady && !options\.force && !options\.invalidateGit\) \{/);
  assert.match(app, /return paintDirectoryFast\(path, requestId\)/);
  assert.match(app, /completeRepositoryOpenStatus\(state\.repository\.path, generation\)/);
  assert.match(app, /await openDirectory\(state\.currentPath, \{ force: true \}\)/);
  assert.match(app, /completeRepositoryOpenStatus finishes, it reloads the \*current\* folder/);
});

test('UTRUD can be launched for any selected folder, not only folders named r', () => {
  assert.match(app, /data-detail-action="utrud"/);
  assert.match(app, /Launches UTRUD with this folder path/);
  assert.doesNotMatch(app, /entry\.name === 'r'/);
  assert.doesNotMatch(app, /currentPath\.split\('\/'\)\.filter\(Boolean\)\.at\(-1\) !== 'r'/);
  assert.match(app, /runUtrud\(\{ kind: 'folder', name, relative_path: state\.currentPath \}\)/);
  assert.match(html, /Launch UTRUD for the current folder/);
});

test('details panel extracts spec ids from loaded commit text and shows compact submodule repository links', () => {
  assert.match(app, /const SPEC_ID_PATTERN =/);
  assert.match(app, /function extractSpecIds\(text = ''\)/);
  assert.match(app, /\[A-Z0-9\]\{8\}\\\.\[A-Z0-9\]\{3\}/);
  assert.match(app, /matchAll\(SPEC_ID_PATTERN\)\]\.map\(match => match\[1\]\)/);
  assert.match(app, /function specDetailRows\(\.\.\.texts\)/);
  assert.match(app, /specDetailRows\(entry\.last_commit_subject\)/);
  assert.match(app, /specDetailRows\(entry\.submodule_commit_subject, \.\.\.\(entry\.submodule_commit_tags \|\| \[\]\)\)/);
  assert.match(app, /entry\.submodule_commit_tags\?\.length/);
  assert.match(app, /function tagListHtml\(tags = \[\]\)/);
  assert.match(app, /function compactRepositoryLabel\(url = ''\)/);
  assert.match(app, /function submoduleRepositoryLinkHtml\(entry\)/);
  assert.match(app, /function submoduleRepositoryRowsHtml\(entry\)/);
  assert.match(app, /entry\.submodule_web_url \? `<span>GitHub<\/span>/);
  assert.match(app, /class="submodule-repository-link"/);
  assert.match(app, /entry\.submodule_web_url/);
  assert.match(app, /event\.target\.closest\('\.submodule-repository-link'\)/);
  assert.match(css, /\.spec-list code/);
  assert.match(css, /\.submodule-repository-link/);
});

test('folder and submodule personal notes stay outside Git and are rendered from an in-memory map', () => {
  assert.match(app, /drillDownNotes: \{\}/);
  assert.match(app, /function ensureDrillDownNotesLoaded\(repositoryPath, force = false\)/);
  assert.match(app, /if \(!force && state\.drillDownNotesRepositoryPath === repositoryPath\) return;/);
  assert.match(app, /invoke\('load_drill_down_notes', \{ repositoryPath \}\)/);
  assert.match(app, /function noteForPath\(path\)/);
  assert.match(app, /function entrySupportsPersonalNote\(entry\)/);
  assert.match(app, /\['folder', 'submodule'\]\.includes\(entry\.kind\)/);
  assert.match(app, /function entryHasPersonalNote\(entry\)/);
  assert.match(app, /function renderPersonalNoteSection\(entry\)/);
  assert.match(app, /PERSONAL NOTE/);
  assert.match(app, /invoke\('set_drill_down_note', \{ repositoryPath: state\.repository\.path, relativePath: entry\.relative_path, note: text \}\)/);
  assert.match(app, /invoke\('delete_drill_down_note', \{ repositoryPath: state\.repository\.path, relativePath: entry\.relative_path \}\)/);
  assert.match(app, /class="personal-note-dot"/);
  assert.match(css, /\.personal-note-section/);
});

test('deleted tracked paths render without trying to stat a path that no longer exists', () => {
  assert.match(app, /\['deleted', 'deleted-folder', 'deleted-submodule'\]\.includes\(state\.selectedEntry\?\.kind\)/);
  assert.match(app, /Deleted tracked file/);
  assert.match(app, /Deleted tracked folder/);
});

test('Explorer marks paths preserved in a stash without hiding their current Git state', () => {
  assert.match(app, /if \(entry\.stashed\) addState\('ST'/);
  assert.match(app, /states\.length === 1/);
  assert.match(app, /primaryState = states\.find\(state => !\['ST', 'NP'\]\.includes\(state\.code\)\)/);
  assert.match(app, /git-badge-tray/);
  assert.match(css, /\.git-code-badge\.stashed/);
});

test('Git column keeps full single-state labels and abbreviates only real combinations', () => {
  assert.match(app, /M: \{ code: 'ML', label: 'Modified locally'/);
  assert.match(app, /'\?\?': \{ code: 'UN', label: 'Untracked'/);
  assert.match(app, /if \(!states\.length\) addState\('TR', 'Tracked'\)/);
  assert.match(app, /ML \+ ST \+ NP/);
});

test('a submodule row shows its attached branch or detached state, only when actually checked', () => {
  // Must never guess: a clean, fully-synced submodule skips the check
  // entirely (submodule_checked stays false) to avoid opening every
  // submodule's repository on every folder listing — this function must
  // fall back to the generic hint rather than claim "detached". A
  // registered-but-not-initialized submodule is the one exception because
  // that answer comes from a cheap `.git` existence check, not opening the
  // submodule repository.
  assert.match(app, /function submoduleHeadHint\(entry\) \{\s*if \(entry\.submodule_initialized === false\) return 'Git submodule · not initialized';\s*if \(!entry\.submodule_checked\) return 'Independent Git repository';/);
  assert.match(app, /entry\.submodule_current_branch \? `Independent Git repository · \$\{entry\.submodule_current_branch\}` : 'Independent Git repository · detached'/);
  assert.match(app, /entry\.kind === 'submodule' \? esc\(submoduleHeadHint\(entry\)\)/);
  assert.match(app, /data-detail-action="subinit"/);
  assert.match(app, /invoke\('init_submodule', \{ repositoryPath: state\.repository\.path, relativePath: entry\.relative_path \}\)/);
  assert.match(app, /entry\.submodule_initialized === false[\s\S]*?Initialize submodule/);
});

test('Explorer submodule navigation stays one coherent folder load', () => {
  // The fast-paint/background-scan experiment made Windows navigation feel
  // flickery and sometimes looked like the submodule had not opened. A
  // submodule folder click should go through the same load_directory path as
  // a normal folder, relying on the backend cache instead of a multi-phase
  // frontend repaint.
  assert.match(app, /const EXPLORER_DOUBLE_CLICK_MS = 800/);
  assert.doesNotMatch(app, /invoke\('submodule_folder_status'/);
  assert.doesNotMatch(app, /invoke\('submodule_navigation_status'/);
});

test('Reset to upstream is reachable for a submodule that is only dirty, not just ahead/behind', () => {
  // submodule_is_dirty is already computed by load_directory for this exact
  // row whenever there was anything to explain — reused here instead of a
  // second, new check, and only meaningful for whichever branch is actually
  // the active checkout right now.
  assert.match(app, /dirty: item\.current && !!submoduleMenuEntry\?\.submodule_is_dirty/);
});

test('a submodule row does not show a generic action next to its identically-behaving dedicated one', () => {
  // "View history"/"Open on server" and "Submodule Branch Map"/"Open
  // submodule repository ↗" call the exact same function with the exact
  // same arguments for a submodule entry — report: two buttons doing one
  // thing is confusing, keep only the more specifically labeled one.
  assert.match(app, /entry\.kind === 'submodule' \? '' : '<button data-detail-action="server">Open on server ↗<\/button>'/);
  assert.match(app, /entry\.kind === 'submodule' \? '' : '<button data-detail-action="history">View history<\/button>'/);
});

test('the status bar quietly shows the last real git command, and the footer opens its full history on double-click', () => {
  // 'busy' means the action is still in flight — the command that will
  // explain it hasn't been recorded on the backend yet, so refreshing then
  // would show last time's stale command instead of this one's.
  assert.match(app, /function status\(message, kind = ''\) \{[\s\S]*?if \(kind !== 'busy'\) refreshCommandHint\(\);/);
  assert.match(app, /async function refreshCommandHint\(\)[\s\S]*?invoke\('recent_git_commands'\)/);
  assert.match(app, /refs\.statusFooter\.addEventListener\('dblclick', openCommandHistoryDialog\)/);
  assert.match(html, /id="commandHistoryDialog"/);
  assert.match(html, /id="statusCommandHint"/);
  // Its own row, not a reuse of .publish-commit: that class assumes a
  // 4-column grid (checkbox/index, dot, 1fr content, badge) built for a
  // clickable, togglable push-commit list — neither applies to this plain,
  // unclickable 2-column read history, and .excluded means "de-prioritized"
  // there, the wrong signal for "this command failed".
  assert.match(app, /function commandHistoryRowHtml\(entry\) \{[\s\S]*?class="command-history-row \$\{entry\.success \? '' : 'failed'\}"/);
});

test('stash and scoped commit are item-detail actions, not crowded toolbar actions', () => {
  // These actions depend on the selected file/folder/submodule. Keeping them
  // in the header made the scope unclear and pushed the toolbar outside the
  // page; they now live in the right-side details panel.
  assert.match(app, /refs\.commitScope\.hidden = true;/);
  assert.match(app, /\$\('#stashWork'\)\.hidden = true;/);
  assert.match(app, /data-detail-action="stashwork"/);
  assert.match(app, /data-detail-action="commit"/);
});

test('stashed paths are shown as compact Git-column state, not beside the folder name', () => {
  assert.doesNotMatch(app, /entry\.stashed \? `<b class="inline-stash-badge"/);
  assert.match(app, /if \(entry\.stashed\) addState\('ST'/);
});

test('the version selector\'s own Push button reuses the real push flow, not a second one, and never leaks the menu underneath it', () => {
  // data-push-version just names the active branch that submoduleMenuEntry
  // already is — no per-row data needed, unlike data-reset-upstream.
  assert.match(app, /refs\.submoduleVersions\.querySelectorAll\('\[data-push-version\]'\)\.forEach\(button => button\.addEventListener\('click', event => \{[\s\S]*?pushSubmodule\(submoduleMenuEntry\)/);
  // Its preview/confirm dialog is a *child* of the version selector exactly
  // like the New branch/New tag dialogs already were — without this, any
  // click inside it (Cancel included) would hide the menu underneath before
  // the dialog itself even closes, the same bug class the branch/tag dialog
  // fix already covered once.
  assert.match(app, /const childActionOpen = refs\.newBranchDialog\.open \|\| \$\('#newTagDialog'\)\.open \|\| \$\('#submodulePublishDialog'\)\.open;/);
  // A successful push makes the menu's own displayed data (e.g. "N commits
  // to push") stale; when it was launched from that menu, refresh it instead
  // of leaving stale rows underneath the push dialog.
  assert.match(app, /const menuWasOpenForEntry = !refs\.submoduleMenu\.hidden && submoduleMenuEntry\?\.relative_path === entry\.relative_path;/);
  assert.match(app, /await refreshSubmoduleMenu\(\);/);
});

test('branches containing a detached commit render as a compact bounded list', () => {
  assert.match(css, /\.version-containing-branches \{[^}]*width: min\(560px, 100%\)/);
  assert.match(css, /\.version-containing-branches \{[^}]*flex-direction: column/);
  assert.match(css, /\.version-containing-branch \{[^}]*grid-template-areas: "name action" "meta action"/);
  assert.match(css, /\.version-containing-branch strong \{[^}]*text-overflow: ellipsis/);
});

test('clone is reachable outside the empty state and sends explicit clone options', () => {
  assert.match(html, /id="repoPickerMenu"/, 'repository picker must offer a compact menu while a repository is already open');
  assert.match(html, /id="repoMenuClone"/, 'repository picker menu must include Clone');
  assert.match(html, /id="cloneBranch"/, 'clone dialog should support an optional branch');
  assert.match(html, /id="cloneRecurseSubmodules"/, 'clone dialog should make recursive submodule init an explicit opt-in');
  assert.match(app, /\$\('#repoMenuClone'\)\.addEventListener\('click', \(\) => \{ closeRepoPickerMenu\(\); openCloneDialog\(\); \}\)/);
  assert.match(app, /invoke\('clone_repository', \{ url, parentPath, folderName, branch: branch \|\| null, recurseSubmodules \}\)/);
});

test('App actions can find and mark the branch start commit via visible git commands', () => {
  assert.match(app, /id: 'branch-start'/);
  assert.match(app, /git merge-base HEAD \$\{baseRef\}/);
  assert.match(app, /git show --no-patch --decorate --date=short --stat \$\{sha\}/);
  assert.match(app, /branchStartMarker/);
  assert.match(app, /is-command-branch-start/);
});

test('Saved actions are a separate persistent command tab and appear in App actions', () => {
  assert.match(html, /data-mode="saved"/);
  assert.match(html, /Saved actions/);
  assert.match(html, /id="commandAddSaved"/);
  assert.match(html, /Save action/);
  assert.match(app, /SAVED_ACTIONS_KEY/);
  assert.match(app, /function renderSavedActions/);
  assert.match(app, /function addOrEditSavedAction/);
  assert.match(app, /function runSavedAction/);
  assert.match(app, /commands to run/i);
  assert.match(app, /state\.savedActions\.forEach/);
  assert.match(app, /Saved action: \$\{action\.name\}/);
  assert.match(app, /data-saved-run/);
});

test('publish indicator surfaces ahead and behind, not only outgoing commit count', () => {
  assert.match(app, /function publishAheadBehindText\(info\)/);
  assert.match(app, /function publishRemoteAheadWarningHtml\(publish\)/);
  assert.match(app, /new remote branch/);
  assert.match(app, /\$\{ahead\} ahead \/ \$\{behind\} behind/);
  assert.match(app, /Everything is on the server · \$\{comparison\}/);
  assert.match(app, /local commit\$\{state\.publish\.commits\.length === 1 \? '' : 's'\} to publish · \$\{comparison\}/);
  assert.match(app, /has \$\{behind\} commit\$\{behind === 1 \? '' : 's'\} you do not have locally/);
  assert.match(app, /publishRemoteAheadWarningHtml\(state\.publish\) \+ publishSubmoduleRisksHtml/);
});

test('folder restore is an explicit right-panel action with preview and scoped backend commands', () => {
  assert.match(html, /id="folderRestoreDialog"/);
  assert.match(html, /Restore to HEAD/);
  assert.match(html, /Restore from commit/);
  assert.match(html, /id="folderRestoreClean" checked/);
  assert.match(app, /entry\.kind === 'folder' \? '<button data-detail-action="restorefolder"/);
  assert.match(app, /if \(action === 'restorefolder'\) return openFolderRestoreDialog\(entry\)/);
  assert.match(app, /invoke\('preview_folder_restore', \{ repositoryPath: state\.repository\.path, relativePath: model\.entry\.relative_path, sourceRevision, cleanUntracked: refs\.folderRestoreClean\.checked \}\)/);
  assert.match(app, /invoke\('restore_folder', \{ repositoryPath: state\.repository\.path, relativePath: restoredPath, sourceRevision: model\.preview\.source_id, cleanPaths \}\)/);
  assert.match(html, /This does not move HEAD, switch branch, commit or push/);
});

test('folder restore gives visible progress and rechecks the restored folder after refresh', () => {
  assert.match(app, /function updateFolderRestoreActionState\(\)/);
  assert.match(app, /refs\.confirmFolderRestore\.textContent = model\.preview \? 'Restore folder' : 'Preview & restore'/);
  assert.match(app, /if \(!model\.preview\) \{[\s\S]*?await previewFolderRestore\(\);[\s\S]*?if \(!model\.preview\) \{ finishConfirmButton\(\); updateFolderRestoreActionState\(\); return; \}/);
  assert.match(app, /beginButtonOperation\(refs\.previewFolderRestore, 'Previewing…'\)/);
  assert.match(app, /status\(`Previewing restore for \$\{model\.entry\.name\}…`, 'busy'\)/);
  assert.match(app, /beginButtonOperation\(refs\.confirmFolderRestore, model\.preview \? 'Restoring…' : 'Previewing…'\)/);
  assert.match(app, /status\(`Restoring \$\{restoredName\} from \$\{sourceLabel\}…`, 'busy'\)/);
  assert.match(app, /await refreshStatusAndFolder\(state\.repository\.path, reopenPath\)/);
  assert.doesNotMatch(app, /await loadRepository\(state\.repository\.path, \{ reopenPath \}\);[\s\S]*?const freshEntry = state\.entries\.find\(entry => entry\.relative_path === restoredPath\)/);
  assert.match(app, /const freshEntry = state\.entries\.find\(entry => entry\.relative_path === restoredPath\);[\s\S]*?if \(freshEntry\) await selectEntry\(restoredPath\);/);
  assert.match(app, /function folderRestoreResultMessage\(name, sourceLabel, remainingCount\)/);
  assert.match(app, /restore finished, but \$\{remainingCount\} local change/);
  assert.match(app, /restored from \$\{sourceLabel\}\. \$\{remainingCount\} local change/);
});

test('graph exposes branch and commit context actions without relying on lane identity', () => {
  assert.match(app, /data-graph-ref-name="\$\{esc\(badge\.name\)\}"/);
  assert.match(app, /showGraphBranchContextMenu\(event, pill\.dataset\.graphRefName\)/);
  assert.match(app, /function graphBranchRefsForCommit\(commitId\)/);
  assert.match(app, /function graphMergeUnavailableReason\(branchName\)/);
  assert.match(app, /Checkout or switch to a branch first — HEAD is detached\./);
  assert.match(app, /Merge \$\{branchName\} into current branch/);
  assert.match(app, /\$\{branchName\} → \$\{current\}\. Current branch is the only branch changed\./);
  assert.match(app, /openMergeBranchDialog\(activeGraphMergeTarget\(\), branchName\)/);
  assert.match(app, /showGraphCommitContextMenu\(event, row\.dataset\.id\)/);
  assert.match(app, /const mergeItems = branchRefs\.map\(\(ref, index\) => graphMergeMenuItem\(ref\.name, `merge-\$\{index\}`\)\)/);
  assert.match(app, /Create branch from this commit/);
  assert.match(app, /Checkout this commit/);
  assert.match(app, /Restore exact checkpoint…/);
  assert.match(app, /Clean workspace to this commit/);
  assert.match(app, /invoke\('create_branch_at_commit', \{ repositoryPath: context\.path, branch: name\.trim\(\), commitId \}\)/);
  assert.match(app, /invoke\('checkout_commit', \{ repositoryPath: context\.path, commitId \}\)/);
  assert.match(app, /invoke\('restore_exact_checkpoint', \{ repositoryPath: context\.path, commitId \}\)/);
  assert.match(app, /forces submodules to the versions recorded by this checkpoint/);
  assert.match(app, /function updateMergeDirectionPreview\(\)/);
  assert.match(app, /Direction: \$\{source\} → \$\{target\}/);
  assert.match(app, /refs\.mergeBranchSource\.addEventListener\('change', updateMergeDirectionPreview\)/);
  assert.match(app, /Resolve with Git mergetool/);
  assert.match(app, /invoke\('open_merge_tool', \{ repositoryPath: conflictRepositoryPath\(target\), targetPath: target\.targetPath, relativePath: path \}\)/);
  assert.match(app, /Configured Git merge\.tool = \$\{tool\.trim\(\)\}\. Retrying…/);
  assert.match(app, /UNRESOLVED/);
  assert.match(app, /Resolved\/staged — ready to complete the merge/);
  assert.match(app, /invoke\('graph_head_main_merge_base', \{ repositoryPath: g\.path \}\)/);
  assert.match(app, /commonAncestorRows/);
  assert.match(app, /Branch start/);
  assert.match(app, /class="commit-date"/);
  assert.match(app, /data-copy-commit-sha/);
  assert.match(css, /\.commit-copy-sha/);
  assert.match(app, /function graphHeadBannerHtml\(g, currentBranch, headVisible\)/);
  assert.match(app, /data-jump-head/);
  assert.match(app, /function jumpToGraphHead\(\)/);
  assert.match(app, /YOU ARE HERE · HEAD/);
  assert.match(css, /\.head-location-pill/);
  assert.match(app, /Real merge-base between HEAD and \$\{esc\(commonAncestorBaseRef\)\}/);
  assert.match(app, /MERGE MAIN/);
  assert.match(app, /const palette = \[[\s\S]*?'#b4f1cf'[\s\S]*?\]/);
  assert.match(app, /class="graph-edge-underlay"/);
  assert.match(css, /\.graph-overlay \.graph-edge-underlay/);
});

test('every local frontend script and stylesheet carries the same cache-busting version', () => {
  // The WebView can keep serving an older cached copy of a same-named asset
  // after an app update. Every local <script src> / stylesheet href in
  // index.html therefore ends in ?v=<release tag>, and the tag must be identical
  // so a stale mix of old and new files can never load together. Bump the tag
  // in one place per release; a file added without it fails here.
  const refs = [...html.matchAll(/<(?:script[^>]*\ssrc|link[^>]*\shref)="([^"]+)"/g)].map(match => match[1])
    .filter(ref => !/^(?:https?:)?\/\//.test(ref) && /\.(?:js|css)(?:\?|$)/.test(ref));
  assert.ok(refs.length >= 9, `expected the local scripts and stylesheet, found ${refs.length}: ${refs.join(', ')}`);
  const versions = new Set();
  for (const ref of refs) {
    const match = ref.match(/^[\w./-]+\.(?:js|css)\?v=([\w.-]+)$/);
    assert.ok(match, `local asset "${ref}" must end with ?v=<release tag>`);
    versions.add(match[1]);
    assert.ok(fs.existsSync(path.join(root, 'frontend', ref.split('?')[0])), `"${ref}" points to a file that does not exist`);
  }
  assert.equal(versions.size, 1, `all assets must share one version tag, found: ${[...versions].join(', ')}`);
});

test('submodule stash is disabled without local changes, and stage/unstage failures are shown as a toast', () => {
  assert.match(app, /const canStashInsideSubmodule = entry\.kind === 'submodule' && entry\.submodule_is_dirty;/);
  assert.match(app, /data-detail-action="substash" \$\{canStashInsideSubmodule \? '' : 'disabled'\}/);
  const flush = app.match(/async function flushOneBatch\(options\) \{([\s\S]*?)\n\}\n/)?.[1] || '';
  assert.match(flush, /const message = handleError\(error\);\s*showOperationToast\(`Stage\/Unstage failed: \$\{message\}`, 'error'\);/);
});
