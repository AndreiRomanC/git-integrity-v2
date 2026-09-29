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

test('Add submodule can browse GitHub Enterprise modules without changing the manual flow', () => {
  assert.match(html, /id="browseSubmoduleRepository"[^>]*>Browse…<\/button>/);
  assert.match(html, /id="submoduleBrowserDialog"/);
  assert.match(html, /GITHUB ENTERPRISE · ENG/);
  assert.match(app, /invoke\('search_github_modules', \{ repositoryPath: state\.repository\.path, query, limit: 25 \}\)/);
  assert.match(app, /const submoduleBrowserRefCache = new Map\(\)/);
  assert.match(app, /function filterLoadedSubmoduleRefs/);
  assert.match(app, /cachedSubmoduleRefs\(repo\)/);
  assert.match(app, /submoduleRefQueryLooksLikeSha/);
  assert.match(app, /invoke\('github_module_refs', \{ repositoryPath: state\.repository\.path, owner: repo\.owner \|\| 'eng', repositoryName: repo\.name, query: trimmedQuery, limit: 180 \}\)/);
  assert.match(app, /refs\.submoduleUrl\.value = repo\.portable_url/);
  assert.match(app, /initialRevision: selected\.revision \|\| null/);
  assert.match(app, /resetSubmoduleBrowseSelection\(\); if \(!refs\.submoduleName\.dataset\.edited\)/);
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
  assert.match(app, /function submoduleRepositoryLinkHtml\(entry\)/);
  assert.match(app, /function submoduleRepositoryRowsHtml\(entry\)/);
  assert.match(app, /return `<span>Remote<\/span><strong>\$\{remote\}<\/strong>`/);
  assert.doesNotMatch(app, /<span>GitHub<\/span>/);
  assert.match(app, /class="submodule-repository-link"/);
  assert.match(app, /entry\.submodule_web_url/);
  assert.match(app, /event\.target\.closest\('\.submodule-repository-link'\)/);
  assert.match(css, /\.spec-list code/);
  assert.match(css, /\.submodule-repository-link/);
});

