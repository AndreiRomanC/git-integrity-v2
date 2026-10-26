// Explicit, snapshot-only merge report. No mutation command or automatic read.
(function (global) {
  const PAGE_SIZE = 200;
  const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const labels = {current:'Keep current', incoming:'From incoming', both:'Both match', combined:'Combined automatically', conflict:'Decision needed'};
  const statuses = {up_to_date:'Already up to date', fast_forward:'Fast-forward available', clean:'Automatic merge calculated', conflicts:'Conflicts need resolution'};
  const same = (a,b) => !a && !b || !!a && !!b && a.oid === b.oid && a.mode === b.mode;
  function reason(item, report) {
    if (item.outcome === 'conflict') return `No final version chosen · ${item.conflict || 'unresolved conflict'}.`;
    if (item.outcome === 'combined') return 'The engine combined both versions automatically. Review the result; no textual conflict does not guarantee functional correctness.';
    if (item.outcome === 'both') return 'Both sides have the same result.';
    const hasBase = report.merge_bases?.length === 1;
    if (item.outcome === 'incoming') return (hasBase && same(item.base,item.current) ? 'Current branch did not change this path since the common base. ' : '') +
      (item.result ? 'The incoming version is used.' : 'The incoming side removes this path.');
    return (hasBase && same(item.base,item.incoming) ? 'Incoming branch did not change this path since the common base. ' : '') +
      (item.result ? 'The current version is kept; no change to this path.' : 'The current branch’s removal is kept.');
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
  function versionHtml(value, title, path, unresolved=false, unknownBase=false) {
    return `<div class="merge-preview-version"><span>${escape(title)}</span><code title="${escape(value?.oid || '')}">${unresolved ? 'Unresolved' : unknownBase ? 'Multiple bases' : value ? escape(value.oid.slice(0,10)) : 'Absent'}</code>${value ? `<small>${value.mode===0o160000 ? 'Submodule commit' : 'File object'}${value.path!==path ? ' · '+escape(value.path) : ''}</small>` : ''}</div>`;
  }
  function rowHtml(item,index,report) {
    const sub = item.kind==='submodule', conflict = item.outcome==='conflict';
    const compare = (pair,label) => `<button type="button" data-preview-file="${index}" data-pair="${pair}">${label}</button>`;
    return `<details class="merge-preview-item${conflict ? ' is-conflict' : ''}${item.warnings?.length ? ' has-warning' : ''}"><summary><span class="merge-preview-kind" title="${sub ? 'Submodule pointer' : 'File'}">${sub ? '◇' : '≡'}</span><span class="merge-preview-path" title="${escape(item.path)}">${escape(item.path)}</span><span class="merge-preview-change">${escape(item.change)}</span><span class="merge-preview-outcome outcome-${escape(item.outcome)}">${labels[item.outcome] || 'Review'}${item.warnings?.length ? ' · review' : ''}</span></summary>
      <div class="merge-preview-item-body"><p>${escape(reason(item,report))}</p>
      <div class="merge-preview-versions">${versionHtml(item.base,'Common base',item.path,false,report.merge_bases?.length>1)}${versionHtml(item.current,'Current · '+report.current_branch,item.path)}${versionHtml(item.incoming,'Incoming · '+report.source_ref,item.path)}${versionHtml(item.result,'Proposed result',item.path,conflict)}</div>
      ${(item.warnings||[]).map(w=>`<p class="merge-preview-warning">${escape(w)}</p>`).join('')}
      ${sub ? '<p class="merge-preview-note">This is the parent’s recorded pointer, not a merge of files inside the submodule. Updating its local folder is a separate update/init operation.</p>' : ''}
      <div class="merge-preview-item-actions">${!sub ? (!conflict ? compare('result','View result diff') : compare('sides','Compare conflicting sides')) + (item.base ? compare('ours','Base → current') + compare('theirs','Base → incoming') : '') : `<button type="button" data-preview-submodule="${index}">Inspect current / incoming versions</button>`}</div><div data-preview-extra="${index}"></div></div></details>`;
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
        el('summary').textContent=`${statuses[data.status] || data.status} · ${c.changes} changed${c.combined ? ` (${c.combined} combined)` : ''} · ${c.kept} kept · ${c.conflicts} unresolved`;
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
  const api={create,counts,filtered,rowHtml,reason};
  if(typeof module!=='undefined'&&module.exports)module.exports=api;
  global.MergePreview=api;
})(typeof window!=='undefined'?window:globalThis);
