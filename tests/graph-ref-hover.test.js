const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { selectRefBadges } = require('../frontend/graph-model.js');
const app = fs.readFileSync(require.resolve('../frontend/app.js'), 'utf8');
function render(refs, anchor = null, currentBranch = null) {
  const context = { selectRefBadges, state: { graphBranchCompareAnchor: anchor },
    activeRepositoryContext: () => ({}), graphBranchCompareAnchorMatches: () => !!anchor,
    REF_BADGE_ICON: { tag: '◆ ' } };
  vm.createContext(context);
  vm.runInContext(app.match(/^function esc\(.*$/m)[0], context);
  vm.runInContext(app.match(/function refsBadges\([^]*?\n}/)[0], context);
  return context.refsBadges(refs, false, currentBranch);
}

test('long remote and local graph refs expose the complete name and actions in a native hover title', () => {
  for (const kind of ['remote_branch', 'local_branch']) {
    const name = `${kind === 'remote_branch' ? 'company/' : ''}feature/very-long-module-integration-branch-name`;
    const html = render([{kind, name}]);
    assert.ok(html.includes(`title="${name}\nClick to select as compare start; right-click for actions"`));
    assert.ok(html.includes(`data-graph-ref-name="${name}"`));
    assert.doesNotMatch(html, /data-tooltip=/, 'CSS tooltips would be clipped by ellipsis containers');
  }
});

test('compare selection retains full names and guidance for both selected and target refs', () => {
  const refs = [{kind: 'local_branch', name: 'topic'}, {kind: 'remote_branch', name: 'company/long-target'}];
  const html = render(refs, {branch:'topic'});
  assert.match(html, /branch-ref-pill compare-start/);
  assert.ok(html.includes('title="topic\nCompare start selected'));
  assert.ok(html.includes('title="company/long-target\nClick to compare topic with this branch"'));
});

test('tags and overflow refs reveal actual complete names without widening graph rows', () => {
  const refs = [
    {kind:'local_branch',name:'main'}, {kind:'remote_branch',name:'origin/main'},
    {kind:'remote_branch',name:'company/feature/extra-long-remote'},
    {kind:'remote_branch',name:'fork/another-remote'},
    {kind:'tag',name:'release/very-long-version'}, {kind:'tag',name:'release/older'}, {kind:'tag',name:'release/oldest'},
  ];
  const html = render(refs, null, 'main');
  assert.ok(html.includes('title="release/very-long-version\nClick for tag details"'));
  assert.ok(html.includes('title="release/older\nrelease/oldest">+2 tags'));
  assert.ok(html.includes('title="company/feature/extra-long-remote\nfork/another-remote">+2'));
  assert.doesNotMatch(html, /style=|data-tooltip=/);
});

test('ref names and compare guidance remain escaped in attributes and markup', () => {
  const html = render([{kind:'remote_branch',name:'origin/feature/"<long>&'}], {branch:'local/"<branch>&'});
  assert.ok(html.includes('title="origin/feature/&quot;&lt;long&gt;&amp;\n'));
  assert.ok(html.includes('Click to compare local/&quot;&lt;branch&gt;&amp; with this branch'));
  assert.doesNotMatch(html, /<long>|<branch>/);
  assert.equal(render([]), '');
});
