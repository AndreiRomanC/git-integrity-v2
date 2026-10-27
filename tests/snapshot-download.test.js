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

test('binary/large previews download immutable blob pairs, never empty placeholder text', () => {
  const c = load(['fileComparisonDownloadSources']);
  const sources=[{kind:'blob',repository_path:'/repo',oid:'abc',name:'Dcm.xdm'},{kind:'missing'}];
  const pair=c.fileComparisonDownloadSources({preview_note:'too large',local_content:'',remote_content:'',download_sources:sources},'Dcm.xdm');
  assert.deepEqual(pair,sources);
});

test('unavailable preview clears Loading and old diff navigation but enables complete download', () => {
  const button={disabled:true};
  const c=load(['resetCompareDiffNavigation','captureFileComparisonDownload','fileComparisonDownloadSources','showComparisonPreviewUnavailable','renderComparisonContents'], {
    state:{compareDiffRows:[1],compareDiffBlocks:[{start:1,end:1}]}, $:()=>button,
    refs:{localCompare:{},remoteCompare:{},compareDiffStatus:{},previousCompareDifference:{},nextCompareDifference:{},compareDialog:{querySelectorAll:()=>[]}},
  });
  c.renderComparisonContents('','',{relative_path:'Dcm.xdm',preview_note:'Left: File exceeds 10 MiB.',local_exists:true,remote_exists:false,local_size:11000000,download_sources:[{kind:'blob',oid:'abc'},{kind:'missing'}]});
  assert.equal(button.disabled,false);
  assert.match(c.refs.compareDiffStatus.textContent,/Text preview unavailable/);
  assert.match(c.refs.remoteCompare.textContent,/Absent/);
  assert.equal(c.state.compareDiffBlocks.length,0);
  assert.equal(c.state.compareDownloadSnapshot.sources[0].oid,'abc');
});

test('large XML above old 1 MB limit retains exact text and uses bounded existing diff engine', () => {
  const diffEngine=require('../frontend/local-diff.js');
  let rendered=0;
  const c=load(['comparisonBlocks','renderComparisonContents'],{state:{},window:{LocalDriveDiff:diffEngine},captureFileComparisonDownload(){},renderComparisonRows(){rendered++;}});
  const left=Array.from({length:4000},(_,i)=>`<item id="${i}">${'a'.repeat(500)}</item>\n`).join('');
  const right=left.replace('aaaa','bbbb');
  c.renderComparisonContents(left,right,{relative_path:'Dcm.xdm'});
  assert.equal(rendered,1); assert.equal(c.state.compareRendered.localText,left);
  assert.equal(c.state.compareDiffBlocks.length,1);
});

test('too many aligned rows yield download-only instead of creating an excessive DOM', () => {
  let unavailable=0,rendered=0;
  const c=load(['renderComparisonContents'],{state:{},window:{LocalDriveDiff:{buildLineDiff:()=>({rows:Array(12001).fill({same:true})})}},
    captureFileComparisonDownload(){},renderComparisonRows(){rendered++;},showComparisonPreviewUnavailable(note,snapshot){assert.match(note,/12,000/);assert.equal(snapshot.relative_path,'many.txt');unavailable++;}});
  c.renderComparisonContents('a','b',{relative_path:'many.txt'});
  assert.equal(unavailable,1);assert.equal(rendered,0);
});
