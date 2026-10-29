const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const rust = fs.readFileSync(path.join(__dirname,'../src-tauri/src/repository.rs'),'utf8');
const app = fs.readFileSync(path.join(__dirname,'../frontend/app.js'),'utf8');
const html = fs.readFileSync(path.join(__dirname,'../frontend/index.html'),'utf8');

test('publish diagnostics keep the same Git refspec, authentication and safety decisions', () => {
  const publish = rust.slice(rust.indexOf('fn publish_branch_inner_with_options'), rust.indexOf('pub async fn submodule_repository'));
  assert.match(publish, /\["push", remote_name, &format!\("\{push_oid\}:refs\/heads\/\{branch\}"\)\]/);
  assert.match(publish, /submodule_publish_safety_check/);
  assert.doesNotMatch(publish, /--no-verify|--force|--no-recurse-submodules/);
  assert.match(publish, /authenticated_push_options_traced/);
  assert.match(publish, /skip_submodule_safety/);
});

test('publish preview accurately names local reads; passive diagnostic loads before app', () => {
  assert.doesNotMatch(app, /Checking server state/);
  assert.match(app, /Reading local commits and cached remote refs/);
  assert.ok(html.indexOf('src="ipc-diagnostics.js') < html.indexOf('src="app.js'));
});
