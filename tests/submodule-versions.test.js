// Automated tests for frontend/submodule-versions.js — run with:
//   node --test
// Same zero-dependency setup as tests/graph-model.test.js — see its own
// banner comment for why this never touches the shipped app's own
// "no Node.js/npm runtime or build step" promise.
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { groupSubmoduleBranchVersions, submoduleCurrentPresentation, submoduleContainingBranchCandidates, submoduleCurrentContextHtml, matchesSubmoduleVersion, submoduleVersionRowHtml, submodulePushDialogState } = require('../frontend/submodule-versions.js');

function branch(name, upstream, revision = '11111111aaaa') { return { name, kind: 'branch', upstream: upstream || null, revision }; }
function remote(name, revision = '11111111aaaa') { return { name, kind: 'remote', revision }; }

test('a local branch with a tracked upstream hides the matching remote-tracking entry', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('develop', 'origin/develop'),
    remote('origin/develop'),
  ]);
  assert.equal(local.length, 1);
  assert.equal(local[0].name, 'develop');
  assert.deepEqual(remoteOnly, [], 'origin/develop is already represented via develop\'s own upstream');
});

test('a remote-tracking branch with no local counterpart lands in remoteOnly', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('main', 'origin/main'),
    remote('origin/main'),
    remote('origin/feature/never-checked-out'),
  ]);
  assert.equal(local.length, 1);
  assert.equal(remoteOnly.length, 1);
  assert.equal(remoteOnly[0].name, 'origin/feature/never-checked-out');
});

test('a local branch with no upstream configured groups a same-named remote only when it is the same commit', () => {
  // Same shorthand name locally and on the remote, no upstream configured yet,
  // but both refs point at the same commit. This is the common result of
  // checking out origin/release and ending up on a local release branch before
  // upstream has been configured; showing origin/release under "REMOTE ONLY"
  // makes it look like a second unrelated branch.
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('release', null, 'aaaaaaaa1111'),
    remote('origin/release', 'aaaaaaaa1111'),
  ]);
  assert.equal(local.length, 1);
  assert.equal(local[0].same_name_remote, 'origin/release');
  assert.deepEqual(remoteOnly, []);
});

test('a local branch with no upstream configured does not hide a same-named remote at a different commit', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('release', null, 'aaaaaaaa1111'),
    remote('origin/release', 'bbbbbbbb2222'),
  ]);
  assert.equal(local.length, 1);
  assert.equal(local[0].same_name_remote, undefined);
  assert.equal(remoteOnly.length, 1, 'a same-named remote at a different commit must remain visible for inspection');
});

test('a local branch can track a differently-named remote branch', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('work', 'release/main'),
    remote('release/main'),
  ]);
  assert.equal(local.length, 1);
  assert.deepEqual(remoteOnly, []);
});

test('tags and commits are ignored entirely — this only ever groups branch/remote entries', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('main', 'origin/main'),
    remote('origin/main'),
    { name: 'v1.0', kind: 'tag' },
    { name: 'abc1234', kind: 'commit' },
  ]);
  assert.equal(local.length, 1);
  assert.deepEqual(remoteOnly, []);
});

test('tolerates a missing/undefined list without throwing', () => {
  assert.deepEqual(groupSubmoduleBranchVersions(undefined), { local: [], remoteOnly: [] });
  assert.deepEqual(groupSubmoduleBranchVersions([]), { local: [], remoteOnly: [] });
});

test('a restored detached checkout is clearly separated from saved divergent branches', () => {
  const restored = submoduleCurrentPresentation({ current_revision: '30402ecb1234', current_branch: '', parent_revision: '30402ecb1234' });
  assert.equal(restored.text, 'Detached HEAD @ 30402ecb');
  assert.match(restored.help, /parent project records this exact commit/i);
  assert.match(restored.help, /saved pointers, not the active checkout/);

  const arbitraryDetached = submoduleCurrentPresentation({ current_revision: '111111111234', current_branch: '', parent_revision: '222222221234' });
  assert.equal(arbitraryDetached.text, 'Detached HEAD @ 11111111');
  assert.match(arbitraryDetached.help, /project records 22222222/);
});

