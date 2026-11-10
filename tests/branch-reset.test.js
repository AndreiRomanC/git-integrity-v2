const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const api = require('../frontend/branch-reset.js');
const app = fs.readFileSync(require.resolve('../frontend/app.js'), 'utf8');
const report = () => ({ branch:'topic', head:'b'.repeat(40), target:'a'.repeat(40), subject:'Older commit',
  relation:'backward', removed_commits:1, upstream:'company/main', push_status:'non_fast_forward' });
const source = name => app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))[0];
function harness() {
  const active = {path:'/repo/module',name:'module',currentBranch:'topic',headOid:'b'.repeat(40),headDetached:false,isSubmodule:true};
  const calls=[],messages=[],errors=[],refreshes=[];
  const c={repoOpenGeneration:7,branchResetActive:false,activeRepositoryContext:()=>active,$:()=>({}),
    window:{GDDBranchReset:{choose:async()=> 'soft'}},
    status:(...args)=>messages.push(args),showOperationToast(){},handleError:e=>errors.push(String(e)),
    invoke:async(command,args)=>{calls.push([command,args]);return command==='preview_branch_reset' ? report() : undefined;},
    refreshAfterGraphCommitAction:async path=>refreshes.push(path)};
  vm.createContext(c);
  for(const name of ['branchResetSelectionMatches','branchResetMenuItem','openBranchReset']) vm.runInContext(source(name),c);
  return {c,active,calls,messages,errors,refreshes,open:()=>c.branchResetMenuItem(active,'refs/heads/other','other').run()};
}
test('reset is opt-in, on the current branch, and pins the target commit from the read-only preview',async()=>{
  const h=harness(); const item=h.c.branchResetMenuItem(h.active,'refs/heads/other','other');
  assert.equal(h.calls.length,0); assert.equal(item.danger,true); assert.match(item.label,/Reset topic to other/);
  await item.run();
  assert.deepEqual(h.calls.map(([cmd])=>cmd),['preview_branch_reset','reset_current_branch']);
  assert.equal(h.calls[1][1].branch,'topic'); assert.equal(h.calls[1][1].repositoryPath,'/repo/module');
  assert.equal(h.calls[1][1].targetCommit,'a'.repeat(40)); assert.equal(h.calls[1][1].expectedHead,'b'.repeat(40));
  assert.equal(h.refreshes.length,1);
});
test('no upstream is needed; detached HEAD has an explanatory disabled action',()=>{
  const h=harness(); assert.equal(h.c.branchResetMenuItem(h.active,'a').disabled,false);
  h.active.headDetached=true;
  const item=h.c.branchResetMenuItem(h.active,'a'); assert.equal(item.disabled,true); assert.match(item.detail,/detached HEAD/);
});
test('cancel, reopened repository, changed branch, changed HEAD and repository switch cannot reset',async()=>{
  for(const mutation of ['cancel','generation','branch','head','path']) {
    const h=harness(); h.c.window.GDDBranchReset.choose=async()=>{
      if(mutation==='generation') h.c.repoOpenGeneration++;
      if(mutation==='branch') h.active.currentBranch='changed';
      if(mutation==='head') h.active.headOid='c'.repeat(40);
      if(mutation==='path') h.active.path='/another';
      return mutation==='cancel' ? null : 'hard';
    };
    await h.open(); assert.equal(h.calls.length,1,mutation);
  }
});
test('stale preview never opens a dialog; execution errors are not retried',async()=>{
  const h=harness(); let opened=0;
  h.c.window.GDDBranchReset.choose=async()=>{opened++; return 'mixed';};
  h.c.invoke=async()=>{h.c.repoOpenGeneration++; return report();};
  await h.open(); assert.equal(opened,0);
  const h2=harness(); h2.c.invoke=async(cmd,args)=>{h2.calls.push([cmd,args]);if(cmd==='preview_branch_reset')return report();throw Error('index.lock');};
  await h2.open(); assert.equal(h2.calls.length,2); assert.match(h2.errors[0],/index.lock/); assert.equal(h2.refreshes.length,1);
});
test('repeated clicks cannot open concurrent reset confirmations or submit twice',async()=>{
  const h=harness(); let resolve;
  h.c.window.GDDBranchReset.choose=()=>new Promise(done=>{resolve=done;});
  const first=h.open(); await Promise.resolve(); await Promise.resolve();
  await h.open(); assert.equal(h.calls.length,1);
  resolve('soft');await first;
  assert.equal(h.calls.length,2);assert.equal(h.c.branchResetActive,false);
});
test('display refresh failure preserves successful reset outcome and never repeats it',async()=>{
  const h=harness(); h.c.refreshAfterGraphCommitAction=async()=>{throw Error('refresh failed');};
  await h.open(); assert.equal(h.calls.length,2); assert.equal(h.errors.length,0);
  assert.match(h.messages.at(-1)[0],/topic reset to.*Display refresh failed/);
});
test('read-only preview failure does not trigger a full repository reload',async()=>{
  const h=harness();h.c.invoke=async()=>{throw Error('Target commit unavailable');};
  await h.open();assert.equal(h.refreshes.length,0);assert.match(h.errors[0],/Target commit unavailable/);
  assert.equal(h.c.branchResetActive,false);
});
test('dialog explains staging, hard deletion, submodules, upstream freshness and escapes user text',()=>{
  assert.match(api.modes.soft.detail,/index.*files stay unchanged/);
  assert.match(api.modes.mixed.detail,/remain unstaged/);
  assert.match(api.modes.hard.detail,/Untracked or ignored files blocking checkout/);
  assert.match(api.modes.hard.detail,/not a full Clean/);
  const r=report(); r.subject='<img src=x onerror=bad()>'; r.branch='topic<script>';
  const html=api.html(r,'module<repo>',true);
  assert.ok(!html.includes('<script>')); assert.ok(!html.includes('<img'));
  assert.match(html,/Soft/); assert.match(html,/Mixed/); assert.match(html,/Hard/);
  assert.match(html,/value="soft" checked/); assert.match(html,/data-reset-apply class="danger" disabled/);
  assert.match(html,/no Fetch, Push or recursive Clean/); assert.match(html,/parent project’s recorded submodule revision is not changed/);
});
test('ancestry and Push warnings distinguish non-fast-forward, missing history and no upstream',()=>{
  assert.match(api.warnings(report()).join(' '),/normal Push.*rejected/);
  assert.match(api.warnings({...report(),relation:'divergent'}).join(' '),/different line of history/);
  assert.match(api.warnings({...report(),relation:'unknown',push_status:'unknown'}).join(' '),/could not be established/);
  assert.match(api.warnings({...report(),push_status:'fast_forward'}).join(' '),/not a guarantee/);
  assert.match(api.warnings({...report(),upstream:null,push_status:'not_configured'}).join(' '),/No upstream is configured/);
});
test('dialog consent defaults off and resets when choosing another type; Escape cancels',async()=>{
  const nodes=new Map(); let mode='soft';
  function node(key){if(!nodes.has(key))nodes.set(key,{checked:false,disabled:false,value:'',textContent:'',handlers:{},classList:{toggle(){}},addEventListener(k,fn){this.handlers[k]=fn;}});return nodes.get(key);}
  const close=node('cancel'),radios=['soft','mixed','hard'].map(node);
  const dialog={innerHTML:'',open:false,handlers:{},
    querySelector:s=>s.endsWith(':checked')?{value:mode}:node(s),
    querySelectorAll:s=>s==='[data-reset-cancel]'?[close]:radios,
    addEventListener(k,fn){this.handlers[k]=fn;},removeEventListener(k){delete this.handlers[k];},
    showModal(){this.open=true;},close(){this.open=false;this.handlers.close?.();}};
  const pending=api.choose(dialog,report(),'repo');
  assert.equal(node('[data-reset-apply]').disabled,true);
  node('[data-reset-consent]').checked=true;node('[data-reset-consent]').handlers.change();
  assert.equal(node('[data-reset-apply]').disabled,false);
  mode='hard';radios[2].handlers.change();
  assert.equal(node('[data-reset-consent]').checked,false);assert.equal(node('[data-reset-apply]').disabled,true);
  assert.match(node('[data-reset-consent-text]').textContent,/permanently discarded/);
  dialog.handlers.cancel({preventDefault(){}}); assert.equal(await pending,null);
});
test('reset uses the existing mutation guard and is registered separately from Clean',()=>{
  assert.match(app.split(']);')[0],/'reset_current_branch'/);
  const rust=fs.readFileSync(require.resolve('../src-tauri/src/lib.rs'),'utf8');
  assert.match(rust,/branches::reset::reset_current_branch/); assert.match(rust,/branches::reset::preview_branch_reset/);
});
