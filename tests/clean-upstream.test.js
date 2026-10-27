const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
function harness() {
  const active = {path:'/parent/module', currentBranch:'my-main', headOid:'a'.repeat(40), headDetached:false,
    branches:[{name:'my-main', remote:false, upstream:'company/main'}, {name:'other',remote:false,upstream:'origin/main'}]};
  const calls = [], confirmations = [], refresh = [];
  const c = {repoOpenGeneration:4, activeRepositoryContext:()=>active, status(){}, showOperationToast(){}, handleError(e){calls.push(['error',String(e)]);},
    customConfirm:async (...args)=>{confirmations.push(args);return true;},
    invoke:async (cmd,args)=>{calls.push([cmd,args]);return {branch:args.branch,upstream:args.upstream,revision:'b'.repeat(40)};},
    refreshAfterGraphCommitAction:async path=>refresh.push(path)};
  vm.createContext(c);
  for (const name of ['cleanUpstreamMenuItems','cleanBranchFromUpstream']) vm.runInContext(app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))[0],c);
  return {c,active,calls,confirmations,refresh};
}
test('red action uses configured upstream, including differently named branch and remote', async () => {
  const {c,active,calls,confirmations,refresh} = harness();
  for (const ref of [{kind:'local_branch',name:'my-main'},{kind:'remote_branch',name:'company/main'}]) {
    const [item] = c.cleanUpstreamMenuItems(active,[ref]);
    assert.equal(item.danger,true); assert.match(item.label,/my-main from company\/main/);
    await item.run();
  }
  assert.equal(calls.length,2);
  assert.equal(calls[0][0],'clean_branch_from_upstream');
  assert.equal(calls[0][1].repositoryPath,'/parent/module');
  assert.equal(calls[0][1].branch,'my-main');
  assert.equal(calls[0][1].upstream,'company/main');
  assert.equal(calls[0][1].expectedHead,active.headOid);
  assert.equal(confirmations[0][1].danger,true);
  assert.match(confirmations[0][0],/No Push; nothing on the server is changed/);
  assert.match(confirmations[0][0],/local-only commits are removed/);
  assert.equal(refresh.length,2);
});
test('unrelated refs, detached HEAD and missing upstream never offer destructive reset', () => {
  const {c,active} = harness();
  assert.equal(c.cleanUpstreamMenuItems(active,[{kind:'remote_branch',name:'origin/main'}]).length,0);
  active.headDetached=true;
  assert.equal(c.cleanUpstreamMenuItems(active,[{kind:'local_branch',name:'my-main'}]).length,0);
  active.headDetached=false; active.branches[0].upstream=null;
  assert.equal(c.cleanUpstreamMenuItems(active,[{kind:'local_branch',name:'my-main'}]).length,0);
});
test('cancel or selection drift during confirmation cannot clean a different context', async () => {
  for (const change of ['cancel','generation','head','upstream','path']) {
    const {c,active,calls} = harness();
    const [item] = c.cleanUpstreamMenuItems(active,[{kind:'local_branch',name:'my-main'}]);
    c.customConfirm=async()=>{
      if(change==='generation') c.repoOpenGeneration++;
      if(change==='head') active.headOid='new';
      if(change==='upstream') active.branches[0].upstream='other/main';
      if(change==='path') active.path='/different';
      return change!=='cancel';
    };
    await item.run(); assert.equal(calls.length,0,change);
  }
});
test('partial failure refreshes current state and reports the original error', async () => {
  const {c,active,calls,refresh} = harness();
  c.invoke=async()=>{throw Error('Submodule update failed; detached workspace');};
  await c.cleanUpstreamMenuItems(active,[{kind:'local_branch',name:'my-main'}])[0].run();
  assert.equal(refresh.length,1); assert.match(calls[0][1],/Submodule update failed/);
});
test('new command is registered and uses the centralized mutation guard', () => {
  assert.match(app.split(']);')[0],/'clean_branch_from_upstream'/);
  const rust=fs.readFileSync(require('node:path').join(__dirname,'../src-tauri/src/lib.rs'),'utf8');
  assert.match(rust,/repository::branches::clean_branch_from_upstream/);
});