test('detached current context never presents an inactive synchronized branch as HEAD', () => {
  const html = submoduleCurrentContextHtml({
    current_revision: 'e00bbe2c1234', current_branch: '', parent_revision: 'e00bbe2c1234',
    current_containing_branches: [],
    versions: [
      { kind: 'branch', name: 'IMS.VITESCO.IO_master', revision: 'dfc890bd1234' },
      { kind: 'commit', name: 'e00bbe2c', revision: 'e00bbe2c1234', subject: 'Migration SDA 12 to SDA 13' },
    ],
  });
  assert.match(html, /ACTIVE CHECKOUT/);
  assert.match(html, /e00bbe2c/);
  assert.match(html, /DETACHED HEAD/);
  assert.match(html, /not reachable from currently known/i);
  assert.doesNotMatch(html, /Attached to branch IMS/);
});

test('detached context distinguishes a containing branch from an exact branch tip', () => {
  const contained = submoduleCurrentContextHtml({
    current_revision: '11111111aaaa', current_branch: '', parent_revision: '22222222bbbb',
    current_containing_branches: ['develop', 'origin/develop'], versions: [
      { kind: 'commit', revision: '11111111aaaa', subject: 'Older point in develop' },
      { kind: 'branch', name: 'develop', revision: '33333333cccc' },
    ],
  });
  assert.match(contained, /Detached inside 1 known branch/);
  assert.match(contained, /⑂ develop/);
  assert.match(contained, /Checkout branch tip/);

  const exact = submoduleCurrentContextHtml({
    current_revision: '44444444aaaa', current_branch: '', parent_revision: '44444444aaaa',
    current_containing_branches: ['main'], versions: [{ kind: 'branch', name: 'main', revision: '44444444aaaa' }],
  });
  assert.match(exact, /Detached at the tip of: main/);
});

test('detached context offers explicit switch actions ordered by nearest useful branch', () => {
  const data = {
    current_revision: '11111111aaaa', current_branch: '', parent_revision: '11111111aaaa',
    current_containing_branches: ['IMS.VITESCO.IO_master', 'feature/errm_common_hip', 'origin/feature/errm_common_hip'],
    versions: [
      { kind: 'branch', name: 'IMS.VITESCO.IO_master', revision: '99999999aaaa', contains_current: true, commits_after_current: 12 },
      { kind: 'branch', name: 'feature/errm_common_hip', revision: '22222222aaaa', upstream: 'origin/feature/errm_common_hip', contains_current: true, commits_after_current: 1 },
      { kind: 'remote', name: 'origin/feature/errm_common_hip', revision: '22222222aaaa', contains_current: true, commits_after_current: 1 },
      { kind: 'commit', revision: '11111111aaaa', current: true, subject: 'Current detached change' },
    ],
  };
  const candidates = submoduleContainingBranchCandidates(data);
  assert.deepEqual(candidates.map(item => item.name), ['feature/errm_common_hip', 'IMS.VITESCO.IO_master']);
  const html = submoduleCurrentContextHtml(data);
  assert.ok(html.indexOf('feature/errm_common_hip') < html.indexOf('IMS.VITESCO.IO_master'));
  assert.match(html, /BRANCHES CONTAINING THIS COMMIT/);
  assert.match(html, /Branch tip is 1 commit newer/);
  assert.match(html, /Checkout branch tip/);
  assert.match(html, />LOCAL</);
  assert.equal((html.match(/data-switch-version/g) || []).length, 2);
});

