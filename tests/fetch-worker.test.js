const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const source = fs.readFileSync(path.join(__dirname, '../src-tauri/src/repository/remotes.rs'), 'utf8');

test('single-remote Fetch offloads the same synchronous body as Fetch All', () => {
  assert.match(source, /pub async fn fetch_remote\(repository_path: String, remote: String\) -> Result<\(\), String> \{\s*off_main_thread\(move \|\| fetch_remote_inner\(repository_path, remote\)\)\.await\s*}/);
  const inner = source.match(/fn fetch_remote_inner\([^]*?\n}/)[0];
  assert.match(inner, /validate_path\(&repository_path\)\?/);
  assert.match(inner, /repo\.find_remote\(remote_name\)/);
  assert.match(inner, /git\(&repository_path, &\["fetch", remote_name\]\)/);
  assert.match(inner, /invalidate_git_metadata\(&repository_path\)/);
  assert.equal((inner.match(/\bgit\(/g) || []).length, 1, 'no additional Git commands');
  assert.doesNotMatch(inner, /--force|--prune|--tags|--recurse-submodules|status|checkout/);
});

test('Pull reuses the fetch body inside its existing worker, preserving dirty-tree checks', () => {
  const sync = source.match(/fn sync_repository_inner\([^]*?\n}/)[0];
  assert.match(sync, /fetch_remote_inner\(repository_path\.clone\(\), remote_name\.into\(\)\)\?/);
  assert.doesNotMatch(sync, /block_on|\.await|\bfetch_remote\(/);
  assert.ok(sync.indexOf('if !dirty.is_empty()') < sync.indexOf('fetch_remote_inner('));
});