test('folder and submodule notes stay outside Git and are rendered from an in-memory map', () => {
  assert.match(app, /drillDownNotes: \{\}/);
  assert.match(app, /function ensureDrillDownNotesLoaded\(repositoryPath, force = false\)/);
  assert.match(app, /if \(!force && state\.drillDownNotesRepositoryPath === repositoryPath\) return;/);
  assert.match(app, /invoke\('load_drill_down_notes', \{ repositoryPath \}\)/);
  assert.match(app, /function noteForPath\(path\)/);
  assert.match(app, /function entrySupportsPersonalNote\(entry\)/);
  assert.match(app, /\['folder', 'submodule'\]\.includes\(entry\.kind\)/);
  assert.match(app, /function entryHasPersonalNote\(entry\)/);
  assert.match(app, /function renderPersonalNoteSection\(entry\)/);
  assert.match(app, /NOTES/);
  assert.doesNotMatch(app, /PERSONAL NOTE/);
  assert.match(app, /invoke\('set_drill_down_note', \{ repositoryPath: state\.repository\.path, relativePath: entry\.relative_path, note: text \}\)/);
  assert.match(app, /invoke\('delete_drill_down_note', \{ repositoryPath: state\.repository\.path, relativePath: entry\.relative_path \}\)/);
  assert.match(app, /class="personal-note-dot"/);
  assert.match(css, /\.personal-note-section/);
  assert.match(css, /\.personal-note-header \{ display: flex;/);
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
  assert.match(app, /'\?\?': \{ code: 'UN', label: 'New file'/);
  assert.match(app, /A: \{ code: 'AL', label: 'Staged new file'/);
  assert.match(app, /function entryGitSummary\(entry\)/);
  assert.match(app, /if \(entry\.status === 'A'\) return 'Staged new file';/);
  assert.match(app, /<span>Git<\/span><strong>\$\{esc\(entryGitSummary\(entry\)\)\}<\/strong>/);
  assert.match(app, /entry\.kind !== 'submodule' && entry\.unpushed/);
  assert.match(app, /if \(!states\.length\) addState\('TR', 'Tracked'\)/);
  assert.match(app, /ML \+ ST \+ NP/);
});

test('brand-new untracked files and folders are labelled as new items in Explorer and details', () => {
  assert.match(app, /function untrackedItemLabel\(entry\)/);
  assert.match(app, /if \(entry\.kind === 'folder'\) return 'New folder';/);
  assert.match(app, /return 'New file';/);
  assert.match(app, /entryKindHint\(entry\)/);
  assert.match(app, /function changeDisplayState\(change\)/);
  assert.match(app, /if \(change\.status === '\?\?'\) return 'New file';/);
  assert.match(app, /if \(change\.status === 'A'\) return change\.staged \? 'Staged new file' : 'New file';/);
  assert.match(app, /This item is new on disk\. Stage it to include it in the next commit\./);
  assert.match(app, /\$\{untrackedItemLabel\(entry\)\} — not tracked yet/);
  assert.match(app, /entry\.status \|\| !entry\.tracked \? '<button data-detail-action="commit">Commit this item<\/button>'/);
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
  assert.match(app, /if \(entry\.kind === 'submodule'\) return submoduleHeadHint\(entry\);/);
  assert.match(app, /entry-hint">\$\{esc\(entryKindHint\(entry\)\)\}/);
  assert.match(app, /function submoduleCheckoutBadgeHtml\(entry\)/);
  assert.match(app, /On branch ·/);
  assert.match(app, /Detached HEAD/);
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

test('a selected submodule can be promoted to the normal full repository context', () => {
  assert.match(app, /function openSubmoduleAsFullRepository\(entry, button = null\)/);
  assert.match(app, /invoke\('resolve_submodule_repository', \{ repositoryPath: parentRepository\.path, relativePath: entry\.relative_path \}\)/);
  assert.match(app, /openRepositoryFast\(target\.path, \{[\s\S]*?origin:/);
  assert.match(app, /The parent gitlink was not changed/);
  assert.match(app, /data-detail-action="subopenfull"[\s\S]*?Open as Full Repository/);
  assert.match(html, /id="parentRepositoryButton"/);
  assert.match(app, /refs\.parentRepositoryButton\.addEventListener\('click'/);
  assert.match(app, /openRepositoryFast\(origin\.parentPath, \{ reopenPath: parentPathOf\(origin\.submodulePath\) \}\)/);
  assert.match(app, /Submodule of \$\{origin\.parentName\}/);
  assert.match(css, /\.parent-repository-button/);
  assert.match(css, /\.recent-repo-btn\.submodule-recent/);
  assert.match(css, /\.recent-repos-bar \{[^}]*height: 27px/);
  assert.match(css, /\.recent-repo-btn \{[^}]*max-width: 118px/);
  assert.match(css, /button\[data-detail-action="subopenfull"\] \{ flex: 1 1 100%;/);
  assert.doesNotMatch(css, /button\[data-detail-action="subopenfull"\]::before/);
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
  assert.match(css, /\.version-containing-branches \{[^}]*display: flex/);
  assert.match(css, /\.version-containing-branches \{[^}]*flex-direction: column/);
  assert.match(css, /\.version-containing-group h5/);
  assert.match(css, /\.version-containing-branch \{[^}]*grid-template-columns: minmax\(0, 1fr\) auto/);
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
  assert.match(app, /DEFAULT_SAVED_ACTIONS/);
  assert.match(app, /git-drilldown-seeded-saved-actions-v3/);
  assert.match(app, /Submodule update --init --recursive/);
  assert.match(app, /git submodule sync --recursive/);
  assert.match(app, /git submodule update --init --recursive/);
  assert.match(app, /git submodule status --recursive/);
  assert.match(app, /DANGER: clean workspace to current HEAD\/checkpoint/);
  assert.match(app, /git reset --hard HEAD/);
  assert.match(app, /git clean -fd -- :\//);
  assert.match(app, /git submodule update --init --recursive --force/);
  assert.match(app, /git submodule foreach --recursive "git reset --hard HEAD"/);
  assert.match(app, /Dangerous saved action/);
  assert.match(app, /skipDestructiveConfirm/);
  assert.match(app, /Compare branch with origin\/main/);
  assert.match(app, /git diff --stat origin\/main\.\.\.HEAD/);
  assert.match(app, /git diff --name-status origin\/main\.\.\.HEAD/);
  assert.match(app, /git log --oneline --decorate --left-right origin\/main\.\.\.HEAD/);
  assert.match(app, /Pre-merge check: origin\/main into current branch/);
  assert.match(app, /git fetch --all --prune/);
  assert.match(app, /git log --oneline --decorate --left-right --cherry-pick origin\/main\.\.\.HEAD/);
  assert.match(app, /git diff --submodule=log origin\/main\.\.\.HEAD/);
  assert.match(app, /git merge-tree --write-tree HEAD origin\/main/);
  assert.match(app, /function renderSavedActions/);
  assert.match(app, /function addOrEditSavedAction/);
  assert.match(app, /function runSavedAction/);
  assert.match(app, /commands to run/i);
  assert.match(app, /state\.savedActions\.forEach/);
  assert.match(app, /Saved action: \$\{action\.name\}/);
  assert.match(app, /data-saved-run/);
  assert.match(app, /saved-run-primary/);
  assert.match(css, /danger-saved-action/);
  assert.match(css, /danger-run-primary/);
  assert.match(app, /Saved action selected\. Press Run to execute it\./);
  assert.match(css, /\.saved-command-actions \.saved-run-primary/);
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
  assert.match(app, /const folderActions = entry\.kind === 'folder'[\s\S]*?data-detail-action="restorefolder"/);
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
  assert.match(app, /model\.loadingCommits = false;[\s\S]*?renderFolderRestoreCommits\(\)/);
});

test('graph exposes branch and commit context actions without relying on lane identity', () => {
  assert.match(app, /data-graph-ref-name="\$\{esc\(badge\.name\)\}"/);
  assert.match(app, /showGraphBranchContextMenu\(event, pill\.dataset\.graphRefName, pill\.dataset\.graphRefKind\)/);
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
  assert.match(html, /id="refreshConflicts"/);
  assert.match(app, /data-resolve="manualmark"/);
  assert.match(app, /markConflictResolvedAfterExternalTool/);
  assert.match(app, /Save the resolved file in the merge tool, then use “Mark resolved” or “Recheck conflicts”/);
  assert.match(app, /All conflicts are resolved and staged\. Complete the merge commit now\?/);
  assert.match(app, /await refreshConflictsDialog\(target, \{ focusNext: true \}\)/);
  assert.match(app, /Configured Git merge\.tool = \$\{tool\.trim\(\)\}\. Retrying…/);
  assert.match(app, /Recommended for Beyond Compare: bc/);
  assert.match(app, /Common names:[\s\S]*bc = Beyond Compare[\s\S]*winmerge[\s\S]*meld[\s\S]*kdiff3/);
  assert.doesNotMatch(app, /'bcomp'|"bcomp"/);
  assert.match(app, /function updateConflictSession\(target, conflicts = \[\]\)/);
  assert.match(app, /UNRESOLVED/);
  assert.match(app, /DONE/);
  assert.match(app, /Resolved and staged for the merge result\./);
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
  assert.match(app, /class="graph-current-jump"/);
  assert.match(app, /YOU ARE HERE · HEAD/);
  assert.match(css, /\.head-location-pill/);
  assert.match(css, /\.graph-current-jump/);
  assert.match(app, /Real merge-base between HEAD and \$\{esc\(commonAncestorBaseRef\)\}/);
  assert.match(app, /MERGE MAIN/);
  assert.match(app, /const palette = \[[\s\S]*?'#b4f1cf'[\s\S]*?\]/);
  assert.match(app, /class="graph-edge-underlay"/);
  assert.match(css, /\.graph-overlay \.graph-edge-underlay/);
});

test('graph search can optionally show only matches as a disconnected result list', () => {
  assert.match(app, /graphOnlySearchMatches: false/);
  assert.match(app, /data-graph-only-matches/);
  assert.match(app, /Only matches/);
  assert.match(app, /if \(onlySearchMatches\) model\.forEach\(node => \{ node\.lane = 0; node\.before = \[\]; node\.after = \[\]; node\.parents = \[\]; \}\)/);
  assert.match(app, /drawGraphOverlay\(model, lanesWidth, \{ disconnected: onlySearchMatches \}\)/);
  assert.match(app, /if \(!options\.disconnected\) \{/);
  assert.match(css, /\.graph-search-toggle/);
});

test('submodule compare can export both exact compared revisions as snapshots', () => {
  assert.match(html, /id="subCompareDownload"/);
  assert.match(app, /subCompareDownload: \$\('#subCompareDownload'\)/);
  assert.match(app, /function exportSubmoduleCompareSnapshots\(\)/);
  assert.match(app, /invoke\('choose_folder'\)/);
  assert.match(app, /invoke\('export_submodule_compare_snapshots', \{/);
  assert.match(app, /leftRef: compare\.leftRevision \|\| compare\.leftRef|const leftRef = compare\.leftRevision \|\| compare\.leftRef/);
  assert.match(app, /function defaultSubmoduleCompareRightRef\(data = \{\}\)/);
  assert.match(app, /if \(action === 'subcompare'\) return openSubmoduleCompareFromEntry\(entry, \{ presetCurrentRight: true \}\)/);
  assert.match(app, /if \(presetCurrentRight\) \{[\s\S]*?state\.submoduleCompare\.leftRef = hasExplicitLeftRef \? \(revisionOverrides\.leftRef \|\| ''\) : '';/);
  assert.match(app, /state\.submoduleCompare\.rightRef = hasExplicitRightRef \? \(revisionOverrides\.rightRef \|\| ''\) : defaultSubmoduleCompareRightRef\(data\)/);
  assert.match(app, /const shouldAutoCompare = Boolean\(state\.submoduleCompare\.leftRef && state\.submoduleCompare\.rightRef\)/);
  assert.match(app, /refs\.subRevisionSearch\.value = ''/);
  assert.match(app, /function optionMatchesSubmoduleRevisionRef\(option = \{\}, ref = ''\)/);
  assert.match(app, /const otherRef = picker\.side === 'left' \? state\.submoduleCompare\?\.rightRef : state\.submoduleCompare\?\.leftRef/);
  assert.match(app, /filter\(option => !optionMatchesSubmoduleRevisionRef\(option, otherRef\)\)/);
  assert.doesNotMatch(app, /state\.submoduleCompare\.leftRef = revisionOverrides\.leftRef \|\| state\.submoduleCompare\.leftRef \|\| data\.parent_revision/);
});

test('repository branch compare reuses the Compare and Sync ref-diff flow', () => {
  assert.match(html, /id="branchCompareLeftRef"/);
  assert.match(html, /id="branchCompareRightRef"/);
  assert.match(app, /gitCompareMode: 'workspace'/);
  assert.match(app, /compare_git_revisions_directory/);
  assert.match(app, /invoke\('compare_git_revision_file', \{/);
  assert.match(app, /id: 'compare-branch'[\s\S]*?openGraphBranchCompare\(branchName\)/);
  assert.match(app, /state\.gitCompareMode = 'refs'/);
});

test('graph branch menu can compare two explicitly selected branches', () => {
  assert.match(app, /graphBranchCompareAnchor: null/);
  assert.match(app, /function setGraphBranchCompareStart\(branchName\)/);
  assert.match(app, /Right-click another branch and choose Compare with start/);
  assert.match(app, /function openGraphBranchCompare\(branchName, rightRef = ''\)/);
  assert.match(app, /state\.branchCompareRightRef = rightRef \|\| defaultBranchCompareRightRef\(branchName\)/);
  assert.match(app, /function handleGraphBranchCompareSelection\(branchName\)/);
  assert.match(app, /handleGraphBranchCompareSelection\(pill\.dataset\.graphRefName\)/);
  assert.match(app, /querySelectorAll\('\.branch-row\[data-branch\]'\)[\s\S]*?addEventListener\('contextmenu'/);
  assert.match(app, /data-action="compare-start"/);
  assert.match(app, /data-action="compare-with-start"/);
  assert.doesNotMatch(app, /Compare branches…/);
  assert.doesNotMatch(app, /compare-branch-from-commit/);
  assert.match(app, /compare-start/);
  assert.match(app, /label: 'Set as compare start'/);
  assert.match(app, /label: 'Compare with start'[\s\S]*?openGraphBranchCompare\(anchor\.branch, branchName\)/);
  assert.match(css, /\.branch-ref-pill\.compare-start/);
  assert.match(css, /\.branch-row\.compare-start/);
});

test('graph commit menu can compare any two exact commits', () => {
  assert.match(app, /graphCommitCompareAnchor: null/);
  assert.match(app, /function graphCommitCompareAnchorMatches\(context, anchor = state\.graphCommitCompareAnchor\)/);
  assert.match(app, /function setGraphCommitCompareStart\(commitId\)/);
  assert.match(app, /Right-click any other commit and choose Compare with start/);
  assert.match(app, /function openGraphRevisionCompare\(leftRef, rightRef = ''\)/);
  assert.match(app, /state\.branchCompareLeftRef = leftRef/);
  assert.match(app, /state\.branchCompareRightRef = rightRef/);
  assert.match(app, /openSubmoduleCompareFromEntry\(entry, \{ leftRef, rightRef \}\)/);
  assert.match(app, /label: 'Set commit as compare start'/);
  assert.match(app, /label: 'Compare this commit with HEAD'/);
  assert.match(app, /label: 'Compare with start'[\s\S]*?openGraphRevisionCompare\(start, commitId\)/);
  assert.match(app, /\[state\.branchCompareLeftRef, state\.branchCompareRightRef\]\.forEach\(ref => add\(ref, 'revision'\)\)/);
  assert.match(app, /is-commit-compare-start/);
  assert.match(css, /\.commit-row\.is-commit-compare-start \.commit-card/);
});

test('compare views can switch to a filtered flat changed-file overview', () => {
  assert.match(html, /id="gitCompareFlatToggle"/);
  assert.match(html, /id="subCompareFlatToggle"/);
  assert.match(html, /id="gitCompareFlatFilter"/);
  assert.match(html, /id="subCompareFlatFilter"/);
  assert.match(app, /compareFlatMode: false/);
  assert.match(app, /subCompareFlatMode: false/);
  assert.match(app, /function compareRowVisible\(row, query = '', filter = 'all'\)/);
  assert.match(app, /compare_remote_file_list/);
  assert.match(app, /compare_git_revisions_file_list/);
  assert.match(app, /compare_submodule_revisions_file_list/);
  assert.match(app, /function flatCompareRowHtml\(row, datasetName\)/);
  assert.match(app, /flat-compare-row/);
  assert.match(css, /\.flat-compare-row/);
});

test('working tree drawer opens a working-tree versus index diff on double-click', () => {
  assert.match(app, /function openIndexWorktreeCompare\(change\)/);
  assert.match(app, /invoke\('compare_index_worktree_file'/);
  assert.match(app, /data-change-compare-path/);
  assert.match(app, /Double-click to compare working tree with the Git index/);
  assert.match(app, /setCompareHeadLabels\('WORKING TREE', 'INDEX'\)/);
});

test('main project merges ask before updating submodule working trees', () => {
  assert.match(app, /function parseChangedSubmodulePaths\(rawDiff = ''\)/);
  assert.match(app, /160000/);
  assert.match(app, /function maybeOfferSubmoduleUpdateAfterMerge\(target, beforeHead = ''\)/);
  assert.match(app, /target\?\.isSubmodule\) return/);
  assert.match(app, /Update submodules after merge\?/);
  assert.match(app, /git submodule update --init --recursive/);
  assert.match(app, /await maybeOfferSubmoduleUpdateAfterMerge\(target, beforeHead\)/);
  assert.match(app, /await maybeOfferSubmoduleUpdateAfterMerge\(target\)/);
  assert.match(app, /await initAndUpdateSubmodulesFromActions\(\)/);
});

test('main project merge pauses for one consolidated submodule pointer review', () => {
  assert.match(html, /id="submoduleMergeReviewDialog"/);
  assert.match(html, /Submodule merge review/);
  assert.match(html, /id="submoduleMergeReviewList"/);
  assert.match(app, /submoduleMergeReview: null/);
  assert.match(app, /invoke\('submodule_merge_review'/);
  assert.match(app, /invoke\('apply_submodule_merge_revision'/);
  assert.match(app, /outcome\.status === 'submodule_review'/);
  assert.match(app, /function openSubmoduleMergeReviewDialog\(target, message = ''\)/);
  assert.match(app, /Keep prepared result/);
  assert.match(app, /function maybeOfferSubmoduleUpdateBeforeMergeCommit/);
  assert.match(app, /Update submodules before build\?/);
  assert.match(app, /Use Current Branch/);
  assert.match(app, /Use Incoming\/origin/);
  assert.match(app, /pointerRevisionLabel/);
  assert.match(app, /submoduleReviewResultSource/);
  assert.match(app, /No submodule/);
  assert.match(app, /selectedSource/);
  assert.match(app, /applySubmoduleMergeReviewOnly/);
  assert.match(html, /Apply choices only/);
  assert.match(html, /Merge and commit/);
  assert.match(app, /Choose another commit/);
  assert.match(app, /Ready for submodule review \/ merge commit/);
  assert.match(css, /\.submodule-merge-review-dialog/);
  assert.match(css, /\.submodule-review-pointers/);
  assert.match(css, /\.submodule-review-pointers div\.absent/);
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

test('checkbox stage flush waits only for index writes, not for the slow visual refresh', () => {
  assert.match(app, /async function refreshStatusAndFolderInBackground\(repositoryPath, folder, reason = 'stage'\)/);
  assert.match(app, /refreshStatusAndFolderInBackground\(repositoryPath, folder, `checkbox generation=\$\{generation\}`\)/);
  assert.match(app, /flushOneBatch scheduled background refresh/);
  assert.doesNotMatch(app, /await refreshStatusAndFolder\(repositoryPath, folder\);\n\s*jsPerfLog\(`flushOneBatch refresh/);
  assert.match(app, /if \(state\.currentPath === folder\) await openDirectory\(folder, \{ force: true \}\);/);
});

test('detached dirty submodules ask for a branch before commit or push', () => {
  assert.match(app, /const detachedDirtySubmodule = entry\.kind === 'submodule' && entry\.submodule_initialized !== false && !submoduleBranchName && entry\.submodule_is_dirty;/);
  assert.match(app, /Detached HEAD/);
  assert.match(app, /Commit\/Push needs a local branch/);
  assert.match(app, /data-detail-action="subnewbranch">Create branch here…/);
  assert.match(css, /\.detached-work-banner \{ display: grid; grid-template-columns: 10px minmax\(0, 1fr\)/);
  assert.match(css, /\.detached-work-banner button \{ grid-column: 2;/);
  assert.match(app, /Commit requires an attached local branch/);
  assert.match(app, /Push requires an attached local branch/);
});

test('graph branch context menus can attach detached HEAD to a local branch', () => {
  assert.match(app, /function graphCheckoutBranchMenuItem\(branchName, kind = 'local_branch', id = 'checkout-branch'\)/);
  assert.match(app, /Remote-tracking refs cannot be checked out directly here/);
  assert.match(app, /Attach detached HEAD to this local branch/);
  assert.match(app, /openGraphBranchCompare\(branchName\)/);
  assert.match(app, /id: 'compare-branch'[\s\S]*?graphCheckoutBranchMenuItem\(branchName, kind\)[\s\S]*?graphMergeMenuItem\(branchName\)/);
  assert.match(app, /const checkoutItems = branchRefs[\s\S]*?filter\(ref => ref\.kind === 'local_branch'\)[\s\S]*?graphCheckoutBranchMenuItem\(ref\.name, ref\.kind, `checkout-\$\{index\}`\)/);
  assert.match(app, /menuItems\.push\(\.\.\.checkoutItems, \{ separator: true \}\)/);
  assert.match(app, /item\.separator[\s\S]*?floating-menu-separator/);
  assert.match(css, /\.floating-menu-separator/);
  assert.match(css, /\.floating-action-menu strong \{ display: block; font-size: 11px;/);
  assert.match(app, /showGraphBranchContextMenu\(event, pill\.dataset\.graphRefName, pill\.dataset\.graphRefKind\)/);
});

test('a submodule opened as a full repository cannot commit or publish from detached HEAD', () => {
  assert.match(app, /function detachedHeadWorkMessage\(action = 'publish'\)/);
  assert.match(app, /Detached commits are easy to lose and cannot be shown as normal Unpublished branch commits/);
  assert.match(app, /function blockDetachedHeadWork\(action = 'publish'\)/);
  assert.match(app, /if \(state\.repository\.head_detached\) \{/);
  assert.match(app, /Detached HEAD — create\/switch to a branch before publishing/);
  assert.match(app, /refs\.commitButton\.disabled = !staged \|\| !refs\.commitMessage\.value\.trim\(\) \|\| detached/);
  assert.match(app, /if \(blockDetachedHeadWork\('publish'\)\) return;/);
  assert.match(app, /if \(blockDetachedHeadWork\('commit'\)\) return;/);
});