test('remote-only containing refs are clearly labeled and never pretend to attach HEAD', () => {
  const html = submoduleCurrentContextHtml({
    current_revision: '11111111aaaa', current_branch: '', parent_revision: '11111111aaaa',
    current_containing_branches: ['origin/release'],
    versions: [
      { kind: 'remote', name: 'origin/release', revision: '22222222aaaa', contains_current: true, commits_after_current: 2 },
      { kind: 'commit', revision: '11111111aaaa', current: true, subject: 'Detached change' },
    ],
  });
  assert.match(html, /version-containing-branch remote/);
  assert.match(html, />REMOTE</);
  assert.match(html, /Checkout remote tip/);
  assert.doesNotMatch(html, /Attach to branch/);
});

test('a commit row names branches that contain it without claiming attachment', () => {
  const html = submoduleVersionRowHtml({
    name: '11111111', kind: 'commit', revision: '11111111aaaa', current: true,
    subject: 'Detached change', author: 'A', date: '2026-09-15',
    containing_branches: ['feature/errm_common_hip', 'origin/feature/errm_common_hip'],
  });
  assert.match(html, /contained in ⑂ feature\/errm_common_hip, origin\/feature\/errm_common_hip/);
  assert.match(html, /COMMIT \(detached\)/);
});

test('this also answers "what branch is it on" for an older commit, not only the active checkout', () => {
  const html = submoduleVersionRowHtml({
    name: '22222222', kind: 'commit', revision: '22222222bbbb', current: false,
    subject: 'An older commit further back in History', author: 'A', date: '2026-08-01',
    containing_branches: ['develop'],
  });
  assert.match(html, /contained in ⑂ develop/);
});

test('a commit that no known branch contains says so plainly, instead of silently omitting the row context', () => {
  const html = submoduleVersionRowHtml({
    name: '33333333', kind: 'commit', revision: '33333333cccc', current: false,
    subject: 'Only reachable through a tag', author: 'A', date: '2025-01-01',
    containing_branches: [],
  });
  assert.match(html, /no known branch contains this commit/);
});

test('submodule search includes SHA, subject, author and date from the loaded response', () => {
  const item = { name: 'develop', revision: 'e00bbe2c1234', subject: 'Migration SDA 13', author: 'Marta Felicia', date: '2025-08-19', upstream: 'origin/develop' };
  for (const query of ['e00bbe2c', 'migration', 'marta', '2025-08-19', 'origin/develop']) assert.equal(matchesSubmoduleVersion(item, query), true);
  assert.equal(matchesSubmoduleVersion(item, 'not-present'), false);
});

test('a branch row shows the exact tip SHA and subject from the existing response', () => {
  const html = submoduleVersionRowHtml({ name: 'develop', kind: 'branch', revision: 'a5cf438372b9568c', subject: 'test32', author: 'Andrei', date: '2026-09-12', current: true, upstream: 'origin/develop', ahead: 1, behind: 0 });
  assert.match(html, /<code>a5cf4383<\/code>/);
  assert.match(html, /test32/);
  assert.match(html, /origin\/develop/);
  assert.match(html, /CURRENT/);
  assert.doesNotMatch(html, /data-switch-version/);
});

test('a tag row shows the target commit SHA and subject', () => {
  const html = submoduleVersionRowHtml({ name: 'v1.0.0', kind: 'tag', revision: '72a991227b54aaaf', subject: 'Release desktop UI', author: 'Andrei', date: '2026-09-12', current: false, attached_branch: 'main' });
  assert.match(html, /v1\.0\.0/);
  assert.match(html, /<code>72a99122<\/code>/);
  assert.match(html, /Release desktop UI/);
});

test('a history commit offers tagging that exact commit', () => {
  const html = submoduleVersionRowHtml({ name: '12345678', kind: 'commit', revision: '1234567890ab', subject: 'Release candidate', author: 'A', date: '2026-09-16' });
  assert.match(html, /data-tag-version/);
  assert.match(html, /Tag this commit…/);
  assert.match(html, /commit 12345678/);
});

test('identically named refs from different submodules keep their own SHAs', () => {
  const first = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: '11111111aaaaaaaa', subject: 'First module', author: 'A', date: '2026-09-12' });
  const second = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: '22222222bbbbbbbb', subject: 'Second module', author: 'B', date: '2026-09-12' });
  assert.match(first, /11111111/);
  assert.doesNotMatch(first, /22222222/);
  assert.match(second, /22222222/);
  assert.doesNotMatch(second, /11111111/);
});

