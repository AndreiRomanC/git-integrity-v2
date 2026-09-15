(() => {
  'use strict';

  const $ = selector => document.querySelector(selector);
  const refs = {
    shell: $('.app-shell'), meta: $('#analysisMeta'), search: $('#globalSearch'),
    overviewMode: $('#overviewMode'), deepMode: $('#deepMode'), learnNav: $('#learnNav'),
    deepNav: $('#deepNav'), scopeTree: $('#scopeTree'), back: $('#backToOverview'),
    breadcrumbs: $('#breadcrumbs'), header: $('#pageHeader'), toolbar: $('#toolbar'),
    content: $('#content'), inspector: $('#inspector'), dialog: $('#modelDialog'), file: $('#modelFile')
  };

  const views = {
    overview: { label: 'System overview', icon: '◇', mode: 'overview' },
    flows: { label: 'How actions run', icon: '→', mode: 'overview' },
    components: { label: 'Responsibilities', icon: '▦', mode: 'overview' },
    files: { label: 'Files', icon: '▤', mode: 'deep' },
    functions: { label: 'Functions', icon: 'ƒ', mode: 'deep' },
    connections: { label: 'Connections', icon: '↔', mode: 'deep' },
    findings: { label: 'Observations', icon: '△', mode: 'deep' }
  };

  const state = {
    model: null, entities: new Map(), files: new Map(), functions: new Map(), functionKeys: new Map(),
    children: new Map(), view: 'overview', scopeId: 'system', selection: null, query: '',
    fileFilter: 'ALL', functionLimit: 10, findingFilter: 'ALL', selectedFlowId: null
  };

  function escapeHtml(value = '') {
    return String(value).replace(/[&<>"']/g, char => ({ '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;' }[char]));
  }
  function plural(count, one, many = `${one}s`) { return `${count} ${count === 1 ? one : many}`; }
  function entity(id) { return state.entities.get(id); }
  function file(id) { return state.files.get(id); }
  function fn(id) { return state.functions.get(id); }
  function childrenOf(id) { return state.children.get(id) || []; }
  function layerIdFor(value) {
    if (!value) return 'system';
    if (value.layerId) return value.layerId;
    let current = value;
    while (current && current.type !== 'layer') current = entity(current.parentId);
    return current?.id || 'system';
  }
  function layerToken(value) {
    const id = typeof value === 'string' ? value : layerIdFor(value);
    if (id === 'presentation') return 'gui';
    if (id === 'tauri-boundary') return 'bridge';
    if (id === 'backend') return 'backend';
    return 'external';
  }
  function layerColor(value) { return `var(--${layerToken(value)})`; }
  function isWithin(candidateId, scopeId) {
    if (scopeId === 'system' || candidateId === scopeId) return true;
    let current = entity(candidateId);
    while (current?.parentId) {
      if (current.parentId === scopeId) return true;
      current = entity(current.parentId);
    }
    return false;
  }
  function fileIsWithin(item, scopeId) {
    return isWithin(item.componentId, scopeId) || (item.componentIds || []).some(componentId => isWithin(componentId, scopeId));
  }
  function sourcesOf(item) {
    if (!item) return [];
    if (Array.isArray(item.source)) return item.source;
    return item.source ? [item.source] : [];
  }
  function sourceHref(source) { return `../../${source.path}${source.startLine ? `#L${source.startLine}` : ''}`; }
  function sourceLabel(source) {
    const lines = source.startLine ? `:${source.startLine}${source.endLine ? `–${source.endLine}` : ''}` : '';
    return `${source.path}${lines}${source.symbol ? ` · ${source.symbol}` : ''}`;
  }
  function sourceHtml(item) {
    const sources = sourcesOf(item);
    return sources.length ? `<div class="source-list">${sources.map(source => `<a class="source-link" href="${escapeHtml(sourceHref(source))}" target="_blank" rel="noreferrer"><span>${escapeHtml(sourceLabel(source))}</span><small>open ↗</small></a>`).join('')}</div>` : '<p>No direct source location recorded.</p>';
  }
  function listHtml(items = []) {
    return items.length ? `<ul class="plain-list">${items.map(item => `<li>${escapeHtml(item)}</li>`).join('')}</ul>` : '<p>None recorded.</p>';
  }
  function section(title, body) { return `<section class="inspect-section"><h3>${escapeHtml(title)}</h3>${body}</section>`; }
  function sectionHeading(title, explanation) { return `<div class="section-heading"><h2>${escapeHtml(title)}</h2><p>${escapeHtml(explanation)}</p></div>`; }

  async function loadModel() {
    if (location.protocol === 'file:' && window.__ARCHITECTURE_MODEL__) return initialize(window.__ARCHITECTURE_MODEL__);
    try {
      const response = await fetch('architecture.json', { cache: 'no-store' });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      initialize(await response.json());
    } catch (error) {
      if (location.protocol === 'file:') refs.dialog.showModal();
      else refs.content.innerHTML = `<div class="empty-state">Could not load architecture.json: ${escapeHtml(error)}</div>`;
    }
  }

  function initialize(model) {
    state.model = model;
    state.entities = new Map(model.entities.map(item => [item.id, item]));
    state.files = new Map((model.files || []).map(item => [item.id, item]));
    state.functions = new Map((model.functions || []).map(item => [item.id, item]));
    state.functionKeys = new Map((model.functions || []).map(item => [`${item.source.path}:${item.source.startLine}:${item.name}`, item]));
    state.children = new Map();
    model.entities.forEach(item => {
      if (!state.children.has(item.parentId)) state.children.set(item.parentId, []);
      state.children.get(item.parentId).push(item);
    });
    refs.meta.textContent = `${model.metadata.analyzedCommit.slice(0, 8)} · ${plural(model.files?.length || 0, 'file')} · ${plural(model.metadata.productionFunctions || 0, 'function')}`;
    routeFromHash();
    if (refs.dialog.open) refs.dialog.close();
  }

  refs.file.addEventListener('change', async event => {
    const selected = event.target.files[0];
    if (!selected) return;
    try { initialize(JSON.parse(await selected.text())); }
    catch (error) { refs.content.innerHTML = `<div class="empty-state">Invalid model: ${escapeHtml(error)}</div>`; }
  });

  function routeFromHash() {
    if (!state.model) return;
    const [requestedView, requestedScope] = location.hash.slice(1).split('/');
    state.view = views[requestedView] ? requestedView : 'overview';
    state.scopeId = entity(requestedScope) ? requestedScope : 'system';
    state.selection = null;
    state.query = '';
    refs.search.value = '';
    render();
  }

  function navigate(view, scopeId = 'system') {
    state.view = views[view] ? view : 'overview';
    state.scopeId = entity(scopeId) ? scopeId : 'system';
    state.selection = null;
    state.query = '';
    refs.search.value = '';
    history.pushState(null, '', `#${state.view}/${state.scopeId}`);
    render();
    refs.content.closest('.workspace').scrollTop = 0;
  }

  function inspect(kind, id) {
    state.selection = { kind, id };
    renderBreadcrumbs();
    renderInspector();
    refs.shell.classList.remove('root-view');
  }

  function openEntity(id) {
    const item = entity(id);
    if (!item) return;
    const children = childrenOf(id);
    if (children.length) navigate('components', id);
    else if ((state.model.files || []).some(entry => fileIsWithin(entry, id))) navigate('files', id);
    else navigate('connections', id);
  }

  function render() {
    const rootView = state.view === 'overview' && state.scopeId === 'system' && !state.query && !state.selection;
    refs.shell.classList.toggle('root-view', rootView);
    renderNavigation();
    renderScopeTree();
    renderBreadcrumbs();
    renderHeader();
    renderToolbar();
    renderContent();
    renderInspector();
  }

  function navButton(id) {
    const item = views[id];
    return `<button class="nav-button ${state.view === id ? 'active' : ''}" type="button" data-view="${id}"><span>${item.icon}</span><span>${escapeHtml(item.label)}</span></button>`;
  }

  function renderNavigation() {
    refs.learnNav.innerHTML = ['overview', 'flows', 'components'].map(navButton).join('');
    refs.deepNav.innerHTML = ['files', 'functions', 'connections', 'findings'].map(navButton).join('');
    const mode = views[state.view].mode;
    refs.overviewMode.classList.toggle('active', mode === 'overview');
    refs.deepMode.classList.toggle('active', mode === 'deep');
  }

  function scopeRow(item, depth = 0) {
    return `<button class="scope-row ${state.scopeId === item.id ? 'active' : ''}" style="--depth:${depth};--layer-color:${layerColor(item)}" type="button" data-scope="${item.id}"><span>${escapeHtml(item.name)}</span></button>`;
  }

  function renderScopeTree() {
    const primaryIds = new Set(['presentation', 'tauri-boundary', 'backend']);
    const system = entity('system');
    refs.scopeTree.innerHTML = scopeRow(system, 0) + childrenOf('system').filter(item => primaryIds.has(item.id)).map(item => scopeRow(item, 1)).join('');
  }

  function lineage(id) {
    const result = [];
    let current = entity(id);
    while (current) { result.unshift(current); current = entity(current.parentId); }
    return result;
  }

  function renderBreadcrumbs() {
    const crumbs = lineage(state.scopeId).map(item => ({ label: item.name, kind: 'entity', id: item.id }));
    if (state.selection?.kind === 'file') crumbs.push({ label: file(state.selection.id)?.name, kind: 'file', id: state.selection.id });
    if (state.selection?.kind === 'function') {
      const selected = fn(state.selection.id);
      const selectedFile = file(selected?.fileId);
      if (selectedFile && !crumbs.some(item => item.id === selectedFile.id)) crumbs.push({ label: selectedFile.name, kind: 'file', id: selectedFile.id });
      crumbs.push({ label: `${selected?.name || 'function'}()`, kind: 'function', id: state.selection.id });
    }
    refs.breadcrumbs.innerHTML = crumbs.map((item, index) => `<button class="crumb" type="button" data-crumb-kind="${item.kind}" data-crumb-id="${item.id}">${escapeHtml(item.label)}</button>${index < crumbs.length - 1 ? '<span class="crumb-sep">›</span>' : ''}`).join('');
  }

  function scopedFiles() { return [...state.files.values()].filter(item => fileIsWithin(item, state.scopeId)); }
  function scopedFunctions() { return [...state.functions.values()].filter(item => !item.test && item.importanceRank && isWithin(item.componentId, state.scopeId)); }
  function scopedFlows() {
    if (state.scopeId === 'system') return state.model.executionFlows;
    return state.model.executionFlows.filter(flow => flow.steps.some(step => isWithin(step.entityId, state.scopeId)));
  }

  function renderHeader() {
    const scope = entity(state.scopeId);
    const layer = layerToken(scope);
    const headers = {
      overview: ['ARCHITECTURE OVERVIEW', 'Understand GitDrillDown before reading code', 'Three responsibilities, one request path, and details only when you ask for them.'],
      flows: ['USER AND RUNTIME FLOWS', 'What happens when someone performs an action?', 'Follow one real action from the interface through the native backend and back to the screen.'],
      components: ['LOGICAL RESPONSIBILITIES', `What exists inside ${scope.name}?`, 'These groups reflect what the software does, not merely where files happen to live.'],
      files: ['SOURCE ORGANIZATION', `Which files implement ${scope.name}?`, 'Files are ordered by architectural role; open one to understand why it exists and what it connects.'],
      functions: ['IMPLEMENTATION LANDMARKS', `Which functions should I study inside ${scope.name}?`, 'A deterministic heuristic ranks architectural entry points and orchestrators; every rank explains itself.'],
      connections: ['FOCUSED CONNECTIONS', `What communicates with ${scope.name}?`, 'Only direct incoming and outgoing relationships are shown so the diagram remains readable.'],
      findings: ['CURRENT STATE AND RECOMMENDATIONS', 'What should we preserve or improve?', 'Evidence-backed observations describe the current implementation separately from suggested direction.']
    };
    const [kicker, title, description] = state.query ? ['SEARCH', `Results for “${state.query}”`, 'Results span architectural concepts, source files, functions and flows.'] : headers[state.view];
    const stats = state.view === 'overview' ? [] : [
      [scopedFiles().length, 'files'], [scopedFunctions().length, 'functions'], [scopedFlows().length, 'flows']
    ];
    refs.header.style.setProperty('--layer-color', `var(--${layer})`);
    refs.header.innerHTML = `<div class="page-kicker"><i></i>${escapeHtml(kicker)}</div><h1>${escapeHtml(title)}</h1><p>${escapeHtml(description)}</p>${stats.length ? `<div class="page-stats">${stats.map(([value, label]) => `<span class="page-stat"><strong>${value}</strong><span>${label}</span></span>`).join('')}</div>` : ''}`;
  }

  function renderToolbar() {
    if (state.query || state.view === 'overview' || state.view === 'flows' || state.view === 'components' || state.view === 'connections') {
      refs.toolbar.innerHTML = state.view === 'connections' ? '<span class="toolbar-note">Select a related element to move the focus · no global spaghetti graph</span>' : '';
      return;
    }
    if (state.view === 'files') {
      refs.toolbar.innerHTML = ['ALL', 'CORE', 'SUPPORTING', 'UTILITY'].map(value => `<button class="filter-button ${state.fileFilter === value ? 'active' : ''}" type="button" data-file-filter="${value}">${value === 'ALL' ? 'All files' : value}</button>`).join('') + '<span class="toolbar-note">Importance is architectural, not a quality rating</span>';
      return;
    }
    if (state.view === 'functions') {
      refs.toolbar.innerHTML = [10, 25, 50, 0].map(value => `<button class="filter-button ${state.functionLimit === value ? 'active' : ''}" type="button" data-function-limit="${value}">${value ? `Top ${value}` : 'All'}</button>`).join('') + '<span class="toolbar-note">Ranking combines call graph, flows, entry points and boundary crossings</span>';
      return;
    }
    if (state.view === 'findings') {
      refs.toolbar.innerHTML = ['ALL', 'KEEP', 'WATCH', 'IMPROVE', 'HIGH RISK'].map(value => `<button class="filter-button ${state.findingFilter === value ? 'active' : ''}" type="button" data-finding-filter="${value}">${value === 'ALL' ? 'All' : value}</button>`).join('') + '<span class="toolbar-note">Recommendations are not presented as current architecture</span>';
    }
  }

  function renderContent() {
    if (state.query) return renderSearch();
    ({ overview: renderOverview, flows: renderFlows, components: renderComponents, files: renderFiles, functions: renderFunctions, connections: renderConnections, findings: renderFindings })[state.view]();
  }

  function renderOverview() {
    const layers = state.model.primaryLayers || [];
    const cards = layers.map((layer, index) => `<button class="layer-card" data-layer="${layer.color}" data-open-entity="${layer.entityId}" type="button"><span class="layer-number"><span>0${index + 1} · ${escapeHtml(layer.label)}</span><b></b></span><h3>${escapeHtml(layer.purpose.split('.')[0])}</h3><p>${escapeHtml(layer.why)}</p><span class="layer-io"><span><b>INPUT</b>${escapeHtml(layer.input)}</span><span><b>OUTPUT</b>${escapeHtml(layer.output)}</span></span><span class="layer-open">Explore this layer →</span></button>`).join('<div class="map-connector"><span>→</span><small>request</small></div>');
    const story = (state.model.architectureStory || []).map(paragraph => `<p>${escapeHtml(paragraph)}</p>`).join('');
    const firstFlow = state.model.executionFlows[0];
    const capabilities = state.model.onboarding.capabilities.map(item => {
      const target = entity(item.entityId);
      return `<button class="entity-card" style="--layer-color:${layerColor(target)}" type="button" data-open-entity="${item.entityId}"><span class="card-top"><span>${escapeHtml(item.title.toUpperCase())}</span><span>→</span></span><h3>${escapeHtml(target?.name || item.title)}</h3><p>${escapeHtml(item.description)}</p></button>`;
    }).join('');
    refs.content.innerHTML = `<section class="story"><span class="story-label">READ THIS FIRST · ABOUT 2 MINUTES</span><h2>Architecture in plain language</h2>${story}</section>
      ${sectionHeading('The whole application in three layers', 'Click a layer only when you want more detail.')}
      <div class="architecture-map">${cards}<div class="return-path"><span>rendered result</span><i></i><span>back to the GUI</span></div></div>
      <div class="destinations"><div class="destination"><span>G</span><div><strong>Git repositories</strong><small>libgit2 and system Git</small></div></div><div class="destination"><span>F</span><div><strong>Local filesystem</strong><small>projects, files and metadata</small></div></div><div class="destination"><span>↗</span><div><strong>Remote services</strong><small>Git servers, GitHub and tools</small></div></div></div>
      <div class="first-flow"><div><small>ONE REAL EXAMPLE</small><strong>${escapeHtml(firstFlow.name)}</strong></div><div class="mini-flow"><span>User click</span><i>→</i><span>GUI handler</span><i>→</i><span>Tauri</span><i>→</i><span>Rust/Git</span><i>→</i><span>screen</span></div><button class="text-link" type="button" data-open-flow="${firstFlow.id}">Follow this flow →</button></div>
      <div style="height:32px"></div>${sectionHeading('Continue by responsibility', 'Choose the job you want to understand; implementation details remain one level deeper.')}<div class="card-grid">${capabilities}</div>`;
  }

  function componentCard(item) {
    const relatedFiles = [...state.files.values()].filter(entry => fileIsWithin(entry, item.id));
    const relatedFunctions = [...state.functions.values()].filter(entry => !entry.test && isWithin(entry.componentId, item.id));
    return `<button class="entity-card" style="--layer-color:${layerColor(item)}" type="button" data-open-entity="${item.id}"><span class="card-top"><span>${escapeHtml(item.type.toUpperCase())}</span><span>OPEN →</span></span><h3>${escapeHtml(item.name)}</h3><p>${escapeHtml(item.summary || '')}</p><p class="why-line"><b>Why:</b> ${escapeHtml((item.responsibilities || [])[0] || 'Provides a distinct architectural responsibility.')}</p><span class="card-meta"><span>${plural(childrenOf(item.id).length, 'child', 'children')}</span><span>${plural(relatedFiles.length, 'file')}</span><span>${plural(relatedFunctions.length, 'function')}</span></span></button>`;
  }

  function renderComponents() {
    const scope = entity(state.scopeId);
    let items = childrenOf(scope.id);
    if (scope.id === 'system') items = items.filter(item => ['presentation', 'tauri-boundary', 'backend'].includes(item.id));
    const wrapper = items.length === 1 && childrenOf(items[0].id).length ? items[0] : null;
    if (wrapper) items = childrenOf(wrapper.id);
    if (!items.length) {
      refs.content.innerHTML = `<div class="empty-state">${escapeHtml(scope.name)} is already a focused responsibility. Continue to its files or connections.</div>`;
      return;
    }
    const note = wrapper ? `${plural(items.length, 'responsibility')} grouped inside ${wrapper.name}.` : `${plural(items.length, 'responsibility')} at the next level. Lower-level details are deliberately hidden.`;
    refs.content.innerHTML = sectionHeading(`Inside ${scope.name}`, note) + `<div class="card-grid">${items.map(componentCard).join('')}</div>`;
  }

  function renderFiles() {
    const files = scopedFiles().filter(item => state.fileFilter === 'ALL' || item.importance === state.fileFilter).sort((a, b) => ['CORE','SUPPORTING','UTILITY'].indexOf(a.importance) - ['CORE','SUPPORTING','UTILITY'].indexOf(b.importance) || b.lines - a.lines);
    refs.content.innerHTML = files.length ? sectionHeading('Implementation files', 'Core files are visually distinct; supporting and utility files remain available.') + `<div class="card-grid">${files.map(item => `<button class="file-card" style="--layer-color:${layerColor(item)}" type="button" data-file-id="${item.id}"><span class="card-top"><span>${escapeHtml(item.role.toUpperCase())}</span><span class="importance ${item.importance}">${item.importance}</span></span><h3>${escapeHtml(item.path)}</h3><p>${escapeHtml(item.description)}</p><span class="card-meta"><span>${item.lines || '—'} lines</span><span>${plural(item.functionCount, 'function')}</span><span>${plural(item.flowIds.length, 'flow')}</span></span></button>`).join('')}</div>` : '<div class="empty-state">No files match this scope and filter.</div>';
  }

  function renderFunctions() {
    const all = scopedFunctions().sort((a, b) => a.importanceRank - b.importanceRank);
    const functions = state.functionLimit ? all.slice(0, state.functionLimit) : all;
    refs.content.innerHTML = `${sectionHeading('Architectural importance ranking', `${plural(all.length, 'production function')} in scope. This is a heuristic, not an objective truth.`)}<div class="function-list">${functions.map(item => `<button class="function-row" style="--layer-color:${layerColor(item)}" type="button" data-function-id="${item.id}"><span class="function-rank">#${item.importanceRank}</span><span class="function-name">${escapeHtml(item.name)}()</span><span class="function-reason">${escapeHtml(item.importanceReasons[0])}</span><span class="function-file">${escapeHtml(item.source.path)}</span><span class="row-arrow">›</span></button>`).join('')}</div>`;
  }

  function renderFlows() {
    const flows = scopedFlows();
    if (!flows.length) { refs.content.innerHTML = '<div class="empty-state">No modeled user flow crosses this scope.</div>'; return; }
    if (!flows.some(flow => flow.id === state.selectedFlowId)) state.selectedFlowId = flows[0].id;
    const flow = flows.find(item => item.id === state.selectedFlowId);
    const steps = flow.steps.map((step, index) => {
      const place = entity(step.entityId);
      const symbol = String(step.symbol).toLowerCase();
      const matchingFunction = [...state.functions.values()].filter(item => {
        const escaped = item.name.toLowerCase().replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        return !item.test && isWithin(item.componentId, step.entityId) && new RegExp(`(^|[^a-z0-9_$])${escaped}([^a-z0-9_$]|$)`).test(symbol);
      }).sort((a, b) => b.name.length - a.name.length)[0];
      return `<button class="flow-step" style="--layer-color:${layerColor(place)}" type="button" data-flow-entity="${step.entityId}" ${matchingFunction ? `data-flow-function="${matchingFunction.id}"` : ''}><span class="step-number">${String(index + 1).padStart(2, '0')}</span><span class="step-place"><strong>${escapeHtml(place?.name || step.entityId)}</strong><small>${escapeHtml(step.symbol)}</small></span><span class="step-action">${escapeHtml(step.operation)}</span><span class="step-open">›</span></button>`;
    }).join('');
    refs.content.innerHTML = `<div class="flow-picker">${flows.map(item => `<button class="flow-pill ${item.id === flow.id ? 'active' : ''}" type="button" data-flow-id="${item.id}">${escapeHtml(item.name)}</button>`).join('')}</div><article class="flow-stage"><header class="flow-stage-head"><span>START → RESULT</span><h2>${escapeHtml(flow.name)}</h2><p>${escapeHtml(flow.trigger)} · ${escapeHtml(flow.summary)}</p></header><div class="flow-steps">${steps}</div><div class="flow-result"><b>Execution model:</b> ${escapeHtml(flow.concurrency)}</div></article>`;
  }

  function renderConnections() {
    const scope = entity(state.scopeId);
    const incoming = state.model.relationships.filter(rel => rel.to === scope.id);
    const outgoing = state.model.relationships.filter(rel => rel.from === scope.id);
    const cards = (relations, direction) => relations.length ? relations.map(rel => {
      const other = entity(direction === 'in' ? rel.from : rel.to);
      return `<button class="connection-card" type="button" data-connection-entity="${other.id}"><strong>${escapeHtml(other.name)}</strong><small>${escapeHtml(rel.type)} · ${escapeHtml(rel.description || '')}</small></button>`;
    }).join('') : '<div class="empty-state">No direct connections</div>';
    refs.content.innerHTML = `<div class="connections-focus"><div class="connection-column"><span class="connection-label">INCOMING</span>${cards(incoming, 'in')}</div><div class="connection-core" style="border-color:${layerColor(scope)};background:color-mix(in srgb, ${layerColor(scope)} 10%, var(--surface))"><strong>${escapeHtml(scope.name)}</strong><small>${escapeHtml(scope.type)}</small></div><div class="connection-column"><span class="connection-label">OUTGOING</span>${cards(outgoing, 'out')}</div></div>`;
  }

  function renderFindings() {
    const findings = state.model.findings.filter(item => state.findingFilter === 'ALL' || item.category === state.findingFilter).filter(item => state.scopeId === 'system' || (item.affectedEntityIds || []).some(id => isWithin(id, state.scopeId)));
    refs.content.innerHTML = findings.length ? `<div class="finding-list">${findings.map(item => `<article class="finding-card"><header><span class="finding-category">${escapeHtml(item.category)}</span><span class="importance ${item.severity === 'CRITICAL' ? 'CORE' : 'SUPPORTING'}">${escapeHtml(item.severity)}</span></header><h3>${escapeHtml(item.title)}</h3><p><b>Current:</b> ${escapeHtml(item.currentSituation)}</p><p style="margin-top:7px"><b>Why it matters:</b> ${escapeHtml(item.whyItMatters)}</p>${item.suggestedDirection ? `<p style="margin-top:7px"><b>Recommendation:</b> ${escapeHtml(item.suggestedDirection)}</p>` : ''}<div class="finding-evidence">${plural(item.evidence?.length || 0, 'source reference')}</div></article>`).join('')}</div>` : '<div class="empty-state">No findings match this scope and filter.</div>';
  }

  function renderSearch() {
    const query = state.query.toLowerCase();
    const entities = [...state.entities.values()].filter(item => `${item.name} ${item.summary || ''}`.toLowerCase().includes(query)).slice(0, 12);
    const files = [...state.files.values()].filter(item => `${item.path} ${item.role} ${item.description}`.toLowerCase().includes(query)).slice(0, 12);
    const functions = [...state.functions.values()].filter(item => !item.test && `${item.name} ${item.source.path} ${item.responsibility}`.toLowerCase().includes(query)).sort((a, b) => a.importanceRank - b.importanceRank).slice(0, 20);
    const flows = state.model.executionFlows.filter(item => `${item.name} ${item.summary}`.toLowerCase().includes(query));
    const group = (title, items, renderer) => items.length ? `${sectionHeading(title, plural(items.length, 'match', 'matches'))}<div class="result-list">${items.map(renderer).join('')}</div>` : '';
    const entityResults = group('Architecture', entities, item => `<button class="search-result" style="--layer-color:${layerColor(item)}" data-open-entity="${item.id}" type="button"><span>${escapeHtml(item.type)}</span><span><strong>${escapeHtml(item.name)}</strong><small>${escapeHtml(item.summary || '')}</small></span><i>›</i></button>`);
    const fileResults = group('Files', files, item => `<button class="search-result" style="--layer-color:${layerColor(item)}" data-file-id="${item.id}" type="button"><span>file</span><span><strong>${escapeHtml(item.path)}</strong><small>${escapeHtml(item.role)}</small></span><i>›</i></button>`);
    const functionResults = group('Functions', functions, item => `<button class="search-result" style="--layer-color:${layerColor(item)}" data-function-id="${item.id}" type="button"><span>#${item.importanceRank}</span><span><strong>${escapeHtml(item.name)}()</strong><small>${escapeHtml(item.source.path)}:${item.source.startLine}</small></span><i>›</i></button>`);
    const flowResults = group('Flows', flows, item => `<button class="search-result" style="--layer-color:var(--gui)" data-open-flow="${item.id}" type="button"><span>flow</span><span><strong>${escapeHtml(item.name)}</strong><small>${escapeHtml(item.summary)}</small></span><i>›</i></button>`);
    const results = `${entityResults}${fileResults}${functionResults}${flowResults}`;
    refs.content.innerHTML = results ? `<div class="search-groups">${results}</div>` : '<div class="empty-state">No result found.</div>';
  }

  function inspectLinks(items) {
    return items.length ? `<div class="inspect-links">${items.map(item => `<button class="inspect-link" type="button" data-${item.kind}-id="${item.id}"><span>${escapeHtml(item.label)}</span><small>›</small></button>`).join('')}</div>` : '<p>None in the current model.</p>';
  }

  function renderInspector() {
    if (!state.selection) {
      refs.inspector.innerHTML = '<div class="inspector-empty"><span>◇</span><strong>Details appear here</strong><p>Select a layer, component, file, function or flow step.</p></div>';
      return;
    }
    if (state.selection.kind === 'entity') return renderEntityInspector(entity(state.selection.id));
    if (state.selection.kind === 'file') return renderFileInspector(file(state.selection.id));
    if (state.selection.kind === 'function') return renderFunctionInspector(fn(state.selection.id));
  }

  function renderEntityInspector(item) {
    if (!item) return;
    const primary = state.model.primaryLayers.find(layer => layer.entityId === item.id);
    const files = [...state.files.values()].filter(entry => isWithin(entry.componentId, item.id));
    const functions = [...state.functions.values()].filter(entry => !entry.test && isWithin(entry.componentId, item.id)).sort((a, b) => a.importanceRank - b.importanceRank).slice(0, 8);
    const flows = state.model.executionFlows.filter(flow => flow.steps.some(step => isWithin(step.entityId, item.id)));
    const relations = state.model.relationships.filter(rel => rel.from === item.id || rel.to === item.id);
    refs.inspector.innerHTML = `<header class="inspector-head" style="--layer-color:${layerColor(item)}"><span class="inspector-kind">${escapeHtml(item.type.toUpperCase())}</span><h2>${escapeHtml(item.name)}</h2><p>${escapeHtml(item.summary || '')}</p></header><div class="inspector-body">
      ${section('PURPOSE', `<p>${escapeHtml(primary?.purpose || item.summary || '')}</p>`)}
      ${primary ? section('INPUT / OUTPUT', `<p><b>Input:</b> ${escapeHtml(primary.input)}</p><p style="margin-top:6px"><b>Output:</b> ${escapeHtml(primary.output)}</p>`) : ''}
      ${section('WHY IT EXISTS', `<p>${escapeHtml(primary?.why || `Keeps ${item.name.toLowerCase()} as a recognizable responsibility in the current implementation.`)}</p>`)}
      ${section('RESPONSIBILITIES', listHtml(item.responsibilities || []))}
      ${section('IMPLEMENTED BY', inspectLinks(files.slice(0, 10).map(entry => ({ kind: 'file', id: entry.id, label: entry.path }))))}
      ${section('IMPORTANT FUNCTIONS', inspectLinks(functions.map(entry => ({ kind: 'function', id: entry.id, label: `#${entry.importanceRank} ${entry.name}()` }))))}
      ${section('FLOWS', inspectLinks(flows.map(flow => ({ kind: 'flow', id: flow.id, label: flow.name }))))}
      ${section('DIRECT CONNECTIONS', `<p>${plural(relations.length, 'relationship')} — use the Connections view to focus them.</p>`)}
      ${section('SOURCE', sourceHtml(item))}
    </div>`;
  }

  function renderFileInspector(item) {
    if (!item) return;
    const owner = entity(item.componentId);
    const responsibilities = (item.componentIds || []).map(entity).filter(component => component && component.id !== owner?.id);
    const functions = item.importantFunctionIds.map(fn).filter(Boolean);
    const flows = item.flowIds.map(id => state.model.executionFlows.find(flow => flow.id === id)).filter(Boolean);
    refs.inspector.innerHTML = `<header class="inspector-head" style="--layer-color:${layerColor(item)}"><span class="inspector-kind">${item.importance} FILE</span><h2>${escapeHtml(item.name)}</h2><p>${escapeHtml(item.path)}</p></header><div class="inspector-body">
      ${section('ARCHITECTURAL ROLE', `<p>${escapeHtml(item.role)}</p>`)}
      ${section('WHAT IT DOES', `<p>${escapeHtml(item.description)}</p>`)}
      ${section('WHY IT EXISTS', `<p>${escapeHtml(item.why)}</p>`)}
      ${section('BELONGS TO', inspectLinks(owner ? [{ kind: 'entity', id: owner.id, label: owner.name }] : []))}
      ${section('IMPLEMENTS RESPONSIBILITIES', inspectLinks(responsibilities.map(component => ({ kind: 'entity', id: component.id, label: component.name }))))}
      ${section('DEPENDS ON', listHtml(item.dependsOn || []))}
      ${section('USED BY', listHtml(item.usedBy || []))}
      ${section('IMPORTANT FUNCTIONS', inspectLinks(functions.map(entry => ({ kind: 'function', id: entry.id, label: `#${entry.importanceRank} ${entry.name}()` }))))}
      ${section('FLOWS', inspectLinks(flows.map(flow => ({ kind: 'flow', id: flow.id, label: flow.name }))))}
      ${section('SOURCE', sourceHtml(item))}
    </div>`;
  }

  function renderFunctionInspector(item) {
    if (!item) return;
    const owner = entity(item.componentId);
    const ownerFile = file(item.fileId);
    const callees = item.callees.map(key => state.functionKeys.get(key)).filter(Boolean).slice(0, 12);
    const ownKey = `${item.source.path}:${item.source.startLine}:${item.name}`;
    const callers = [...state.functions.values()].filter(candidate => !candidate.test && candidate.callees.includes(ownKey)).slice(0, 12);
    const flows = item.flowIds.map(id => state.model.executionFlows.find(flow => flow.id === id)).filter(Boolean);
    refs.inspector.innerHTML = `<header class="inspector-head" style="--layer-color:${layerColor(item)}"><span class="inspector-kind">${escapeHtml(item.language.toUpperCase())} FUNCTION</span><h2>${escapeHtml(item.name)}()</h2><p>${escapeHtml(item.source.path)}:${item.source.startLine}</p></header><div class="inspector-body">
      ${section('IMPORTANCE', `<div class="rank-card"><strong>#${item.importanceRank}</strong><span>of ${state.model.metadata.productionFunctions} production functions in this static model</span></div>`)}
      ${section('WHY THIS RANK', listHtml(item.importanceReasons || []))}
      ${section('RESPONSIBILITY', `<p>${escapeHtml(item.responsibility)}</p>`)}
      ${section('SIGNATURE', `<pre class="code-signature">${escapeHtml(item.signature)}</pre>`)}
      ${section('INPUT / OUTPUT', `<p><b>Input:</b> ${escapeHtml(item.inputs)}</p><p style="margin-top:6px"><b>Output:</b> ${escapeHtml(item.output)}</p>`)}
      ${section('SIDE EFFECTS', listHtml(item.sideEffects || []))}
      ${section('BELONGS TO', inspectLinks([{ kind: 'entity', id: owner.id, label: owner.name }, { kind: 'file', id: ownerFile.id, label: ownerFile.path }]))}
      ${section('CALL METRICS', `<div class="metric-grid"><div class="metric"><strong>${item.callers}</strong><span>approx. callers</span></div><div class="metric"><strong>${item.calleeCount}</strong><span>approx. callees</span></div></div>`)}
      ${section('CALLERS', inspectLinks(callers.map(entry => ({ kind: 'function', id: entry.id, label: `${entry.name}()` }))))}
      ${section('CALLEES', inspectLinks(callees.map(entry => ({ kind: 'function', id: entry.id, label: `${entry.name}()` }))))}
      ${section('FLOWS', inspectLinks(flows.map(flow => ({ kind: 'flow', id: flow.id, label: flow.name }))))}
      ${section('SOURCE', sourceHtml(item))}
    </div>`;
  }

  document.addEventListener('click', event => {
    const target = event.target.closest('button, a.brand');
    if (!target) return;
    if (target.matches('a.brand')) { event.preventDefault(); navigate('overview', 'system'); return; }
    if (target.dataset.view) return navigate(target.dataset.view, state.scopeId);
    if (target.dataset.scope) return navigate(state.view === 'overview' ? 'components' : state.view, target.dataset.scope);
    if (target.dataset.openEntity) return openEntity(target.dataset.openEntity);
    if (target.dataset.connectionEntity) return navigate('connections', target.dataset.connectionEntity);
    if (target.dataset.entityId) return inspect('entity', target.dataset.entityId);
    if (target.dataset.fileId) return inspect('file', target.dataset.fileId);
    if (target.dataset.functionId) return inspect('function', target.dataset.functionId);
    if (target.dataset.openFlow || target.dataset.flowId && target.closest('.inspect-links')) {
      state.selectedFlowId = target.dataset.openFlow || target.dataset.flowId;
      return navigate('flows', state.scopeId);
    }
    if (target.dataset.flowId) { state.selectedFlowId = target.dataset.flowId; renderContent(); return; }
    if (target.dataset.flowEntity) {
      const functionId = target.dataset.flowFunction;
      return functionId ? inspect('function', functionId) : inspect('entity', target.dataset.flowEntity);
    }
    if (target.dataset.fileFilter) { state.fileFilter = target.dataset.fileFilter; renderToolbar(); renderFiles(); return; }
    if (target.dataset.functionLimit !== undefined) { state.functionLimit = Number(target.dataset.functionLimit); renderToolbar(); renderFunctions(); return; }
    if (target.dataset.findingFilter) { state.findingFilter = target.dataset.findingFilter; renderToolbar(); renderFindings(); return; }
    if (target.dataset.crumbKind === 'entity') return navigate(state.view, target.dataset.crumbId);
    if (target.dataset.crumbKind === 'file') return inspect('file', target.dataset.crumbId);
    if (target.dataset.crumbKind === 'function') return inspect('function', target.dataset.crumbId);
  });

  refs.back.addEventListener('click', () => navigate('overview', 'system'));
  refs.overviewMode.addEventListener('click', () => navigate('overview', 'system'));
  refs.deepMode.addEventListener('click', () => navigate('files', state.scopeId));
  refs.search.addEventListener('input', event => {
    state.query = event.target.value.trim();
    state.selection = null;
    render();
  });
  document.addEventListener('keydown', event => {
    if (event.key === '/' && document.activeElement !== refs.search) { event.preventDefault(); refs.search.focus(); }
    if (event.key === 'Escape' && state.query) { refs.search.value = ''; state.query = ''; render(); }
  });
  window.addEventListener('popstate', routeFromHash);

  loadModel();
})();
