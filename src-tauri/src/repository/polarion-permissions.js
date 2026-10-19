// Explicit, isolated diagnostic. Only WSDL GETs and the fixed read-only SOAP
// methods below are callable. Never login, create, update, delete or transact.
// Cookies remain in this portal; REST tokens are not SOAP credentials.
(async () => {
  const args = __DDT_ARGUMENTS__;
  const checks = [], origin = 'https://polarion.vitesco.io'; let attempts = 0;
  const reply = () => {
    const relay = new URL(origin + '/polarion/__ddt_search_result');
    relay.searchParams.set('request', args.request);
    relay.searchParams.set('data', JSON.stringify({ code: 'ok', permission_checks: checks, http_requests: attempts }));
    location.href = relay.href;
  };
  if (location.origin !== origin) {
    checks.push({ step: 'soap_session', code: 'wrong_origin', http: 0, duration_ms: 0, operations: [] }); return reply();
  }
  const abort = new AbortController(), timeout = setTimeout(() => abort.abort(), 25000);
  const SOAP = 'http://schemas.xmlsoap.org/soap/envelope/', XSD = 'http://www.w3.org/2001/XMLSchema';
  const methods = {
    security: ['canModifyInstance', 'canModifyKey', 'canReadInstance', 'hasCurrentUserPermission', 'canCreateInstances'],
    tracker: ['getModuleByLocation', 'getModuleByLocationWithFields', 'createWorkItemInModule', 'createWorkItem', 'updateModule', 'updateWorkItem', 'addLinkedItem'],
    session: ['hasSubject', 'logInWithToken'],
  };
  const services = { security: 'SecurityWebService', tracker: 'TrackerWebService', session: 'SessionWebService' };
  const callable = {
    hasSubject: ['session', []], getModuleByLocation: ['tracker', ['projectId', 'location']],
    canModifyInstance: ['security', ['uri']], canModifyKey: ['security', ['uri', 'key']],
    hasCurrentUserPermission: ['security', ['permission', 'projectId']],
  };
  const fault = code => Object.assign(new Error(code), { code });
  const descendants = (node, name, ns = '*') => [...node.getElementsByTagNameNS(ns, name)];
  const children = node => [...node.childNodes].filter(n => n.nodeType === 1);
  const local = (node, name) => children(node).filter(n => n.localName === name);
  const xml = value => String(value).replace(/[<>&"']/g, c => ({ '<':'&lt;', '>':'&gt;', '&':'&amp;', '"':'&quot;', "'":'&apos;' }[c]));
  function parse(raw) {
    if (/<!DOCTYPE|<!ENTITY/i.test(raw)) throw fault('invalid_response');
    const doc = new DOMParser().parseFromString(raw, 'application/xml');
    if (!doc.documentElement || descendants(doc, 'parsererror').length) throw fault('invalid_response');
    return doc;
  }
  // Bounded streaming, including chunked replies without Content-Length.
  async function body(response) {
    const limit = 2_000_000;
    if (Number(response.headers.get('content-length')) > limit) throw fault('response_limit');
    if (!response.body?.getReader) throw fault('invalid_response');
    const reader = response.body.getReader(), decoder = new TextDecoder(); let size = 0, raw = '';
    try {
      for (;;) {
        const part = await reader.read(); if (part.done) break;
        size += part.value.byteLength; if (size > limit) throw fault('response_limit');
        raw += decoder.decode(part.value, { stream: true });
      }
      return raw + decoder.decode();
    } catch (error) { try { await reader.cancel(); } catch {} throw error; }
    finally { reader.releaseLock(); }
  }
  async function request(step, service, payload = null) {
    const entry = { step, code: 'network_error', http: 0, duration_ms: 0, operations: [] }, started = Date.now(); checks.push(entry);
    try {
      if (abort.signal.aborted) throw fault('timeout');
      attempts++;
      const response = await fetch(origin + '/polarion/ws/services/' + services[service] + (payload === null ? '?wsdl' : ''), {
        method: payload === null ? 'GET' : 'POST', credentials: 'same-origin', redirect: 'manual', signal: abort.signal,
        headers: payload === null ? { Accept: 'application/xml,text/xml' } : { Accept: 'text/xml', 'Content-Type': 'text/xml; charset=utf-8', SOAPAction: '""' },
        ...(payload === null ? {} : { body: payload }),
      });
      entry.http = response.status;
      if (response.type === 'opaqueredirect' || [301,302,303,307,308,401].includes(response.status)) throw fault('auth_required');
      if (response.status === 403) throw fault('forbidden');
      if ([404,405].includes(response.status)) throw fault('api_unavailable');
      if (/text\/html/i.test(response.headers.get('content-type') || '')) throw fault('auth_required');
      const doc = parse(await body(response));
      const soapFault = descendants(doc, 'Fault', SOAP)[0];
      if (soapFault) {
        // Only a fixed category leaves the portal, never fault strings/stack traces.
        const text = soapFault.textContent.toLowerCase();
        throw fault(/not authorized|not authenticated|no subject|not logged|authentication|session.*(?:expired|invalid)/.test(text) ? 'soap_auth_required'
          : /permission|access denied|forbidden/.test(text) ? 'forbidden' : 'soap_fault');
      }
      if (!response.ok) throw fault('http_error');
      entry.code = 'ok'; return { doc, entry };
    } catch (error) {
      const allowed = ['auth_required','soap_auth_required','forbidden','api_unavailable','invalid_response','response_limit','timeout','http_error','soap_fault'];
      entry.code = error?.name === 'AbortError' ? 'timeout' : allowed.includes(error?.code) ? error.code : 'network_error';
      return { doc: null, entry };
    } finally { entry.duration_ms = Date.now() - started; }
  }
  function schema(doc, method) {
    const [service, params] = callable[method] || [];
    if (!service || !doc) return null;
    for (const type of descendants(doc, 'schema', XSD)) {
      const ns = type.getAttribute('targetNamespace');
      if (![services[service], services[service] + '-impl'].some(name => ns === 'http://ws.polarion.com/' + name)) continue;
      const element = children(type).find(n => n.localName === 'element' && n.getAttribute('name') === method);
      const names = element ? descendants(element, 'element', XSD).map(n => n.getAttribute('name')) : null;
      if (names && JSON.stringify(names) === JSON.stringify(params)) return { ns, qualified: type.getAttribute('elementFormDefault') === 'qualified' };
    }
    return null;
  }
  function resolved(doc, element) {
    const href = element?.getAttribute('href');
    if (!href) return element;
    if (!/^#[A-Za-z0-9_.-]{1,120}$/.test(href)) throw fault('invalid_response');
    const matches = descendants(doc, '*').filter(n => n.getAttribute('id') === href.slice(1));
    if (matches.length !== 1 || matches[0].getAttribute('href')) throw fault('invalid_response');
    return matches[0];
  }
  async function call(step, method, values, catalogs) {
    const [service, params] = callable[method], spec = schema(catalogs[service], method);
    if (!spec) { const entry = { step, code: 'unsupported_method', http: 0, duration_ms: 0, operations: [] }; checks.push(entry); return { entry }; }
    const prefix = spec.qualified ? 'p:' : '';
    const payload = '<s:Envelope xmlns:s="' + SOAP + '" xmlns:p="' + spec.ns + '"><s:Body><p:' + method + '>' + params.map(name => '<' + prefix + name + '>' + xml(values[name]) + '</' + prefix + name + '>').join('') + '</p:' + method + '></s:Body></s:Envelope>';
    const result = await request(step, service, payload); if (!result.doc) return result;
    try {
      const envelope = result.doc.documentElement;
      if (envelope.localName !== 'Envelope' || envelope.namespaceURI !== SOAP) throw fault('invalid_response');
      const bodies = local(envelope, 'Body').filter(n => n.namespaceURI === SOAP);
      if (bodies.length !== 1) throw fault('invalid_response');
      const responses = local(bodies[0], method + 'Response').filter(n => n.namespaceURI === spec.ns);
      if (responses.length !== 1) throw fault('invalid_response');
      const returns = local(responses[0], method + 'Return');
      if (returns.length !== 1) throw fault('invalid_response');
      result.value = resolved(result.doc, returns[0]);
    } catch { result.entry.code = 'invalid_response'; }
    return result;
  }
  async function boolean(step, method, values, catalogs) {
    const result = await call(step, method, values, catalogs);
    if (result.entry.code === 'ok') {
      const value = result.value?.textContent.trim();
      if (!result.value || children(result.value).length || !['true','false','1','0'].includes(value)) result.entry.code = 'invalid_response';
      else result.entry.allowed = value === 'true' || value === '1';
    }
    return result.entry;
  }
  const skip = step => checks.push({ step, code: 'not_run', http: 0, duration_ms: 0, operations: [] });
  try {
    const catalogs = {};
    for (const service of ['security', 'tracker', 'session']) {
      const result = await request(service + '_wsdl', service);
      if (!result.doc) continue;
      const root = result.doc.documentElement;
      if (root.localName !== 'definitions' || root.namespaceURI !== 'http://schemas.xmlsoap.org/wsdl/') { result.entry.code = 'invalid_response'; continue; }
      result.entry.operations = [...new Set(descendants(result.doc, 'operation', 'http://schemas.xmlsoap.org/wsdl/').map(n => n.getAttribute('name')).filter(name => methods[service].includes(name)))];
      catalogs[service] = result.doc;
    }
    const subject = await boolean('soap_session', 'hasSubject', {}, catalogs);
    if (subject.allowed !== true) {
      for (const step of ['document_resolve','document_edit','document_text','project_document_content']) skip(step);
      return;
    }
    const [projectId, space, name] = args.resourceId.split('/');
    const location = (space === '_default' ? [name] : [space, name]).map(encodeURIComponent).join('/');
    const document = await call('document_resolve', 'getModuleByLocation', { projectId, location }, catalogs);
    let uri = '';
    if (document.entry.code === 'ok') {
      const field = key => { const nodes = local(document.value, key); return nodes.length === 1 ? resolved(document.doc, nodes[0])?.textContent.trim() : ''; };
      const folder = field('moduleFolder') || '_default';
      const projects = local(document.value, 'project');
      const project = projects.length === 1 ? resolved(document.doc, projects[0]) : null;
      const projectIds = project ? local(project, 'id') : [];
      uri = field('uri');
      if (field('unresolvable') === 'true' || field('moduleName') !== name || folder !== space || projectIds.length !== 1 || projectIds[0].textContent.trim() !== projectId || !/^subterra:data-service:objects:[^\x00-\x1f]{1,2000}$/.test(uri)) {
        uri = ''; document.entry.code = 'document_identity_unverified';
      }
    }
    if (!uri) { for (const step of ['document_edit','document_text','project_document_content']) skip(step); return; }
    await boolean('document_edit', 'canModifyInstance', { uri }, catalogs);
    await boolean('document_text', 'canModifyKey', { uri, key: 'homePageContent' }, catalogs);
    await boolean('project_document_content', 'hasCurrentUserPermission', { permission: 'com.polarion.persistence.object.Module.modifyContent', projectId }, catalogs);
  } catch { checks.push({ step: 'document_edit', code: abort.signal.aborted ? 'timeout' : 'invalid_response', http: 0, duration_ms: 0, operations: [] }); }
  finally { clearTimeout(timeout); reply(); }
})();