test('row rendering is pure and does not invoke a backend loader', () => {
  let requests = 0;
  global.invoke = () => { requests += 1; };
  submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: 'abcdef1234567890', subject: 'No I/O', author: 'A', date: '2026-09-12' });
  delete global.invoke;
  assert.equal(requests, 0);
});

test('push stays enabled when an existing origin branch is ahead but has no configured upstream', () => {
  const state = submodulePushDialogState({
    branch: 'develop', local_sha: '2ab9a609e2b7', will_create_remote_branch: false,
    ahead: 2, behind: 0, can_push: true, blocked_reason: null,
    commits: [{ id: 'a5cf4383', subject: 'test32' }, { id: '2ab9a609', subject: 'test 32' }],
  });
  assert.equal(state.canPush, true);
  assert.equal(state.commits.length, 2);
  assert.equal(state.summary, '2 commits to push');
});

test('new remote branch creation is enabled even with no comparison commit list', () => {
  const state = submodulePushDialogState({ branch: 'feature/new', local_sha: '1234567890ab', will_create_remote_branch: true, can_push: true, commits: [] });
  assert.equal(state.canPush, true);
  assert.equal(state.summary, 'Create origin/feature/new');
  assert.match(state.emptyMessage, /create origin\/feature\/new at 12345678/);
});

test('an up-to-date submodule keeps normal push disabled with the backend reason', () => {
  const state = submodulePushDialogState({ branch: 'develop', local_sha: '2ab9a609', will_create_remote_branch: false, can_push: false, blocked_reason: 'Already up to date with origin/develop.', commits: [] });
  assert.equal(state.canPush, false);
  assert.equal(state.commits.length, 0);
  assert.equal(state.emptyMessage, 'Already up to date with origin/develop.');
});

test('branch rows explain ahead and behind as branch-tip state and name the compared upstream', () => {
  const ahead = submoduleVersionRowHtml({ name: 'develop', kind: 'branch', revision: 'aaaaaaaa1234', subject: 'Local work', author: 'A', date: '2026-09-13', upstream: 'origin/develop', ahead: 2, behind: 0 });
  assert.match(ahead, /BRANCH TIP AHEAD · 2 commits to push · origin\/develop/);
  assert.match(ahead, /Branch develop compared with origin\/develop/);

  const behind = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: 'bbbbbbbb1234', subject: 'Old local tip', author: 'A', date: '2026-09-13', upstream: 'origin/main', ahead: 0, behind: 1 });
  assert.match(behind, /BRANCH TIP BEHIND · 1 commit to pull · origin\/main/);

  const diverged = submoduleVersionRowHtml({ name: 'release', kind: 'branch', revision: 'cccccccc1234', subject: 'Both moved', author: 'A', date: '2026-09-13', upstream: 'origin/release', ahead: 3, behind: 4 });
  assert.match(diverged, /BRANCH DIVERGED · 3 ahead \/ 4 behind · origin\/release/);

  const inactive = submoduleVersionRowHtml({ name: 'IMS.VITESCO.IO_master', kind: 'branch', revision: 'dfc890bd1234', subject: 'Branch tip', author: 'A', date: '2024-12-06', upstream: 'origin/IMS.VITESCO.IO_master', ahead: 0, behind: 0, checkout_detached: true });
  assert.match(inactive, /BRANCH TIP · matches upstream · origin\/IMS\.VITESCO\.IO_master/);
  assert.match(inactive, /INACTIVE/);
});

