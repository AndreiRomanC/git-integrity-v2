// Read-only requests run in the authenticated portal on explicit UI actions.
// Only bounded allowlisted data leaves it; never credentials or raw HTML.
(async () => {
  const args = __DDT_ARGUMENTS__;
  let session_token = false, prefix_fallback = false, metadata_fallback = false;
  let http_requests = 0, diagnostics_omitted = 0;
  const diagnostics = [];
  let server_metadata;
  const trace = entry => {
    // Keep the first context and the latest failure even in long catalogs.
    if (diagnostics.length === 40) { diagnostics.pop(); diagnostics_omitted++; }
    diagnostics.push(entry);
  };
  const reply = data => {
    const target = new URL('https://polarion.vitesco.io/polarion/__ddt_search_result');
    target.searchParams.set('request', args.request);
    const compatibility = () => metadata_fallback ? { metadata_fallback: true } : {};
    const serialize = () => JSON.stringify({ session_token, prefix_fallback, ...compatibility(), diagnostics, diagnostics_omitted, http_requests, server_metadata, ...data });
    const oversized = value => value.length > 20000 && encodeURIComponent(value).replace(/%[0-9A-F]{2}/g, 'x').length > 60000;
    let payload = serialize();
    // Stay below the native bridge's UTF-8 limit. Return a useful error
    // instead of timing out. Diagnostics must not displace previously valid
    // search results or snapshots: reduce diagnostic detail first.
    if (oversized(payload) && diagnostics.length > 1) {
      diagnostics_omitted += diagnostics.length - 1;
      diagnostics.splice(0, diagnostics.length - 1); payload = serialize();
    }
    if (oversized(payload) && diagnostics.length) {
      diagnostics_omitted += diagnostics.length; diagnostics.length = 0; payload = serialize();
    }
    if (oversized(payload)) {
      // At the exact old size boundary even the diagnostic counters may not
      // fit. Preserve the legacy reply instead of breaking a successful read.
      const legacy = JSON.stringify({ session_token, prefix_fallback, ...compatibility(), ...data });
      payload = !oversized(legacy) ? legacy : JSON.stringify({ code: 'response_limit', session_token, diagnostics, diagnostics_omitted, http_requests });
    }
    target.searchParams.set('data', payload);
    location.href = target.href; // intercepted/cancelled locally, never sent
  };
  if (location.origin !== 'https://polarion.vitesco.io') return reply({ code: 'wrong_origin' });
  const abort = new AbortController();
  const catalog = args.kind === 'projects' || args.kind === 'documents';
  const timeout = setTimeout(() => abort.abort(), args.kind === 'projects' ? 90000 : 25000);
  const failureCode = error => error?.name === 'AbortError' ? 'timeout'
    : ['auth_required','forbidden','api_unavailable','query_rejected','http_error','not_json','network_error','timeout','wrong_origin','invalid_response','catalog_limit','response_limit'].includes(error?.code) ? error.code : 'network_error';
  const text = (value, limit = 500) => String(value ?? '').slice(0, limit);
  const enumValue = value => text(value && typeof value === 'object' ? value.id || value.name : value, 240);
  const validId = (id, count) => typeof id === 'string' && id.split('/').length === count && id.split('/').every(p => p && p !== '.' && p !== '..' && p.length <= 240 && !/[\x00-\x1f\\]/.test(p));
  const hasErrors = item => Boolean(item?.meta?.errors?.length);
  // Validation can fail after a successful GET. Keep it attached to that
  // response (not whichever request happened last), without retaining bodies
  // in the diagnostic report or making any additional request.
  const responseDiagnostics = new WeakMap();
  const invalidResponse = (data, reason, catalog_pages = 0) => {
    const entry = responseDiagnostics.get(data);
    if (entry) { entry.code = 'invalid_response'; entry.reason = reason; }
    throw { code: 'invalid_response', http: entry?.http || 0, catalog_pages };
  };
  const route = url => {
    const p = url.pathname.split('/').slice(4);
    if (p.length === 2 && p[0] === 'all' && p[1] === 'documents') return 'all_documents';
    if (p.length === 1 && ['metadata', 'user', 'projects'].includes(p[0])) return p[0];
    if (p[0] !== 'projects' || !p[1]) return 'unknown';
    if (p.length === 2) return 'project';
    if (p[2] === 'workitems') return p.length === 3 ? 'workitems' : p.length === 4 ? 'workitem' : p.length === 5 && p[4] === 'linkedworkitems' ? 'links' : 'unknown';
    if (p.length === 3 && ['documents', 'spaces'].includes(p[2])) return p[2];
    if (p[2] !== 'spaces' || !p[3]) return 'unknown';
    if (p.length === 5 && ['documents', 'pages'].includes(p[4])) return 'space_' + p[4];
    if (p.length === 6 && ['documents', 'pages'].includes(p[4])) return p[4] === 'documents' ? 'document' : 'page';
    return p.length === 7 && p[4] === 'documents' && p[6] === 'parts' ? 'parts' : 'unknown';
  };
  const resourceTypes = ['metadata', 'users', 'projects', 'workitems', 'documents', 'pages', 'spaces', 'document_parts', 'linkedworkitems'];
  const safeFields = ['id','name','title','type','status','moduleName','moduleFolder','created','updated','description','assignee','module','workItem','sourceWorkItem','level','revision','role','nextPart','previousPart','@all'];
  const safeRelationships = ['assignee','module','workItem','sourceWorkItem','nextPart','previousPart'];
  const technicalType = value => {
    const id = value && typeof value === 'object' ? value.id : value;
    return typeof id === 'string' && /^[A-Za-z][A-Za-z0-9_.-]{0,79}$/.test(id) ? id : '';
  };
  const responseFacts = data => {
    const rows = (Array.isArray(data.data) ? data.data : data.data ? [data.data] : []).slice(0, 10);
    const included = Array.isArray(data.included) ? data.included.slice(0, 10) : [];
    const fields = new Set(), relationships = new Set(), types = new Set();
    let module_links = 0, workitem_links = 0;
    for (const row of [...rows, ...included]) {
      if (!resourceTypes.includes(row?.type)) continue;
      for (const key of Object.keys(row.attributes || {})) if (safeFields.includes(key)) fields.add(row.type + '.' + key);
      for (const key of Object.keys(row.relationships || {})) if (safeRelationships.includes(key)) relationships.add(row.type + '.' + key);
      if (['workitems', 'documents', 'document_parts'].includes(row.type)) {
        const type = technicalType(row.attributes?.type); if (type) types.add(type);
      }
    }
    for (const row of rows) {
      if (row?.relationships?.module?.data?.type === 'documents') module_links++;
      if (row?.relationships?.workItem?.data?.type === 'workitems') workitem_links++;
    }
    return { shape: Array.isArray(data.data) ? 'list' : data.data ? 'single' : 'missing', sample_rows: rows.length,
      returned_fields: [...fields].sort().slice(0, 32), relationship_fields: [...relationships].sort().slice(0, 32),
      item_types: [...types].sort().slice(0, 12), module_links, workitem_links,
      included_workitems: included.filter(row => row?.type === 'workitems' && !hasErrors(row)).length };
  };
  // Error bodies are used only to derive fixed categories. No message, URL,
  // header value, query text or response content is sent to the desktop log.
  const errorInfo = async response => {
    if (!response.headers.get('content-type')?.includes('json')) return {};
    let body;
    try {
      if (response.body?.getReader) {
        const reader = response.body.getReader(); let size = 0, chunks = [];
        try {
          for (;;) {
            const { done, value } = await reader.read(); if (done) break;
            size += value.byteLength; if (size > 8192) return { reason: 'error_body_limit' };
            chunks.push(value);
          }
          const bytes = new Uint8Array(size); let offset = 0;
          for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
          body = JSON.parse(new TextDecoder().decode(bytes));
        } finally { await reader.cancel().catch(() => {}); }
      } else body = await response.json(); // Test adapters; real Fetch uses the bounded stream above.
    } catch { return {}; }
    const errors = Array.isArray(body?.errors) ? body.errors.slice(0, 3) : [];
    const words = errors.map(e => String(e.title || '').slice(0, 500) + ' ' + String(e.detail || '').slice(0, 1500)).join(' ').toLowerCase();
    const source = errors.find(e => e.source?.parameter)?.source?.parameter || '';
    const parameter = source === 'query' ? 'query' : /^fields\[/.test(source) ? 'fields' : /^page\[/.test(source) ? 'page' : ['sort','include','revision'].includes(source) ? source : source ? 'other' : '';
    const reason = /(?:leading|first character|start[^.]{0,30})[^.]{0,70}(?:wildcard|\*|\?)|wildcard[^.]{0,70}(?:leading|first|start)/.test(words) ? 'leading_wildcard'
      : /(?:unknown|invalid|unsupported|unrecognized)[^.]{0,60}(?:field)|field[^.]{0,60}(?:unknown|invalid|unsupported|not found)/.test(words) ? 'unknown_field'
      : /(?:query|lucene|parse|syntax)/.test(words) ? 'query_syntax'
      : /(?:unknown|invalid|unsupported)[^.]{0,60}parameter/.test(words) ? 'unsupported_parameter'
      : /(?:permission|forbidden|not allowed|access denied)/.test(words) ? 'permission_denied'
      : /(?:unauthoriz|authentication|sign.in)/.test(words) ? 'authentication_required'
      : /(?:not found|does not exist|no such)/.test(words) ? 'not_found' : 'unclassified';
    const error_class = /toomanyclauses|too many clauses/.test(words) ? 'clause_limit'
      : /parse\s*exception|queryparser|query parse|parse error|syntax error/.test(words) ? 'query_parser'
      : /unsupportedoperationexception|unsupported operation/.test(words) ? 'unsupported_operation'
      : /illegalargumentexception/.test(words) ? 'invalid_argument' : '';
    return { reason, parameter, error_class };
  };
  const snapshot = (item, summary = false) => {
    const a = item.attributes || {}, kind = item.type === 'documents' ? 'document' : 'workitem';
    const fields = {};
    for (const key of ['created', 'updated', 'plannedStart', 'plannedEnd', 'dueDate', 'startDate', 'endDate', 'actualStart', 'actualEnd', 'resolvedOn', 'resolution', 'priority', 'initialEstimate', 'remainingEstimate', 'timeSpent', 'moduleFolder', 'moduleName', 'outlineNumber']) {
      if (a[key] != null) fields[key] = enumValue(a[key]);
    }
    if (item.revision) fields.revision = text(item.revision, 120);
    if (metadata_fallback && kind === 'document') fields.compatibilityNote = 'This server rejected optional metadata fields. The document was read with title only; unavailable fields are not inferred.';
    const assignee = item.relationships?.assignee;
    if (assignee?.meta?.totalCount > (assignee?.data?.length || 0)) fields.assigneeNote = 'The server returned only part of the assignee list.';
    const raw = summary ? '' : text(a.description?.value ?? a.description ?? '', 12000);
    if (!summary && String(a.description?.value ?? a.description ?? '').length > 12000) fields.descriptionNote = 'Description truncated to 12000 characters in this local overview.';
    const description = raw.replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/gi, '').replace(/<br\s*\/?\s*>|<\/p>/gi, '\n').replace(/<[^>]*>/g, '').replace(/&(?:nbsp|amp|lt|gt|quot);/g, entity => ({ '&nbsp;': ' ', '&amp;': '&', '&lt;': '<', '&gt;': '>', '&quot;': '"' }[entity]));
    return { id: item.id, title: text(a.title || a.moduleName || item.id), kind, item_type: enumValue(a.type), status: enumValue(a.status),
      assignees: (Array.isArray(assignee?.data) ? assignee.data : []).slice(0, 100).map(u => text(u.id, 240)), fields, description,
      fetched_at: new Date().toISOString() };
  };
  try {
    const headers = { Accept: 'application/json' };
    if (typeof window.getRestApiToken === 'function') {
      const token = window.getRestApiToken();
      if (token) { headers['X-Polarion-REST-Token'] = token; session_token = true; }
    }
    const options = { method: 'GET', headers, credentials: 'same-origin', redirect: 'manual', signal: abort.signal };
    const request = async (url, step = 'primary', bounded = false, query_variant = '') => {
      const parsed = new URL(url);
      if (parsed.origin !== location.origin || !parsed.pathname.startsWith('/polarion/rest/v1/')) throw { code: 'invalid_response' };
      if (http_requests >= 210) throw { code: 'catalog_limit' };
      const started = Date.now(), query = parsed.searchParams.get('query') || '';
      const entry = { step, route: route(parsed), code: 'network_error', http: 0, page: Number(parsed.searchParams.get('page[number]') || 1), query: Boolean(query), id_star: /(?:^|[\s(])id:\s*\*/.test(query), leading_wildcard: /(?:^|[\s(:])[*?]/.test(query), media: '', reason: '', parameter: '', resource_type: '', count: 0, more: false, allow: [], query_variant, query_fields: [] };
      const queryFields = ['id','title','type','module','module.id','location','location.tokenized','project.id','assignee.id','HAS_VALUE'];
      entry.query_fields = [...new Set([...query.matchAll(/\b([A-Za-z][A-Za-z0-9_.]*)\s*:/g)].map(match => match[1]).filter(field => queryFields.includes(field)))].sort();
      entry.query_kind = !query ? 'none' : /^(?:module(?:\.id)?|location(?:\.tokenized)?|HAS_VALUE):/.test(query) ? 'module_name' : /^project\.id:/.test(query) ? 'project_scope' : /(?:id|title):/.test(query) ? 'work_item_search' : 'other';
      entry.requested_fields = [];
      for (const [key, value] of parsed.searchParams) {
        const resource = /^fields\[([a-z_]+)\]$/.exec(key)?.[1];
        if (resourceTypes.includes(resource)) for (const field of value.split(',')) if (safeFields.includes(field)) entry.requested_fields.push(resource + '.' + field);
      }
      entry.requested_fields = [...new Set(entry.requested_fields)].sort().slice(0, 32);
      const controller = new AbortController(), cancel = () => controller.abort();
      abort.signal.addEventListener('abort', cancel, { once: true });
      if (abort.signal.aborted) controller.abort();
      const timer = bounded ? setTimeout(cancel, 5000) : null;
      http_requests++;
      try {
        const response = await fetch(parsed.href, { ...options, signal: controller.signal });
        const http = entry.http = response.status, media = response.headers.get('content-type') || '';
        entry.media = /json/i.test(media) ? 'json' : /html/i.test(media) ? 'html' : /text\//i.test(media) ? 'text' : media ? 'other' : '';
        entry.allow = String(response.headers.get('allow') || '').split(',').map(m => m.trim().toUpperCase()).filter(m => ['GET','HEAD','OPTIONS','POST','PATCH','PUT','DELETE'].includes(m)).slice(0, 7);
        if (response.type === 'opaqueredirect') throw { http, code: 'auth_required' };
        if (!response.ok) {
          Object.assign(entry, await errorInfo(response));
          if (http === 405) entry.reason = 'method_not_allowed';
          throw { http, parameter: entry.parameter, code: http === 401 ? 'auth_required' : http === 403 ? 'forbidden' : [404,405,501,503].includes(http) ? 'api_unavailable' : http === 400 ? 'query_rejected' : 'http_error' };
        }
        if (entry.media !== 'json') throw { http, code: 'not_json' };
        let data;
        try { data = await response.json(); }
        catch (error) {
          if (error?.name !== 'SyntaxError') throw error;
          entry.reason = 'invalid_json'; throw { http, code: 'invalid_response' };
        }
        if (!data || typeof data !== 'object' || Array.isArray(data)) {
          entry.reason = 'invalid_payload'; throw { http, code: 'invalid_response' };
        }
        responseDiagnostics.set(data, entry);
        const resource = Array.isArray(data.data) ? data.data[0] : data.data;
        entry.resource_type = resourceTypes.includes(resource?.type) ? resource.type : '';
        entry.count = Array.isArray(data.data) ? data.data.length : data.data ? 1 : 0;
        entry.more = Boolean(data.links?.next);
        Object.assign(entry, responseFacts(data));
        if (hasErrors(data)) { entry.reason = 'partial_errors'; throw { http, code: 'invalid_response' }; }
        entry.code = 'ok';
        return data;
      } catch (error) {
        // DOMException AbortError has a numeric code (20), not our protocol
        // string. Never let it turn a timeout into an invalid bridge reply.
        entry.code = failureCode(error);
        throw { code: entry.code, http: entry.http, parameter: entry.parameter };
      } finally {
        clearTimeout(timer); abort.signal.removeEventListener('abort', cancel);
        entry.duration_ms = Math.min(600000, Math.max(0, Date.now() - started)); trace(entry);
      }
    };
    if (args.kind === 'diagnostics') {
      const started = Date.now(), confirmed = new Set(); let sampleId = '';
      for (const probe of args.probes || []) {
        let probeUrl = probe.url;
        if (probe.step === 'workitem_sample' && sampleId) {
          const url = new URL(probe.url), [project, item] = sampleId.split('/');
          url.pathname = '/polarion/rest/v1/projects/' + encodeURIComponent(project) + '/workitems/' + encodeURIComponent(item); probeUrl = url.href;
        }
        if (probe.step === 'known_item_query' && sampleId) {
          const url = new URL(probe.url), item = sampleId.split('/')[1];
          url.searchParams.set('query', 'id:"' + item.replace(/[\\"]/g, '\\$&') + '"'); probeUrl = url.href;
        }
        if (Date.now() - started >= 25000 || abort.signal.aborted || (probe.requires && !confirmed.has(probe.requires))) {
          const parsed = new URL(probeUrl);
          trace({ step: probe.step, route: route(parsed), query_variant: probe.variant || '', code: 'not_run', reason: probe.requires && !confirmed.has(probe.requires) ? 'prerequisite_missing' : 'budget', duration_ms: 0 }); continue;
        }
        if (probe.step === 'workitem_sample' && !sampleId) {
          trace({ step: probe.step, route: 'workitem', code: 'not_run', reason: 'sample_missing', duration_ms: 0 }); continue;
        }
        try {
          const data = await request(probeUrl, probe.step, true, probe.variant || ''), last = diagnostics[diagnostics.length - 1];
          const expected = ['document_exact', 'document_parts', 'document_parts_expanded', 'page_exact', 'metadata', 'current_user', 'project_access'].includes(probe.step)
            ? ({ document_exact:'documents', document_parts:'document_parts', document_parts_expanded:'document_parts', page_exact:'pages', metadata:'metadata', current_user:'users', project_access:'projects' })[probe.step]
            : ['documents_catalog','documents_global','space_documents'].includes(probe.step) ? 'documents' : probe.step === 'space_pages' ? 'pages' : probe.step === 'spaces' ? 'spaces' : 'workitems';
          const single = ['document_exact', 'page_exact', 'metadata', 'current_user', 'project_access', 'workitem_sample'].includes(probe.step);
          const rows = Array.isArray(data.data) ? data.data : data.data ? [data.data] : null;
          if (!rows || single === Array.isArray(data.data) || rows.some(item => item.type !== expected || hasErrors(item))) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
          if (['workitem_modules', 'document_name_query', 'document_query', 'known_document_query', 'known_item_query'].includes(probe.step) && rows.some(row => !validId(row.id, 2) || !row.id.startsWith(args.project + '/'))) {
            last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue;
          }
          if (['workitem_modules', 'document_name_query', 'document_query'].includes(probe.step) && probe.variant !== 'location_tokens_prefix' && rows.some(row =>
            !row.relationships?.module?.data || row.relationships.module.data.type !== 'documents' || !validId(row.relationships.module.data.id, 3))) {
            last.code = 'invalid_response'; last.reason = 'relationship_missing'; continue;
          }
          if (['document_exact', 'page_exact'].includes(probe.step)) {
            if (data.data?.id !== args.resourceId) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
            if (probe.step === 'document_exact') confirmed.add(probe.step);
          }
          if (probe.step === 'workitem_sample' && data.data?.id !== sampleId) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
          if (probe.step === 'workitem_sample') confirmed.add(probe.step);
          if (probe.known === 'item') last.known_match = rows.some(row => row.id === sampleId);
          if (probe.known === 'document') last.known_match = rows.some(row => row.relationships?.module?.data?.type === 'documents' && row.relationships.module.data.id === args.resourceId);
          if (probe.step === 'document_parts_expanded') {
            if (rows.length > 10 || rows.some(row => !row.id?.startsWith(args.resourceId + '/'))) { last.code = 'invalid_response'; last.reason = 'unexpected_resource'; continue; }
            confirmed.add(probe.step);
            sampleId = rows.filter(row => !hasErrors(row.relationships?.workItem)).map(row => row.relationships?.workItem?.data)
              .find(ref => ref?.type === 'workitems' && validId(ref.id, 2) && ref.id.startsWith(args.project + '/'))?.id || '';
          }
          if (probe.step === 'metadata') {
            const a = data.data?.attributes || {};
            server_metadata = {
              version: /^\d{1,4}(?:\.\d{1,8}){1,3}$/.test(String(a.version || '')) ? String(a.version) : '',
              build: /^\d{8}-\d{4}(?:-[A-Za-z0-9]{1,24}){0,3}$/.test(String(a.build || '')) ? String(a.build) : '',
              max_page_size: Number.isSafeInteger(a.apiProperties?.maxPageSize) && a.apiProperties.maxPageSize >= 0 && a.apiProperties.maxPageSize <= 1000000 ? a.apiProperties.maxPageSize : 0,
            };
          }
        } catch { /* Each failed GET is reported independently; never try a write. */ }
      }
      return reply({ code: 'ok' }); // Completed checks, NOT a claim that every endpoint is supported.
    }
    if (args.kind === 'documents') {
      // Collection GETs are absent on some older Polarion servers, although
      // exact Document reads and Work Item -> module relationships work.
      // All fallbacks remain project-scoped GETs in this same browser session.
      const key = '__ddtDocumentSearchV2_' + args.project, token = headers['X-Polarion-REST-Token'];
      const queryKey = String(args.query || '').trim().toLowerCase(), cached = window[key];
      const words = queryKey.split(/\s+/).filter(Boolean);
      const matchesQuery = item => words.every(word => (item.id + ' ' + item.title).toLowerCase().includes(word));
      const catalog_cached = Boolean(token && cached?.token === token && Date.now() - cached.time < 120000 &&
        (cached.source !== 'work_item_modules' || (cached.query === queryKey && cached.items?.some(matchesQuery))));
      let items = catalog_cached ? cached.items : [], catalog_source = catalog_cached ? cached.source : '', catalog_pages = 0;
      let document_query_variant = catalog_cached ? cached.variant || '' : '';
      if (!catalog_cached) {
        delete window[key];
        const moduleQueries = Array.isArray(args.documentModuleQueries) ? args.documentModuleQueries.slice(0, 7)
          : args.documentModulesUrl ? [{ url: args.documentModulesUrl, variant: 'module_id_contains' }] : [];
        const sources = [{ source: 'project_documents', url: args.url, step: 'catalog_page' },
          { source: 'all_documents', url: args.documentGlobalUrl, step: 'document_global' },
          ...moduleQueries.map(candidate => ({ source: 'work_item_modules', url: candidate.url, variant: candidate.variant, step: 'document_modules' }))].filter(s => s.url);
        const invalidDocumentResponse = reason => {
          const last = diagnostics[diagnostics.length - 1];
          if (last) { last.code = 'invalid_response'; last.reason = reason; }
          throw { code: 'invalid_response', http: last?.http || 0 };
        };
        let acceptedModuleLookup = false;
        for (let index = 0; index < sources.length; index++) {
          const source = sources[index], modules = source.source === 'work_item_modules';
          const seenRows = new Set(), documents = new Map();
          let unavailable = false;
          for (let page = 1; ; page++) {
            if (page > 200) throw { code: 'catalog_limit' };
            const url = new URL(source.url); url.searchParams.set('page[number]', String(page));
            let data;
            try { data = await request(url.href, source.step, false, source.variant || ''); }
            catch (error) {
              // Do not retry authentication/permission errors, transient server
              // failures, invalid responses or a later failed catalog page.
              if (page === 1 && !modules && [404, 405, 501].includes(error.http) && index < sources.length - 1) { unavailable = true; break; }
              // Retry only a first-page query rejection. Fields/auth/transport
              // errors and failed later pages never become partial success.
              if (page === 1 && modules && error.http === 400 && error.parameter !== 'fields'
                && (index < sources.length - 1 || acceptedModuleLookup)) { unavailable = true; break; }
              throw error;
            }
            catalog_pages++;
            if (!Array.isArray(data.data) || data.data.length > 100) invalidDocumentResponse('unexpected_resource');
            let added = 0;
            for (const row of data.data) {
              if (hasErrors(row) || row.type !== (modules ? 'workitems' : 'documents') || !validId(row.id, modules ? 2 : 3) ||
                (source.source !== 'all_documents' && !row.id.startsWith(args.project + '/'))) invalidDocumentResponse('unexpected_resource');
              if (seenRows.has(row.id)) continue;
              seenRows.add(row.id); added++;
              let doc = row;
              if (modules) {
                const relationship = row.relationships?.module;
                // A location index may also match standalone Work Items.
                // Never invent a Document association for those rows.
                if (!hasErrors(relationship) && (source.variant === 'workitem_title_prefix' && !relationship?.data
                  || source.variant === 'location_tokens_prefix' && relationship?.data === null)) continue;
                // A name query returned matching Work Items but the requested
                // relationship was omitted: don't claim there are no documents.
                if (!relationship || hasErrors(relationship) || !relationship.data) invalidDocumentResponse('relationship_missing');
                doc = relationship.data;
                if (doc.type !== 'documents' || !validId(doc.id, 3)) invalidDocumentResponse('unexpected_resource');
              }
              if (!doc.id.startsWith(args.project + '/')) continue;
              const a = doc.attributes || {};
              documents.set(doc.id, { id: doc.id, title: text(a.title || a.moduleName || doc.id.split('/').pop()), prefix: '', item_type: enumValue(a.type) });
            }
            if (!data.links?.next) break;
            // Many consecutive pages can legitimately refer to the same
            // document. Detect loops using Work Item IDs, not Document IDs.
            if (!added) invalidDocumentResponse('unexpected_resource');
          }
          if (unavailable) continue;
          catalog_source = source.source; items = [...documents.values()].sort((a, b) => a.id.localeCompare(b.id));
          if (modules) {
            acceptedModuleLookup = true; document_query_variant = source.variant || 'module_id_contains';
            // A valid zero-result prefix does not disprove another targeted
            // index representation. Do not label discovery as a full catalog.
            if (!items.some(matchesQuery) && index < sources.length - 1) continue;
          }
          break;
        }
        if (token && (catalog_source !== 'work_item_modules' || items.some(matchesQuery))) window[key] = { token, time: Date.now(), items, source: catalog_source, query: queryKey, variant: document_query_variant };
      }
      const matches = items.filter(matchesQuery);
      const start = ((args.page || 1) - 1) * 25;
      return reply({ code: 'ok', http: 200, catalog_source, catalog_cached, catalog_pages, document_query_variant,
        document_search_limited: catalog_source === 'work_item_modules', received: items.length, matched: matches.length,
        items: matches.slice(start, start + 25).map(({ id, title, item_type }) => ({ id, title, item_type })), more: matches.length > start + 25 });
    }
    if (catalog) {
      const isProject = args.kind === 'projects', key = isProject ? '__ddtProjectCatalog' : '__ddtDocumentCatalog_' + args.project;
      const token = headers['X-Polarion-REST-Token'], cached = window[key];
      const catalog_cached = Boolean(token && cached?.token === token && Date.now() - cached.time < 120000);
      let items = catalog_cached ? cached.items : [], catalog_pages = 0;
      if (!catalog_cached) {
        delete window[key];
        const seen = new Set();
        for (let page = 1; ; page++) {
          if (page > 200) throw { code: 'catalog_limit', catalog_pages };
          const url = new URL(args.url); url.searchParams.set('page[number]', String(page)); url.searchParams.set('sort', 'id');
          const data = await request(url.href, 'catalog_page'); catalog_pages++;
          if (!Array.isArray(data.data)) invalidResponse(data, 'expected_list', catalog_pages);
          if (data.data.some(hasErrors)) invalidResponse(data, 'partial_errors', catalog_pages);
          if (data.data.some(item => isProject ? item?.type !== 'projects' || !/^[\w-]+$/.test(item.id) : item?.type !== 'documents' || !validId(item.id, 3) || !item.id.startsWith(args.project + '/'))) invalidResponse(data, 'unexpected_resource', catalog_pages);
          let added = 0;
          for (const item of data.data) {
            if (seen.has(item.id)) continue;
            seen.add(item.id); added++;
            const a = item.attributes || {};
            items.push({ id: item.id, title: text(isProject ? a.name || item.id : a.title || a.moduleName || item.id), prefix: text(a.trackerPrefix), item_type: enumValue(a.type) });
          }
          if (!data.links?.next) break;
          if (!added) invalidResponse(data, 'duplicate_item', catalog_pages);
        }
        if (token) window[key] = { token, time: Date.now(), items };
      }
      const words = String(args.query || '').toLowerCase().trim().split(/\s+/).filter(Boolean);
      const matches = items.filter(item => words.every(word => (item.id + ' ' + item.title + ' ' + item.prefix).toLowerCase().includes(word)));
      const start = ((args.page || 1) - 1) * 25;
      return reply({ code: 'ok', http: 200, catalog_cached, catalog_pages, received: items.length, matched: matches.length,
        items: matches.slice(start, start + 25).map(({ id, title, item_type }) => ({ id, title, item_type })), more: matches.length > start + 25 });
    }
    if (args.kind === 'tasks' && args.taskGroups) {
      const groups = args.taskGroups;
      if (!Array.isArray(groups) || !groups.length || groups.length > 6 || !Array.isArray(args.taskCursor)) throw { code: 'invalid_response' };
      let [group, serverPage, offset] = args.taskCursor, reads = 0, received = 0;
      const items = [], seen = new Set();
      // At most one display page plus lookahead, never a repository-wide
      // catalog. The offset lets a display page span status groups without
      // dropping the unused tail of a server page.
      while (group < groups.length) {
        // Even servers returning fewer rows than requested stay bounded:
        // 25 displayed rows, up to six empty groups and one lookahead.
        if (++reads > 32 || !Number.isInteger(group) || !Number.isInteger(serverPage) || !Number.isInteger(offset)
          || group < 0 || serverPage < 1 || serverPage > 1000 || offset < 0 || offset > 24) throw { code: 'catalog_limit' };
        const urls = groups[group];
        const atPage = value => { const url = new URL(value); url.searchParams.set('page[number]', String(serverPage)); return url.href; };
        let data;
        try { data = await request(atPage(urls.url)); }
        catch (error) {
          if (error.http !== 400 || !urls.fallbackUrl || urls.fallbackUrl === urls.url) throw error;
          prefix_fallback = true; data = await request(atPage(urls.fallbackUrl), 'prefix_fallback');
        }
        if (!Array.isArray(data.data)) invalidResponse(data, 'expected_list');
        if (data.data.length > 25) invalidResponse(data, 'page_size_exceeded');
        if (offset > data.data.length) invalidResponse(data, 'page_offset_changed');
        if (!data.data.length && data.links?.next) invalidResponse(data, 'empty_page_with_next');
        received += data.data.length;
        for (; offset < data.data.length; offset++) {
          const item = data.data[offset];
          if (item?.type !== 'workitems') invalidResponse(data, 'unexpected_resource');
          if (!validId(item.id, 2)) invalidResponse(data, 'invalid_item_id');
          if (!item.id.startsWith(args.project + '/')) invalidResponse(data, 'wrong_project');
          if (hasErrors(item)) invalidResponse(data, 'partial_errors');
          const itemType = enumValue(item.attributes?.type);
          if (!itemType) invalidResponse(data, 'missing_task_type');
          if (itemType !== 'task') invalidResponse(data, 'unexpected_task_type');
          if (seen.has(item.id)) invalidResponse(data, 'duplicate_item');
          const matches = urls.statuses?.some(status => status.toLowerCase() === enumValue(item.attributes?.status).toLowerCase());
          if (urls.exclude ? matches : !matches) invalidResponse(data, enumValue(item.attributes?.status) ? 'task_status_mismatch' : 'missing_task_status');
          // Look ahead for a real next result, not just a possibly empty group.
          if (items.length === 25) return reply({ code: 'ok', http: 200, items, received, more: true, task_next_cursor: [group, serverPage, offset].join(':') });
          seen.add(item.id);
          items.push({ id: item.id.split('/').pop(), title: text(item.attributes?.title || item.id), item_type: 'task', status: enumValue(item.attributes?.status) });
        }
        offset = 0;
        if (data.links?.next) serverPage++;
        else { group++; serverPage = 1; }
      }
      return reply({ code: 'ok', http: 200, items, received, more: false, task_next_cursor: '' });
    }
    let data;
    try { data = await request(args.url); }
    catch (error) {
      if (error.http !== 400 || !args.fallbackUrl || args.fallbackUrl === args.url) throw error;
      if (args.readFallback) { metadata_fallback = true; data = await request(args.fallbackUrl, 'metadata_fallback'); }
      else { prefix_fallback = true; data = await request(args.fallbackUrl, 'prefix_fallback'); }
    }
    if (args.kind === 'user') {
      if (hasErrors(data.data)) invalidResponse(data, 'partial_errors');
      if (data.data?.type !== 'users' || typeof data.data.id !== 'string' || !/^[\w.@-]{1,120}$/.test(data.data.id)) invalidResponse(data, 'unexpected_resource');
      return reply({ code: 'ok', http: 200, user_id: data.data.id, user_name: text(data.data.attributes?.name) });
    }
    if (args.kind === 'workitem' || args.kind === 'document') {
      const item = data.data;
      if (hasErrors(item)) invalidResponse(data, 'partial_errors');
      if (!item || item.id !== args.resourceId || item.type !== (args.kind === 'document' ? 'documents' : 'workitems')) invalidResponse(data, 'unexpected_resource');
      return reply({ code: 'ok', http: 200, snapshot: snapshot(item) });
    }
    if (!Array.isArray(data.data)) invalidResponse(data, 'expected_list');
    if (args.kind === 'elements' || args.kind === 'links') {
      if (data.data.length > 25) invalidResponse(data, 'page_size_exceeded');
      if (data.included != null && !Array.isArray(data.included)) invalidResponse(data, 'expected_list');
      if (data.included?.some(item => item == null)) invalidResponse(data, 'unexpected_resource');
      const elements = [], included = new Map((data.included || []).filter(i => i?.type === 'workitems').map(i => [i.id, i]));
      let incomplete = 0;
      for (const part of data.data.slice(0, 25)) {
        if (hasErrors(part)) invalidResponse(data, 'partial_errors');
        if (part?.type !== (args.kind === 'elements' ? 'document_parts' : 'linkedworkitems')) invalidResponse(data, 'unexpected_resource');
        if (args.kind === 'elements' && !part.id?.startsWith(args.resourceId + '/')) invalidResponse(data, 'unexpected_resource');
        if (args.kind === 'elements' && part.attributes?.type !== 'workitem') continue;
        if (args.kind === 'links' && part.relationships?.sourceWorkItem?.data?.id !== args.resourceId) invalidResponse(data, 'unexpected_resource');
        const related = part.relationships?.workItem?.data;
        if (!related || related.type !== 'workitems' || !validId(related.id, 2)) invalidResponse(data, 'relationship_missing');
        const item = included.get(related.id), available = item && !hasErrors(item);
        if (!available) incomplete++;
        const saved = snapshot(available ? item : { id: related.id, type: 'workitems' }, true);
        if (!available) saved.fields.details = 'Details not returned by Polarion; open this item to read them.';
        if (args.kind === 'elements' && Number.isInteger(part.attributes?.level)) saved.fields.level = String(part.attributes.level);
        if (args.kind === 'links') saved.fields.linkRole = enumValue(part.attributes?.role);
        elements.push(saved);
      }
      return reply({ code: 'ok', http: 200, elements, page_key: data.data.slice(0, 25).map(p => p.id).join('|'), received: data.data.length, incomplete, more: Boolean(data.links?.next) });
    }
    if (data.data.length > 25) invalidResponse(data, 'page_size_exceeded');
    if (data.data.some(item => item == null)) invalidResponse(data, 'unexpected_resource');
    const items = data.data.filter(item => item?.type === 'workitems' && validId(item.id, 2) && item.id.startsWith(args.project + '/') && !hasErrors(item)
      && (args.kind === 'containers' || enumValue(item.attributes?.type) === 'task'))
      .map(item => ({ id: item.id.split('/').pop(), title: text(item.attributes?.title || item.id), item_type: enumValue(item.attributes?.type), status: enumValue(item.attributes?.status) }));
    if (data.data.length && !items.length) invalidResponse(data, 'unexpected_resource');
    reply({ code: 'ok', http: 200, received: data.data.length, items, more: Boolean(data.links?.next) });
  } catch (error) {
    reply({ code: failureCode(error), http: error?.http || 0, catalog_pages: error?.catalog_pages || 0 });
  } finally { clearTimeout(timeout); }
})();
