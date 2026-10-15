const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
function load(names, context = {}) {
  vm.createContext(context);
  for (const name of names) {
    const source = app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))?.[0];
    assert.ok(source, name); vm.runInContext(source, context);
  }
  return context;
}
const plain = value => JSON.parse(JSON.stringify(value));

test('download captures sources before picker, blocks double click, never runs a Git mutation', async () => {
  let choose; const calls = [], errors = [];
  const c = load(['downloadSnapshots'], { status() {}, showOperationToast() {}, handleError: e => errors.push(e),
    invoke: (command, args) => { calls.push({ command, args }); return command === 'choose_folder' ? new Promise(r => { choose = r; }) : Promise.resolve('/out/snapshot'); },
  });
  const sources = [{kind:'text',name:'a.txt',content:'old\r\n'}];
  const button = { dataset: {}, disabled: false };
  const pending = c.downloadSnapshots('a', sources, button);
  assert.equal(button.disabled, true);
  await c.downloadSnapshots('a', sources, button);
  assert.equal(calls.length, 1);
  sources[0].content = 'new selection'; choose('/out');
  assert.equal(await pending, '/out/snapshot');
  assert.deepEqual(calls.map(call => call.command), ['choose_folder','export_snapshots']);
  assert.equal(calls[1].args.sources[0].content, 'old\r\n');
  assert.equal(button.disabled, false); assert.equal(errors.length, 0);
});

test('cancel and export failure release the button without a refresh or restore', async () => {
  for (const cancel of [true,false]) {
    const errors = [], calls = [];
    const c = load(['downloadSnapshots'], {status(){},showOperationToast(){},handleError:e=>errors.push(e),
      invoke: async command => { calls.push(command); if (command === 'choose_folder') return cancel ? null : '/out'; throw Error('disk full'); },
    });
    const button = {dataset:{},disabled:false}; await c.downloadSnapshots('a',[{kind:'missing'}],button);
    assert.equal(button.disabled,false); assert.equal(errors.length,cancel ? 0 : 1);
    assert.deepEqual(calls,cancel ? ['choose_folder'] : ['choose_folder','export_snapshots']);
  }
});

test('file download uses loaded raw text, never diff HTML, and distinguishes empty from absent', () => {
  const c = load(['fileComparisonDownloadSources']);
  const pair = c.fileComparisonDownloadSources({local_content:'<tag>\r\n',remote_content:'',local_exists:true,remote_exists:false},'a.c');
  assert.deepEqual(plain(pair),[{kind:'text',name:'a.c',content:'<tag>\r\n'},{kind:'missing'}]);
  assert.equal(c.fileComparisonDownloadSources({local_content:'',local_exists:true},'empty')[0].kind,'text');
});

test('folder export uses resolved SHA and owner repository, not editable branch inputs', () => {
  const c = load(['commanderDownloadSnapshot']);
  const refs = c.commanderDownloadSnapshot({relative_path:'src',left_revision:'abc',right_revision:'def'},'/repo',true);
  assert.deepEqual(plain(refs.sources),[
    {kind:'revision',repository_path:'/repo',revision:'abc',relative_path:'src'},
    {kind:'revision',repository_path:'/repo',revision:'def',relative_path:'src'},
  ]);
  const worktree = c.commanderDownloadSnapshot({repository_path:'D:/repo/module',relative_path:'inside',remote_revision:'xyz'},'D:/repo',false);
  assert.equal(worktree.sources[0].path,'D:/repo/module/inside');
  assert.equal(worktree.sources[1].repository_path,'D:/repo/module');
  assert.equal(c.commanderDownloadSnapshot({rows:[]},'/repo',true),null);
});

test('reset disables stale download until a successful file comparison arrives', () => {
  const button = {disabled:false};
  const c = load(['resetCompareDiffNavigation','captureFileComparisonDownload','fileComparisonDownloadSources'], {
    state:{compareDownloadSnapshot:{name:'old'}}, $:()=>button,
    refs:{compareDiffStatus:{},previousCompareDifference:{},nextCompareDifference:{},compareDialog:{querySelectorAll:()=>[]}},
  });
  c.resetCompareDiffNavigation(); assert.equal(button.disabled,true); assert.equal(c.state.compareDownloadSnapshot,null);
  c.captureFileComparisonDownload({relative_path:'folder/a.txt',local_content:'before',remote_content:'after'});
  assert.equal(button.disabled,false); assert.equal(c.state.compareDownloadSnapshot.name,'a.txt');
});

test('current submodule revisions also expose download without requiring checkout', () => {
  const {submoduleVersionRowHtml} = require('../frontend/submodule-versions.js');
  for (const kind of ['branch','tag','commit']) {
    const html = submoduleVersionRowHtml({kind,name:'example',revision:'a'.repeat(40),current:true});
    assert.match(html,/data-download-version/); assert.match(html,/no checkout/);
  }
});