test('destructive remote matching is offered only for the active branch', () => {
  const current = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: 'aaaaaaaa1234', subject: 'Local', author: 'A', date: '2026-09-13', current: true, upstream: 'origin/main', ahead: 1, behind: 1 });
  assert.match(current, /data-reset-upstream/);
  assert.match(current, /Discard local work…/);
  assert.match(current, /Destructive recovery/);
  assert.doesNotMatch(current, /data-switch-version/);

  const other = submoduleVersionRowHtml({ name: 'develop', kind: 'branch', revision: 'bbbbbbbb1234', subject: 'Other', author: 'A', date: '2026-09-13', current: false, upstream: 'origin/develop', ahead: 1, behind: 1 });
  assert.doesNotMatch(other, /data-reset-upstream/);
  assert.match(other, /data-switch-version/);

  const synced = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: 'cccccccc1234', subject: 'Synced', author: 'A', date: '2026-09-13', current: true, upstream: 'origin/main', ahead: 0, behind: 0 });
  assert.doesNotMatch(synced, /data-reset-upstream/);

  // Report: "BRANCH TIP AHEAD · N commits to push" next to the only button
  // being the destructive "Discard local work…" read as if throwing the new
  // commit away were the suggested move. The constructive counterpart must
  // be offered too, for the active branch, whenever there is something to
  // push — regardless of whether it's also behind (diverged, like `current`
  // here: ahead=1, behind=1) — never for an inactive row, and never when
  // there is nothing committed yet to send (synced or purely dirty).
  assert.match(current, /data-push-version/, 'ahead>0 on the active branch must also offer Push, not only Discard');
  assert.match(current, /Send 1 local commit on main to origin\/main/);
  assert.doesNotMatch(other, /data-push-version/, 'push is only offered for the currently active branch');
  assert.doesNotMatch(synced, /data-push-version/, 'nothing to push when ahead is 0');

  // Report: a submodule with only uncommitted local edits (no divergent
  // commits at all — ahead/behind both 0) had no way to reach "Discard
  // local work…" from here, even though the backend already handles this
  // exact case (see reset_submodule_branch_to_upstream_inner's own dirty
  // handling) whenever the target is the currently active branch.
  const dirtyOnly = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: 'eeeeeeee1234', subject: 'Dirty but not diverged', author: 'A', date: '2026-09-13', current: true, upstream: 'origin/main', ahead: 0, behind: 0, dirty: true });
  assert.match(dirtyOnly, /data-reset-upstream/, 'a purely dirty active branch must still offer Discard local work, not only a committed-ahead/behind one');
  assert.match(dirtyOnly, /a purely dirty working tree with no divergent commits/);
  assert.doesNotMatch(dirtyOnly, /data-push-version/, 'uncommitted edits are not something git push can send — nothing committed yet to push');

  // Report: "BRANCH TIP · matches upstream" right next to "Discard local
  // work…" read as a contradiction — nothing in the row itself said why the
  // button was still there (only the button's own hover tooltip did). The
  // dirty case must say so plainly in the row; the clean/synced case above
  // must not claim it.
  assert.match(dirtyOnly, /class="version-dirty-note"[^>]*>● Uncommitted changes present</);
  assert.doesNotMatch(synced, /version-dirty-note/);
});

test('a branch without upstream is explicitly local and does not show invented counts', () => {
  const row = submoduleVersionRowHtml({ name: 'work', kind: 'branch', revision: 'dddddddd1234', subject: 'Work', author: 'A', date: '2026-09-13', upstream: null, ahead: null, behind: null });
  assert.match(row, /LOCAL · no upstream configured/);
  assert.doesNotMatch(row, /0 ahead|0 behind|in sync/);
});

test('a no-upstream branch can still show the same origin branch without duplicating it as remote-only', () => {
  const row = submoduleVersionRowHtml({
    name: 'Test_branch_1', kind: 'branch', revision: '0d2b51611234',
    subject: 'Test-123-b1', author: 'Andrei', date: '2026-08-18',
    same_name_remote: 'origin/Test_branch_1', same_name_remote_revision: '0d2b51611234',
  });
  assert.match(row, /LOCAL \+ ORIGIN · same commit · origin\/Test_branch_1 · upstream not configured/);
  assert.doesNotMatch(row, /LOCAL · no upstream configured/);
});
