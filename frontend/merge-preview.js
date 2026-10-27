// Explicit, snapshot-only merge report. No mutation command or automatic read.
(function (global) {
  const PAGE_SIZE = 200;
  const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const labels = {current:'Keep current', incoming:'From incoming', both:'Both match', combined:'Combined automatically', conflict:'Decision needed'};
  const statuses = {up_to_date:'Already up to date', fast_forward:'Fast-forward available', clean:'Automatic merge calculated', conflicts:'Conflicts need resolution'};
  const same = (a,b) => !a && !b || !!a && !!b && a.oid === b.oid && a.mode === b.mode;
  // One definition for both the buttons and the actual viewer. A result diff
  // measures the merge's effect on CURRENT, not the difference between branches.
  function comparison(item, report, pair) {
    const sides={result:['current','result'],sides:['current','incoming'],ours:['base','current'],theirs:['base','incoming']}[pair];
    if(!sides || sides.some(side=>item.directory_sides?.includes(side))) return null;
    const current = `Current · ${report.current_branch} @ ${report.current_revision.slice(0,10)}`;
    const incoming = `Incoming · ${report.source_ref} @ ${report.incoming_revision.slice(0,10)}`;
    const base = `Common base @ ${report.merge_bases?.[0]?.slice(0,10) || '?'}`;
    const choices = {
      result: {left:item.current,right:item.result,leftLabel:current,rightLabel:'Proposed merge result',label:'Current → result',description:'Changes this merge would make to your current committed version.'},
      sides: {left:item.current,right:item.incoming,leftLabel:current,rightLabel:incoming,label:'Current ↔ incoming',description:'Branch snapshots only — this is not the proposed merge result.'},
      ours: {left:item.base,right:item.current,leftLabel:base,rightLabel:current,label:'Base → current',description:'Changes on the current branch since the common base.'},
      theirs: {left:item.base,right:item.incoming,leftLabel:base,rightLabel:incoming,label:'Base → incoming',description:'Changes on the incoming branch since the common base.'},
    };
    if (pair==='result' && item.outcome==='conflict' || ['ours','theirs'].includes(pair) && report.merge_bases?.length!==1) return null;
    const selected = choices[pair];
    return selected ? {...selected, identical:same(selected.left,selected.right)} : null;
  }
  function changeLabel(item,report) {
    if(item.change!=='unchanged' && item.outcome!=='conflict' && item.directory_sides?.includes('result')) return 'Replaced by folder';
    if(item.change!=='unchanged' && item.outcome!=='conflict' && item.directory_sides?.includes('current')) return item.kind==='submodule' ? 'Folder → submodule' : 'Folder → file';
    if(item.change!=='unchanged') return {added:'Added by merge',removed:'Removed by merge',modified:'Changed by merge',unresolved:'Unresolved'}[item.change] || item.change;
    if(report.merge_bases?.length===1) {
      if(!item.base && item.current && !item.incoming && !item.directory_sides?.length) return 'Current-only addition';
      if(item.base && !item.current && same(item.base,item.incoming) && !item.directory_sides?.length) return 'Removed on current';
    }
    return 'No change to current';
  }
  function reason(item, report) {
    if (item.outcome === 'conflict') return `No final version chosen · ${item.conflict || 'unresolved conflict'}.`;
    if(item.directory_sides?.length) {
      const result = item.directory_sides.includes('result') ? 'The result is a folder.' : item.result ? `The result ${item.outcome==='current' ? 'keeps the current' : 'uses a'} ${item.kind==='submodule' ? 'submodule pointer' : 'file'}.` : 'The result removes this path.';
      return `Folder/${item.kind==='submodule' ? 'submodule' : 'file'} conversion. ${result} Folder contents are listed as separate paths.`;
    }
    if (item.outcome === 'combined') return 'The engine calculated a new result for this path (combined changes or rename handling). Review it; no textual conflict does not guarantee functional correctness.';
    if (item.outcome === 'both') return 'Both sides have the same result.';
    const hasBase = report.merge_bases?.length === 1;
    if(hasBase && !item.base && item.outcome==='current' && item.current && !item.incoming)
      return 'Added only on the current branch; absent from the common base and incoming branch. The merge keeps it — absence on incoming is not a deletion from the base.';
    if(hasBase && !item.base && item.outcome==='incoming' && !item.current && item.incoming)
      return 'Added only on the incoming branch. The merge adds it to the current branch.';
    if(!item.result && hasBase && !same(item.base,item.current) && !same(item.base,item.incoming))
      return 'Absent at this path in the proposed result. A rename may be recorded under another path; compare the branch snapshots before interpreting this as a deletion.';
    if (item.outcome === 'incoming') return (hasBase && same(item.base,item.current) ? 'Current branch did not change this path since the common base. ' : '') +
      (item.result ? 'The incoming version is used.' : 'The incoming side removes this path.');
    return (hasBase && same(item.base,item.incoming) ? 'Incoming branch did not change this path since the common base. ' : '') +
      (item.result ? 'The current version is kept; the merge makes no further change to it.' : 'The current branch’s removal is kept.');
  }
  function counts(items) {
    return {changes:items.filter(i=>i.change!=='unchanged' && i.outcome!=='conflict').length,
      kept:items.filter(i=>i.change==='unchanged').length, combined:items.filter(i=>i.outcome==='combined').length,
      conflicts:items.filter(i=>i.outcome==='conflict').length, submodules:items.filter(i=>i.kind==='submodule').length,
      review:items.filter(i=>i.outcome==='conflict' || i.warnings?.length).length};
  }
  function filtered(items, query, filter) {
    const text = query.trim().toLowerCase();
    return items.filter(item => (!text || [item.path,item.base?.path,item.current?.path,item.incoming?.path].filter(Boolean).join(' ').toLowerCase().includes(text)) &&
      (filter==='all' || filter==='changes' && item.change!=='unchanged' && item.outcome!=='conflict' || filter==='conflicts' && item.outcome==='conflict' ||
        filter==='submodules' && item.kind==='submodule' || filter==='review' && (item.outcome==='conflict' || item.warnings?.length)));
  }
  function versionHtml(value, title, path, unresolved=false, unknownBase=false, directory=false) {
    return `<div class="merge-preview-version"><span>${escape(title)}</span><code title="${escape(value?.oid || '')}">${unresolved ? 'Unresolved' : unknownBase ? 'Multiple bases' : directory ? 'Folder' : value ? escape(value.oid.slice(0,10)) : 'Absent'}</code>${directory ? '<small>See child paths</small>' : value ? `<small>${value.mode===0o160000 ? 'Submodule commit' : value.mode===0o120000 ? 'Symbolic link' : value.mode===0o100755 ? 'Executable file object' : 'File object'}${value.path!==path ? ' · '+escape(value.path) : ''}</small>` : ''}</div>`;
  }
  function rowHtml(item,index,report) {
    const sub = item.kind==='submodule', conflict = item.outcome==='conflict';
    const folder = side => item.directory_sides?.includes(side);
    const compare = pair => {
      const selected=comparison(item,report,pair);
      if(!selected || selected.identical && pair!=='result') return '';
      return `<button type="button" data-preview-file="${index}" data-pair="${pair}" title="${escape(selected.identical ? 'The proposed result is identical to the current version. Compare branches or the common base instead.' : selected.description)}"${selected.identical ? ' disabled' : ''}>${selected.identical ? 'Current = result' : selected.label}</button>`;
    };
    return `<details class="merge-preview-item${conflict ? ' is-conflict' : ''}${item.warnings?.length ? ' has-warning' : ''}"><summary><span class="merge-preview-kind" title="${sub ? 'Submodule pointer' : 'File'}">${sub ? '◇' : '≡'}</span><span class="merge-preview-path" title="${escape(item.path)}">${escape(item.path)}</span><span class="merge-preview-change" title="Effect of the proposed merge on the current committed version">${escape(changeLabel(item,report))}</span><span class="merge-preview-outcome outcome-${escape(item.outcome)}">${labels[item.outcome] || 'Review'}${item.warnings?.length ? ' · review' : ''}</span></summary>
      <div class="merge-preview-item-body"><p>${escape(reason(item,report))}</p>
      <div class="merge-preview-versions">${versionHtml(item.base,'Common base',item.path,false,report.merge_bases?.length>1,folder('base'))}${versionHtml(item.current,'Current · '+report.current_branch,item.path,false,false,folder('current'))}${versionHtml(item.incoming,'Incoming · '+report.source_ref,item.path,false,false,folder('incoming'))}${versionHtml(item.result,'Proposed result',item.path,conflict,false,folder('result'))}</div>
      ${(item.warnings||[]).map(w=>`<p class="merge-preview-warning">${escape(w)}</p>`).join('')}
      ${sub ? '<p class="merge-preview-note">This is the parent’s recorded pointer, not a merge of files inside the submodule. Updating its local folder is a separate update/init operation.</p>' : ''}
      <div class="merge-preview-item-actions">${!sub ? compare('result') + compare('sides') + compare('ours') + compare('theirs') : `<button type="button" data-preview-submodule="${index}">Inspect current / incoming versions</button>`}</div><div data-preview-extra="${index}"></div></div></details>`;
  }
  function create({dialog, invoke, onFile, current}) {
    const el = name => dialog.querySelector(`[data-preview-${name}]`);
    let request=0, report=null, args=null, context=null, indices=new Map(), limit=PAGE_SIZE;
    const valid = token => token===request && dialog.open && current(context);
    const message = text => { el('list').innerHTML=`<p class="merge-preview-empty" role="status">${escape(text)}</p>`; };
    const renderRows = () => {
      if (!report) return;
      const rows=filtered(report.items,el('search').value,el('filter').value);
      el('list').innerHTML=rows.slice(0,limit).map(item=>rowHtml(item,indices.get(item),report)).join('') || '<p class="merge-preview-empty">No items match this view.</p>';
      if(rows.length>limit) el('list').innerHTML+='<button type="button" data-preview-more>Show 200 more</button>';
      el('shown').textContent=(rows.length>limit ? `${limit} shown · ` : '') + (rows.length===report.items.length ? `${rows.length} items` : `${rows.length} / ${report.items.length} items match`);
    };
    async function calculate() {
      const token=++request; report=null; indices.clear(); limit=PAGE_SIZE;
      el('refresh').disabled=true; el('search').disabled=true; el('filter').disabled=true;
      el('summary').textContent='Calculating committed snapshots…'; el('shown').textContent=''; el('warnings').replaceChildren();
      message('Reading local history and calculating the automatic merge. No checkout, fetch or submodule update.');
      try {
        const data=await invoke('preview_merge',args);
        if (token!==request || !dialog.open) return;
        if (!current(context)) { el('summary').textContent='Preview no longer applies'; message('Repository context changed. Close and open a new preview.'); return; }
        report=data;
        indices=new Map(data.items.map((item,index)=>[item,index]));
        const c=counts(data.items);
        el('direction').textContent=`${data.source_ref} → ${data.current_branch}`;
        el('direction').title=`${data.repository_path}\nIncoming: ${data.incoming_revision}\nCurrent: ${data.current_revision}`;
        el('snapshots').textContent=`${data.incoming_revision.slice(0,10)} → ${data.current_revision.slice(0,10)} · ${(data.elapsed_ms/1000).toFixed(1)}s`;
        el('summary').textContent=`${statuses[data.status] || data.status} · ${c.changes} changes to current${c.combined ? ` (${c.combined} combined)` : ''} · ${c.kept} kept · ${c.conflicts} unresolved`;
        el('warnings').innerHTML=(data.warnings||[]).map(w=>`<p class="merge-preview-warning">${escape(w)}</p>`).join('');
        el('search').disabled=false; el('filter').disabled=false;
        if (!data.items.length) message(data.status==='up_to_date' ? 'The incoming branch is already contained in the current branch. Nothing to merge.' : 'No path changes in the calculated result.');
        else renderRows();
      } catch(error) {
        if (token===request && dialog.open) { el('summary').textContent='Preview unavailable · no result applied'; message(String(error)); }
      } finally { if(token===request && dialog.open) el('refresh').disabled=false; }
    }
    dialog.addEventListener('close',()=>{if(!dialog.open){request++;report=null;indices.clear();}});
    dialog.querySelectorAll('[data-preview-close]').forEach(button=>button.addEventListener('click',()=>dialog.close()));
    el('refresh').addEventListener('click',()=>{if(!current(context)){message('Repository context changed. Open a new preview.');return;} args={...args,expectedIncoming:null}; return calculate();});
    const refilter=()=>{limit=PAGE_SIZE;renderRows();};
    el('search').addEventListener('input',refilter); el('filter').addEventListener('change',refilter);
    el('list').addEventListener('click',async event=>{
      if(event.target.closest('[data-preview-more]')) {limit+=PAGE_SIZE;renderRows();return;}
      const file=event.target.closest('[data-preview-file]'), sub=event.target.closest('[data-preview-submodule]');
      if(!file && !sub || !report) return;
      const token=request, snapshot=report;
      if(!valid(token)){message('Repository context changed. Open a new preview.');return;}
      const item=report.items[Number((file||sub).dataset[file?'previewFile':'previewSubmodule'])];
      if(!item) return;
      if(file) { return onFile(snapshot,item,file.dataset.pair,()=>valid(token)&&report===snapshot); }
      const output=el('list').querySelector(`[data-preview-extra="${sub.dataset.previewSubmodule}"]`);
      sub.disabled=true; output.textContent='Reading local submodule metadata…';
      try {
        if(item.current?.path && item.incoming?.path && item.current.path!==item.incoming.path) throw new Error('Renamed submodule conflict: inspect the separate paths shown above. No repository location was guessed.');
        const data=await invoke('submodule_comparison_details',{repositoryPath:snapshot.repository_path,relativePath:item.path,leftRef:snapshot.current_revision,rightRef:snapshot.incoming_revision,workspaceRevision:null});
        if(valid(token) && report===snapshot && output.isConnected) output.innerHTML=global.SubmoduleCompareDetails.detailsHtml(data,['Current pointer','Incoming pointer']);
      }catch(error){if(valid(token)&&output.isConnected)output.textContent=String(error);}
      finally{if(valid(token))sub.disabled=false;}
    });
    return {open(nextArgs,nextContext){args=nextArgs;context=nextContext;el('search').value='';el('filter').value='all';el('direction').textContent=`${args.sourceRef} → current branch`;el('snapshots').textContent='';if(!dialog.open)dialog.showModal();return calculate();}};
  }
  const api={create,counts,filtered,rowHtml,reason,comparison,changeLabel};
  if(typeof module!=='undefined'&&module.exports)module.exports=api;
  global.MergePreview=api;
})(typeof window!=='undefined'?window:globalThis);
