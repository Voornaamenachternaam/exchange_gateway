// worker/index.js
const FORWARDED_PATH_PREFIXES = ['/ews', '/microsoft-server-activesync'];
const DEFAULT_MAX_BODY_BYTES = 1024 * 1024;
const HOP_BY_HOP_HEADERS = [
  'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization',
  'te', 'trailer', 'transfer-encoding', 'upgrade'
];

const API_METHODS = {
  '/api/set_sync_key': 'POST',
  '/api/get_sync_key': 'GET',
  '/api/get_client_sync_command': 'GET',
  '/api/upsert_item_map': 'POST',
  '/api/put_client_sync_command': 'POST',
  '/api/delete_item_by_server_id': 'POST',
  '/api/add_delete_tombstone': 'POST',
  '/api/list_changes_since': 'GET',
  '/api/list_deleted_since': 'GET',
  '/api/get_latest_change_seq': 'GET',
  '/api/list_changes_since_seq': 'GET',
  '/api/list_deleted_since_seq': 'GET',
  '/api/list_journal_since_seq': 'GET',
  '/api/set_provision_policy': 'POST',
  '/api/get_provision_policy': 'GET',
  '/api/list_ews_items': 'GET',
  '/api/get_ews_sync_state': 'GET',
  '/api/set_ews_sync_state': 'POST',
  '/api/get_ews_item_by_id': 'GET',
  '/api/get_ews_item_owner': 'GET',
  '/api/upsert_device_info': 'POST',
  '/api/upsert_calendar_exception': 'POST',
  '/api/get_calendar_exceptions': 'GET',
  '/api/get_calendar_exception': 'GET',
  '/api/delete_calendar_exception': 'POST',
  '/api/record_meeting_response': 'POST',
  '/api/get_meeting_response': 'GET',
  '/api/get_calendar_permission': 'GET',
  '/api/get_calendar_permissions': 'GET',
  '/api/get_user_calendar_permissions': 'GET',
  '/api/get_default_calendar_permission': 'GET',
  '/api/get_anonymous_calendar_permission': 'GET',
  '/api/upsert_calendar_permission': 'POST',
  '/api/delete_calendar_permission': 'POST',
  '/api/get_delegate': 'GET',
  '/api/get_delegates': 'GET',
  '/api/upsert_delegate': 'POST',
  '/api/delete_delegate': 'POST',
  '/api/add_permission_audit': 'POST',
  '/api/get_permission_audit_log': 'GET',
  '/api/upsert_calendar_attachment': 'POST',
  '/api/get_calendar_attachment': 'GET',
  '/api/get_calendar_attachments_for_item': 'GET',
  '/api/delete_calendar_attachment': 'POST',
  '/api/upsert_room_list': 'POST',
  '/api/get_room_lists': 'GET',
  '/api/delete_room_list': 'POST',
  '/api/upsert_room': 'POST',
  '/api/get_rooms_for_list': 'GET',
  '/api/get_all_rooms': 'GET',
  '/api/delete_room': 'POST'
};

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname.toLowerCase();

    if (request.method === 'OPTIONS') {
      return corsPreflightResponse();
    }

    if (isForwardedPath(path)) {
      return handleGatewayForward(request, env, ctx);
    }

    const methodValidation = validateApiMethod(path, request.method);
    if (!methodValidation.allowed) {
      return new Response('Method Not Allowed', { status: 405, headers: { Allow: methodValidation.allow } });
    }


    if (path === '/api/set_sync_key') return handleSetSyncKey(request, env);
    if (path === '/api/get_sync_key') return handleGetSyncKey(url, request, env);
    if (path === '/api/get_client_sync_command') return handleGetClientSyncCommand(url, request, env);
    if (path === '/api/upsert_item_map') return handleUpsertItemMap(request, env);
    if (path === '/api/put_client_sync_command') return handlePutClientSyncCommand(request, env);
    if (path === '/api/delete_item_by_server_id') return handleDeleteItemByServerId(request, env);
    if (path === '/api/add_delete_tombstone') return handleAddDeleteTombstone(request, env);
    if (path === '/api/list_changes_since') return handleListChangesSince(url, request, env);
    if (path === '/api/list_deleted_since') return handleListDeletedSince(url, request, env);
    if (path === '/api/get_latest_change_seq') return handleGetLatestChangeSeq(request, env);
    if (path === '/api/list_changes_since_seq') return handleListChangesSinceSeq(url, request, env);
    if (path === '/api/list_deleted_since_seq') return handleListDeletedSinceSeq(url, request, env);
    if (path === '/api/list_journal_since_seq') return handleListJournalSinceSeq(url, request, env);
    if (path === '/api/set_provision_policy') return handleSetProvisionPolicy(request, env);
    if (path === '/api/get_provision_policy') return handleGetProvisionPolicy(url, request, env);
    if (path === '/api/list_ews_items') return handleListEwsItems(url, request, env);
    if (path === '/api/get_ews_sync_state') return handleGetEwsSyncState(url, request, env);
    if (path === '/api/set_ews_sync_state') return handleSetEwsSyncState(request, env);
    if (path === '/api/get_ews_item_by_id') return handleGetEwsItemById(url, request, env);
  if (path === '/api/get_ews_item_owner') return handleGetEwsItemOwner(url, request, env);
    if (path === '/api/upsert_device_info') return handleUpsertDeviceInfo(request, env);
    if (path === '/api/upsert_calendar_exception') return handleUpsertCalendarException(request, env);
    if (path === '/api/get_calendar_exceptions') return handleGetCalendarExceptions(url, request, env);
    if (path === '/api/get_calendar_exception') return handleGetCalendarException(url, request, env);
    if (path === '/api/delete_calendar_exception') return handleDeleteCalendarException(request, env);
    if (path === '/api/record_meeting_response') return handleRecordMeetingResponse(request, env);
    if (path === '/api/get_meeting_response') return handleGetMeetingResponse(url, request, env);
if (path === '/api/get_calendar_permission') return handleGetCalendarPermission(url, request, env);
if (path === '/api/get_calendar_permissions') return handleGetCalendarPermissions(url, request, env);
if (path === '/api/get_user_calendar_permissions') return handleGetUserCalendarPermissions(url, request, env);
if (path === '/api/get_default_calendar_permission') return handleGetDefaultCalendarPermission(url, request, env);
if (path === '/api/get_anonymous_calendar_permission') return handleGetAnonymousCalendarPermission(url, request, env);
if (path === '/api/upsert_calendar_permission') return handleUpsertCalendarPermission(request, env);
if (path === '/api/delete_calendar_permission') return handleDeleteCalendarPermission(request, env);
if (path === '/api/get_delegate') return handleGetDelegate(url, request, env);
if (path === '/api/get_delegates') return handleGetDelegates(url, request, env);
if (path === '/api/upsert_delegate') return handleUpsertDelegate(request, env);
if (path === '/api/delete_delegate') return handleDeleteDelegate(request, env);
if (path === '/api/add_permission_audit') return handleAddPermissionAudit(request, env);
if (path === '/api/get_permission_audit_log') return handleGetPermissionAuditLog(url, request, env);

    if (path.startsWith('/api/')) {
      return withCors(await handleApiRequest(request, env));
    }

    if (
      path.includes('/autodiscover/') ||
      path.endsWith('/autodiscover.xml') ||
      path.endsWith('/autodiscover.svc') ||
      path.includes('/autodiscover.json')
    ) {
      if (path.includes('.json')) return handleAutodiscoverJson(env, request);
      if (path.endsWith('.svc')) return handleAutodiscoverSoap(request, env);
      return handleAutodiscoverXml(request, env);
    }

    return new Response('Not Found', { status: 404 });
  },

  async scheduled(event, env, ctx) {
    ctx.waitUntil(cleanupIdempotencyKeys(env));
  }
};

function isForwardedPath(path) {
  return FORWARDED_PATH_PREFIXES.some((prefix) => path.startsWith(prefix));
}

function validateApiMethod(path, methodRaw) {
  const expected = API_METHODS[path];
  if (!expected) return { allowed: true, allow: 'GET, POST, OPTIONS' };
  const method = (methodRaw || 'GET').toUpperCase();
  if (method === expected || method === 'OPTIONS') return { allowed: true, allow: `${expected}, OPTIONS` };
  return { allowed: false, allow: `${expected}, OPTIONS` };
}

function isAuthorized(request, env) {
  const bearer = request.headers.get('Authorization');
  const xSecret = request.headers.get('x-gateway-secret');
  const secret = env.GATEWAY_SECRET;
  if (typeof secret !== 'string' || secret.length === 0) return false;
  const expectedBearer = `Bearer ${secret}`;
  return subtleEqual(bearer ?? '', expectedBearer) || subtleEqual(xSecret ?? '', secret);
}

function subtleEqual(a, b) {
  if (typeof a !== 'string' || typeof b !== 'string') return false;
  if (a.length !== b.length) return false;
  let mismatch = 0;
  for (let i = 0; i < a.length; i += 1) mismatch |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return mismatch === 0;
}

const CORS_HEADERS = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Methods': 'GET, POST, OPTIONS, PUT, DELETE',
  'Access-Control-Allow-Headers': 'Content-Type, Authorization, X-Gateway-Secret, Idempotency-Key',
  'Access-Control-Max-Age': '86400',
};

function withCors(response) {
  const headers = new Headers(response.headers);
  for (const [key, value] of Object.entries(CORS_HEADERS)) headers.set(key, value);
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

function corsPreflightResponse() {
  return new Response(null, { status: 204, headers: CORS_HEADERS });
}

async function readJson(request) {
  try { return await request.json(); }
  catch { throw new Error('Invalid JSON'); }
}

async function checkIdempotency(request, env, routeName) {
  const key = request.headers.get('Idempotency-Key');
  if (!key) return;
  if (key.length > 128 || !/^[A-Za-z0-9._:-]+$/.test(key)) return;
  await env.EXCHANGE_DB
    .prepare('INSERT INTO api_idempotency (idempotency_key, route_name, created_at) VALUES (?, ?, CURRENT_TIMESTAMP) ON CONFLICT(idempotency_key) DO NOTHING')
    .bind(key, routeName)
    .run();
}

async function handleGatewayForward(request, env, ctx) {
  if (!env.ORIGIN_BASE_URL) {
    return new Response('Worker misconfigured: ORIGIN_BASE_URL is required', { status: 500 });
  }
  const method = (request.method || 'GET').toUpperCase();
  if (method === 'OPTIONS') {
    return corsPreflightResponse();
  }

  if (!['OPTIONS', 'POST'].includes(method)) {
    return new Response('Method Not Allowed', { status: 405, headers: { Allow: 'OPTIONS, POST' } });
  }
  if (!isValidOriginBaseUrl(env.ORIGIN_BASE_URL)) {
    return new Response('Worker misconfigured: ORIGIN_BASE_URL must be http/https URL', { status: 500 });
  }
  const contentLength = Number.parseInt(request.headers.get('content-length') || '0', 10);
  const maxForwardBodyBytes = Math.max(1024, Number.parseInt(env.MAX_FORWARD_BODY_BYTES || `${DEFAULT_MAX_BODY_BYTES}`, 10) || DEFAULT_MAX_BODY_BYTES);
  if (Number.isFinite(contentLength) && contentLength > maxForwardBodyBytes) {
    return new Response('Payload too large', { status: 413 });
  }
  const rateLimitResult = await enforceRateLimit(request, env, ctx);
  if (!rateLimitResult.allowed) {
    return new Response('Too Many Requests', {
      status: 429,
      headers: { 'Retry-After': String(rateLimitResult.retryAfterSec), 'Cache-Control': 'private, no-store' }
    });
  }
  const incoming = new URL(request.url);
  const upstream = new URL(incoming.pathname + incoming.search, env.ORIGIN_BASE_URL);
  const forwardedHeaders = sanitizeForwardHeaders(request.headers);
  forwardedHeaders.set('X-Forwarded-Proto', 'https');
  forwardedHeaders.set('X-Forwarded-Host', incoming.host);
  forwardedHeaders.set('X-Forwarded-For', request.headers.get('CF-Connecting-IP') || '');
  const upstreamRequest = new Request(upstream.toString(), {
    method,
    headers: forwardedHeaders,
    body: method === 'POST' ? request.body : undefined,
    redirect: 'manual'
  });
  try {
    const upstreamResponse = await fetch(upstreamRequest);
    return withCommonSecurityHeaders(upstreamResponse);
  } catch {
    return new Response('Bad Gateway', { status: 502, headers: { 'Cache-Control': 'private, no-store' } });
  }
}

function sanitizeForwardHeaders(inputHeaders) {
  const headers = new Headers(inputHeaders);
  for (const h of HOP_BY_HOP_HEADERS) headers.delete(h);
  return headers;
}

function isValidOriginBaseUrl(raw) {
  try {
    const u = new URL(raw);
    return u.protocol === 'https:' || u.protocol === 'http:';
  } catch { return false; }
}

async function enforceRateLimit(request, env, ctx) {
  const enabled = String(env.RATE_LIMIT_ENABLED || 'true').toLowerCase() === 'true';
  if (!enabled) return { allowed: true, retryAfterSec: 0 };
  if (!env.RATE_LIMIT_KV) return { allowed: true, retryAfterSec: 0 };
  const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
  const path = new URL(request.url).pathname.toLowerCase();
  const key = `rl:${ip}:${path}`;
  const maxRaw = Number.parseInt(env.RATE_LIMIT_MAX || '120', 10);
  const windowRaw = Number.parseInt(env.RATE_LIMIT_WINDOW_SEC || '60', 10);
  const max = Math.max(1, Number.isFinite(maxRaw) ? maxRaw : 120);
  const windowSec = Math.max(10, Number.isFinite(windowRaw) ? windowRaw : 60);
  const now = Math.floor(Date.now() / 1000);
  const currentRaw = await env.RATE_LIMIT_KV.get(key);
  let current;
  try { current = currentRaw ? JSON.parse(currentRaw) : { count: 0, resetAt: now + windowSec }; }
  catch { current = { count: 0, resetAt: now + windowSec }; }
  if (now >= current.resetAt) { current.count = 0; current.resetAt = now + windowSec; }
  current.count += 1;
  const ttl = Math.max(1, current.resetAt - now);
  ctx.waitUntil(env.RATE_LIMIT_KV.put(key, JSON.stringify(current), { expirationTtl: ttl }));
  return { allowed: current.count <= max, retryAfterSec: Math.max(1, current.resetAt - now) };
}

function withCommonSecurityHeaders(response) {
  const headers = new Headers(response.headers);
  headers.set('X-Content-Type-Options', 'nosniff');
  headers.set('Referrer-Policy', 'no-referrer');
  headers.set('X-Frame-Options', 'DENY');
  headers.set('Permissions-Policy', 'geolocation=(), microphone=(), camera=()');
  headers.set('Cache-Control', headers.get('Cache-Control') || 'private, no-store');
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

async function handleSetSyncKey(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleSetSyncKey');
  const body = await readJson(request);
  const { owner = '', collection_id = '', sync_key = '', token = null } = body;
  if (!owner || !collection_id || !sync_key) return new Response('Missing owner/collection_id/sync_key', { status: 400 });
  await env.EXCHANGE_DB
    .prepare(`INSERT INTO sync_state (owner, collection_id, sync_key, token, updated_at)
              VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
              ON CONFLICT(owner, collection_id)
              DO UPDATE SET sync_key = excluded.sync_key, token = excluded.token, updated_at = CURRENT_TIMESTAMP`)
    .bind(owner, collection_id, sync_key, token)
    .run();
  return Response.json({ success: true });
}

async function handleGetSyncKey(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const collectionId = url.searchParams.get('collection_id') || '';
  if (!owner || !collectionId) return new Response('Missing owner/collection_id', { status: 400 });
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT sync_key, token FROM sync_state WHERE owner = ? AND collection_id = ? LIMIT 1`)
    .bind(owner, collectionId)
    .all();
  return Response.json((result.results || [])[0] || null);
}

async function handleGetClientSyncCommand(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const collectionId = url.searchParams.get('collection_id') || '';
  const clientId = url.searchParams.get('client_id') || '';
  if (!owner || !collectionId || !clientId) return new Response('Missing owner/collection_id/client_id', { status: 400 });
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT server_id, status FROM client_sync_command WHERE owner = ? AND collection_id = ? AND client_id = ? LIMIT 1`)
    .bind(owner, collectionId, clientId)
    .all();
  return Response.json((result.results || [])[0] || null);
}

async function recordChangeJournal(env, owner, serverId, op, resourceHref) {
  await env.EXCHANGE_DB
    .prepare(`INSERT INTO change_journal (owner, server_id, op, resource_href, created_at) VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)`)
    .bind(owner, serverId, op, resourceHref || null)
    .run();
}

async function handleUpsertItemMap(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleUpsertItemMap');
  const body = await readJson(request);
  const { owner = '', caldav_href = '', resource_href = '', server_id = '', uid = '', etag = '' } = body;
  if (!owner || !resource_href || !server_id) return new Response('Missing owner/resource_href/server_id', { status: 400 });
  await env.EXCHANGE_DB
    .prepare(`INSERT INTO item_map (owner, caldav_href, resource_href, server_id, uid, etag, updated_at)
              VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
              ON CONFLICT(owner, server_id)
              DO UPDATE SET caldav_href = excluded.caldav_href, resource_href = excluded.resource_href,
                            uid = excluded.uid, etag = excluded.etag, updated_at = CURRENT_TIMESTAMP`)
    .bind(owner, caldav_href, resource_href, server_id, uid, etag)
    .run();
  await env.EXCHANGE_DB
    .prepare('DELETE FROM deleted_item_tombstone WHERE owner = ? AND server_id = ?')
    .bind(owner, server_id)
    .run();
  await recordChangeJournal(env, owner, server_id, 'upsert', resource_href);
  return Response.json({ success: true });
}

async function handlePutClientSyncCommand(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handlePutClientSyncCommand');
  const body = await readJson(request);
  const { owner = '', collection_id = '', client_id = '', server_id = null, status = '' } = body;
  if (!owner || !collection_id || !client_id || !status) return new Response('Missing owner/collection_id/client_id/status', { status: 400 });
  await env.EXCHANGE_DB
    .prepare(`INSERT INTO client_sync_command (owner, collection_id, client_id, server_id, status, created_at, updated_at)
              VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
              ON CONFLICT(owner, collection_id, client_id)
              DO UPDATE SET server_id = excluded.server_id, status = excluded.status, updated_at = CURRENT_TIMESTAMP`)
    .bind(owner, collection_id, client_id, server_id, status)
    .run();
  return Response.json({ success: true });
}

async function handleDeleteItemByServerId(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleDeleteItemByServerId');
  const body = await readJson(request);
  const { owner = '', server_id = '' } = body;
  if (!owner || !server_id) return new Response('Missing owner/server_id', { status: 400 });
  await env.EXCHANGE_DB.prepare('DELETE FROM item_map WHERE owner = ? AND server_id = ?').bind(owner, server_id).run();
  return Response.json({ success: true });
}

async function handleAddDeleteTombstone(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleAddDeleteTombstone');
  const body = await readJson(request);
  const { owner = '', server_id = '' } = body;
  if (!owner || !server_id) return new Response('Missing owner/server_id', { status: 400 });
  await env.EXCHANGE_DB
    .prepare(`INSERT INTO deleted_item_tombstone (owner, server_id, deleted_at)
              VALUES (?, ?, CURRENT_TIMESTAMP)
              ON CONFLICT(owner, server_id) DO UPDATE SET deleted_at = CURRENT_TIMESTAMP`)
    .bind(owner, server_id)
    .run();
  await recordChangeJournal(env, owner, server_id, 'delete', null);
  return Response.json({ success: true });
}

async function handleListChangesSince(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const since = url.searchParams.get('since') || '0';
  if (!owner) return new Response('Missing owner', { status: 400 });
  const sinceExpr = Number.isFinite(Number(since)) ? Number(since) : 0;
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT server_id, resource_href FROM item_map
              WHERE owner = ? AND strftime('%s', updated_at) >= ? ORDER BY updated_at ASC`)
    .bind(owner, sinceExpr)
    .all();
  return Response.json(result.results || []);
}

async function handleListDeletedSince(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const since = url.searchParams.get('since') || '0';
  if (!owner) return new Response('Missing owner', { status: 400 });
  const sinceExpr = Number.isFinite(Number(since)) ? Number(since) : 0;
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT server_id FROM deleted_item_tombstone
              WHERE owner = ? AND strftime('%s', deleted_at) >= ? ORDER BY deleted_at ASC`)
    .bind(owner, sinceExpr)
    .all();
  return Response.json(result.results || []);
}

async function handleApiRequest(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const sqlApiEnabled = String(env.SQL_API_ENABLED || 'false').toLowerCase() === 'true';
  if (!sqlApiEnabled) return new Response('SQL API disabled', { status: 403 });
  const contentLength = Number.parseInt(request.headers.get('content-length') || '0', 10);
  const maxBodyBytes = Math.max(1024, Number.parseInt(env.MAX_API_BODY_BYTES || `${DEFAULT_MAX_BODY_BYTES}`, 10) || DEFAULT_MAX_BODY_BYTES);
  if (Number.isFinite(contentLength) && contentLength > maxBodyBytes) return new Response('Payload too large', { status: 413 });
  let body;
  try { body = await request.json(); } catch { return new Response('Invalid JSON', { status: 400 }); }
  const { query, params } = body;
  if (!query || typeof query !== 'string') return new Response("Missing 'query' field", { status: 400 });
  if (!query.trim().toUpperCase().startsWith('SELECT')) return new Response('Only SELECT queries are permitted', { status: 403 });
  try {
    let stmt = env.EXCHANGE_DB.prepare(query);
    if (params && Array.isArray(params)) stmt = stmt.bind(...params);
    const result = await stmt.all();
    return Response.json({ success: result.success ?? true, errors: result.success === false ? [{ message: result.errors?.[0]?.message ?? 'DB query failed' }] : [], result: [{ results: result.results, meta: result.meta }] });
  } catch (e) {
    console.error('D1 Error:', e);
    return Response.json({ error: { message: e.message } }, { status: 500 });
  }
}

async function handleAutodiscoverJson(env, request) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response('Config Error', { status: 500 });
  const url = new URL(request.url);
  const protocol = (url.searchParams.get('Protocol') || url.searchParams.get('protocol') || 'Exchange').toLowerCase();
  const ewsUrl = `https://${domain}/EWS/Exchange.asmx`;
  const asUrl = `https://${domain}/Microsoft-Server-ActiveSync`;
  const v1Url = `https://${domain}/autodiscover/autodiscover.xml`;
  let payload;
  if (protocol === 'activesync') {
    payload = { Protocol: 'ActiveSync', Url: asUrl, ActiveSyncUrl: asUrl, MobileSyncUrl: asUrl };
  } else if (protocol === 'ews') {
    payload = { Protocol: 'Ews', Url: ewsUrl, EwsUrl: ewsUrl, ExternalEwsUrl: ewsUrl, InternalEwsUrl: ewsUrl };
  } else if (protocol === 'autodiscoverv1') {
    payload = { Protocol: 'AutodiscoverV1', Url: v1Url };
  } else {
    payload = { Protocol: 'Exchange', Url: ewsUrl, EwsUrl: ewsUrl, ExternalEwsUrl: ewsUrl, InternalEwsUrl: ewsUrl, ActiveSyncUrl: asUrl, MobileSyncUrl: asUrl, ExternalEwsVersion: 'Exchange2016', EwsSupportedSchemas: 'Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016' };
  }
  return new Response(JSON.stringify(payload), { headers: privateNoStoreHeaders({ 'Content-Type': 'application/json' }) });
}

async function handleAutodiscoverXml(request, env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response('Config Error', { status: 500 });
  let email = '';
  try { const body = await request.text(); const match = body.match(/<EMailAddress>(.*?)<\/EMailAddress>/i); if (match) email = match[1]; } catch {}
  const xml = `<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <User><DisplayName>Stalwart Mail</DisplayName><EMailAddress>${escapeXml(email)}</EMailAddress><DeploymentId>00000000-0000-0000-0000-000000000000</DeploymentId></User>
    <Account>
      <AccountType>email</AccountType><Action>settings</Action>
      <Protocol>
        <Type>EXCH</Type><Server>${domain}</Server>
        <ServerDN>/o=Exchange/ou=Exchange Administrative Group/cn=Recipients/cn=user</ServerDN>
        <ServerVersion>15.20.0.0</ServerVersion><MdbDN />
        <ASUrl>https://${domain}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://${domain}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://${domain}/EWS/Exchange.asmx</EmwsUrl>
        <EcpUrl>https://${domain}/EWS/Exchange.asmx</EcpUrl>
        <OABUrl>https://${domain}/EWS/Exchange.asmx</OABUrl>
        <OOFUrl>https://${domain}/EWS/Exchange.asmx</OOFUrl>
        <UMUrl>https://${domain}/EWS/Exchange.asmx</UMUrl>
        <EwsPartnerUrl>https://${domain}/EWS/Exchange.asmx</EwsPartnerUrl>
        <LoginName>${escapeXml(email)}</LoginName>
        <DomainRequired>off</DomainRequired><SPA>off</SPA>
        <AuthPackage>Basic</AuthPackage><CertPrincipalName>None</CertPrincipalName>
        <SSL>on</SSL><AuthRequired>on</AuthRequired>
      </Protocol>
      <Protocol>
        <Type>EXPR</Type><Server>${domain}</Server><SSL>on</SSL><SPA>off</SPA>
        <CertPrincipalName>None</CertPrincipalName><AuthPackage>Basic</AuthPackage>
        <LoginName>${escapeXml(email)}</LoginName><ServerExclusiveConnect>off</ServerExclusiveConnect>
        <TTL>1</TTL>
        <ASUrl>https://${domain}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://${domain}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://${domain}/EWS/Exchange.asmx</EmwsUrl>
        <EcpUrl>https://${domain}/EWS/Exchange.asmx</EcpUrl>
        <OABUrl>https://${domain}/EWS/Exchange.asmx</OABUrl>
        <OOFUrl>https://${domain}/EWS/Exchange.asmx</OOFUrl>
        <EwsPartnerUrl>https://${domain}/EWS/Exchange.asmx</EwsPartnerUrl>
      </Protocol>
      <Protocol>
        <Type>MobileSync</Type><Server>${domain}</Server><Name>Exchange Gateway</Name>
        <Url>https://${domain}/Microsoft-Server-ActiveSync</Url>
        <LoginName>${escapeXml(email)}</LoginName>
        <DomainRequired>off</DomainRequired><SSL>on</SSL><AuthPackage>Basic</AuthPackage>
        <ASUrl>https://${domain}/Microsoft-Server-ActiveSync</ASUrl>
      </Protocol>
    </Account>
  </Response>
</Autodiscover>`;
  return new Response(xml, { headers: privateNoStoreHeaders({ 'Content-Type': 'application/xml; charset=utf-8' }) });
}

async function handleAutodiscoverSoap(request, env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response('Config Error', { status: 500 });
  const body = await request.text();
  const emailMatch = body.match(/<a:EMailAddress>(.*?)<\/a:EMailAddress>/i);
  const email = emailMatch ? emailMatch[1] : '';
  const xml = `<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Header><a:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" /></s:Header>
  <s:Body>
    <a:GetUserSettingsResponseMessage>
      <a:Response>
        <a:ErrorCode>NoError</a:ErrorCode><a:ErrorMessage /><a:UserResponses>
          <a:UserResponse>
            <a:ErrorCode>NoError</a:ErrorCode><a:ErrorMessage /><a:RedirectTarget /><a:UserSettingErrors />
            <a:UserSettings>
              <a:UserSetting><a:Name>UserDisplayName</a:Name><a:Value>Stalwart Mail</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>UserDN</a:Name><a:Value>${escapeXml(email)}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>AutoDiscoverSMTPAddress</a:Name><a:Value>${escapeXml(email)}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalRpcClientServer</a:Name><a:Value>${domain}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEmwsUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEmwsUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEcpUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEcpUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalOABUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalOABUrl</a:Name><a:Value>https://${domain}/EWS/Exchange.asmx</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>MobileSyncServer</a:Name><a:Value>${domain}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalMobileSyncUrl</a:Name><a:Value>https://${domain}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalMobileSyncUrl</a:Name><a:Value>https://${domain}/Microsoft-Server-ActiveSync</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ExternalEwsVersion</a:Name><a:Value>Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>InternalEwsVersion</a:Name><a:Value>Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>EwsSupportedSchemas</a:Name><a:Value>Exchange2007,Exchange2007_SP1,Exchange2010,Exchange2010_SP1,Exchange2010_SP2,Exchange2013,Exchange2013_SP1,Exchange2016</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>PublicFolderServer</a:Name><a:Value>${domain}</a:Value></a:UserSetting>
              <a:UserSetting><a:Name>ActiveDirectoryServer</a:Name><a:Value>${domain}</a:Value></a:UserSetting>
            </a:UserSettings>
          </a:UserResponse>
        </a:UserResponses>
      </a:Response>
    </a:GetUserSettingsResponseMessage>
  </s:Body>
</s:Envelope>`;
  return new Response(xml, { headers: privateNoStoreHeaders({ 'Content-Type': 'application/soap+xml; charset=utf-8' }) });
}

function mergeHeaders(...sets) {
  const headers = new Headers();
  for (const set of sets) {
    if (!set) continue;
    const source = set instanceof Headers ? set : new Headers(set);
    source.forEach((value, key) => headers.set(key, value));
  }
  return headers;
}

function privateNoStoreHeaders(extra = {}) {
  return mergeHeaders({ 'Cache-Control': 'private, no-store', 'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer', 'X-Frame-Options': 'DENY' }, extra);
}

function escapeXml(unsafe = '') {
  return String(unsafe).replace(/[<>&'"]/g, (c) => {
    switch (c) { case '<': return '&lt;'; case '>': return '&gt;'; case '&': return '&amp;'; case '\'': return '&apos;'; case '"': return '&quot;'; default: return c; }
  });
}

async function handleSetProvisionPolicy(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleSetProvisionPolicy');
  const body = await readJson(request);
  const { owner = '', device_id = '', policy_key = '', policy_status = '' } = body;
  if (!owner || !device_id || !policy_key || !policy_status) return new Response('Missing owner/device_id/policy_key/policy_status', { status: 400 });
  await env.EXCHANGE_DB
    .prepare(`INSERT INTO provision_state (owner, device_id, policy_key, policy_status, updated_at)
              VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
              ON CONFLICT(owner, device_id)
              DO UPDATE SET policy_key = excluded.policy_key, policy_status = excluded.policy_status, updated_at = CURRENT_TIMESTAMP`)
    .bind(owner, device_id, policy_key, policy_status)
    .run();
  return Response.json({ success: true });
}

async function handleGetProvisionPolicy(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const deviceId = url.searchParams.get('device_id') || '';
  if (!owner || !deviceId) return new Response('Missing owner/device_id', { status: 400 });
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT policy_key, policy_status FROM provision_state WHERE owner = ? AND device_id = ? LIMIT 1`)
    .bind(owner, deviceId)
    .all();
  return Response.json((result.results || [])[0] || null);
}

async function handleUpsertDeviceInfo(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleUpsertDeviceInfo');
  try {
    const body = await readJson(request);
    const {
      owner = '',
      device_id = '',
      friendly_name = null,
      model = null,
      os = null,
      os_version = null,
      phone_number = null,
      imei = null,
      user_agent = null,
      protocol_version = '16.1',
    } = body;
    if (!owner || !device_id) {
      return new Response('Missing owner/device_id', { status: 400 });
    }

    await env.EXCHANGE_DB
      .prepare(`INSERT INTO device_info (user_email, device_id, friendly_name, model, os, os_version, phone_number, imei, user_agent, protocol_version, last_seen) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ON CONFLICT(user_email, device_id) DO UPDATE SET friendly_name = excluded.friendly_name, model = excluded.model, os = excluded.os, os_version = excluded.os_version, phone_number = excluded.phone_number, imei = excluded.imei, user_agent = excluded.user_agent, protocol_version = excluded.protocol_version, last_seen = CURRENT_TIMESTAMP`)
      .bind(owner, device_id, friendly_name, model, os, os_version,
        phone_number, imei, user_agent, protocol_version)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error upserting device info:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleListEwsItems(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const limit = Number(url.searchParams.get('limit') || '50');
  const offset = Number(url.searchParams.get('offset') || '0');
  if (!owner) return new Response('Missing owner', { status: 400 });
  const safeLimit = Math.max(1, Math.min(512, Number.isFinite(limit) ? limit : 50));
  const safeOffset = Math.max(0, Number.isFinite(offset) ? offset : 0);
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT server_id, resource_href, uid, etag, updated_at FROM item_map
              WHERE owner = ? ORDER BY updated_at DESC, server_id ASC LIMIT ? OFFSET ?`)
    .bind(owner, safeLimit, safeOffset)
    .all();
  return Response.json(result.results || []);
}

async function handleGetEwsSyncState(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const folderId = url.searchParams.get('folder_id') || '';
  if (!owner || !folderId) return new Response('Missing owner/folder_id', { status: 400 });
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT sync_state FROM ews_sync_state WHERE user_email = ? AND folder_id = ? LIMIT 1`)
    .bind(owner, folderId)
    .all();
  return Response.json((result.results || [])[0] || null);
}

async function handleSetEwsSyncState(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleSetEwsSyncState');
  const body = await readJson(request);
  const { owner = '', folder_id = '', sync_state = '' } = body;
  if (!owner || !folder_id || !sync_state) return new Response('Missing owner/folder_id/sync_state', { status: 400 });
  await env.EXCHANGE_DB
    .prepare(`INSERT INTO ews_sync_state (user_email, folder_id, sync_state, created_at)
              VALUES (?, ?, ?, CURRENT_TIMESTAMP)
              ON CONFLICT(user_email, folder_id) DO UPDATE SET sync_state = excluded.sync_state, created_at = CURRENT_TIMESTAMP`)
    .bind(owner, folder_id, sync_state)
    .run();
  return Response.json({ success: true });
}

async function handleGetEwsItemById(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const serverId = url.searchParams.get('server_id') || '';
  if (!owner || !serverId) return new Response('Missing owner/server_id', { status: 400 });
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT server_id, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ? AND server_id = ? LIMIT 1`)
    .bind(owner, serverId)
    .all();
  return Response.json((result.results || [])[0] || null);
}


// Look up the owner of an item by server_id (for delegate access)
async function handleGetEwsItemOwner(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const serverId = url.searchParams.get('server_id') || '';
  if (!serverId) return new Response('Missing server_id', { status: 400 });
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT owner FROM item_map WHERE server_id = ? LIMIT 1`)
    .bind(serverId)
    .first();
  return Response.json(result || null);
}
async function handleGetLatestChangeSeq(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const result = await env.EXCHANGE_DB.prepare('SELECT COALESCE(MAX(id), 0) AS seq FROM change_journal').all();
  return Response.json((result.results || [])[0] || { seq: 0 });
}

async function handleListChangesSinceSeq(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const since = Number(url.searchParams.get('since') || '0');
  if (!owner) return new Response('Missing owner', { status: 400 });
  const safeSince = Number.isFinite(since) ? since : 0;
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT cj.id AS seq, cj.server_id, im.resource_href
              FROM change_journal cj
              LEFT JOIN item_map im ON im.owner = cj.owner AND im.server_id = cj.server_id
              WHERE cj.owner = ? AND cj.id > ? AND cj.op != 'delete'
              ORDER BY cj.id ASC`)
    .bind(owner, safeSince)
    .all();
  return Response.json(result.results || []);
}

async function handleListDeletedSinceSeq(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const since = Number(url.searchParams.get('since') || '0');
  if (!owner) return new Response('Missing owner', { status: 400 });
  const safeSince = Number.isFinite(since) ? since : 0;
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT id AS seq, server_id FROM change_journal
              WHERE owner = ? AND id > ? AND op = 'delete' ORDER BY id ASC`)
    .bind(owner, safeSince)
    .all();
  return Response.json(result.results || []);
}

async function handleListJournalSinceSeq(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const since = Number(url.searchParams.get('since') || '0');
  const until = Number(url.searchParams.get('until') || '0');
  const limit = Number(url.searchParams.get('limit') || '100');
  if (!owner) return new Response('Missing owner', { status: 400 });
  const safeSince = Number.isFinite(since) ? since : 0;
  const safeUntil = Number.isFinite(until) && until > 0 ? until : Number.MAX_SAFE_INTEGER;
  const safeLimit = Math.max(1, Math.min(512, Number.isFinite(limit) ? limit : 100));
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT cj.id AS seq, cj.server_id, cj.op, im.resource_href
              FROM change_journal cj
              LEFT JOIN item_map im ON im.owner = cj.owner AND im.server_id = cj.server_id
              WHERE cj.owner = ? AND cj.id > ? AND cj.id <= ?
              ORDER BY cj.id ASC LIMIT ?`)
    .bind(owner, safeSince, safeUntil, safeLimit)
    .all();
  return Response.json(result.results || []);
}

async function cleanupIdempotencyKeys(env) {
  try {
    await env.EXCHANGE_DB
      .prepare(`DELETE FROM api_idempotency WHERE created_at < datetime('now', '-24 hours')`)
      .run();
  } catch (e) {
    console.error('Failed to clean up idempotency keys:', e);
  }
}

async function handleUpsertCalendarException(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleUpsertCalendarException');
  try {
    const body = await readJson(request);
    const {
      owner = '',
      parent_server_id = '',
      exception_start = '',
      server_id = null,
      is_deleted = 0
    } = body;
    if (!owner || !parent_server_id || !exception_start) {
      return new Response('Missing owner/parent_server_id/exception_start', { status: 400 });
    }
    const isDeletedInt = Number(is_deleted) || 0;
    await env.EXCHANGE_DB
      .prepare(`INSERT INTO calendar_exceptions (owner, parent_server_id, exception_start, server_id, is_deleted, created_at)
        VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT(owner, parent_server_id, exception_start) DO UPDATE SET
          server_id = excluded.server_id,
          is_deleted = excluded.is_deleted,
          created_at = CURRENT_TIMESTAMP`)
      .bind(owner, parent_server_id, exception_start, server_id, isDeletedInt)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error upserting calendar exception:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleGetCalendarExceptions(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const parentServerId = url.searchParams.get('parent_server_id') || '';
  if (!owner || !parentServerId) {
    return new Response('Missing owner/parent_server_id', { status: 400 });
  }

  try {
    const result = await env.EXCHANGE_DB
      .prepare(`SELECT parent_server_id, exception_start, server_id, is_deleted, created_at
        FROM calendar_exceptions WHERE owner = ? AND parent_server_id = ? ORDER BY exception_start ASC`)
      .bind(owner, parentServerId)
      .all();
		return Response.json(result.results || []);
  } catch (error) {
    console.error('Error fetching calendar exceptions:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleGetCalendarException(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const parentServerId = url.searchParams.get('parent_server_id') || '';
  const exceptionStart = url.searchParams.get('exception_start') || '';
  if (!owner || !parentServerId || !exceptionStart) {
    return new Response('Missing owner/parent_server_id/exception_start', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT parent_server_id, exception_start, server_id, is_deleted, created_at
      FROM calendar_exceptions WHERE owner = ? AND parent_server_id = ? AND exception_start = ? LIMIT 1`)
    .bind(owner, parentServerId, exceptionStart)
    .all();
  return Response.json((result.results || [])[0] || null);
}

async function handleDeleteCalendarException(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleDeleteCalendarException');
  try {
    const body = await readJson(request);
    const { owner = '', parent_server_id = '', exception_start = '' } = body;
    if (!owner || !parent_server_id || !exception_start) {
      return new Response('Missing owner/parent_server_id/exception_start', { status: 400 });
    }
    await env.EXCHANGE_DB
      .prepare(`DELETE FROM calendar_exceptions WHERE owner = ? AND parent_server_id = ? AND exception_start = ?`)
      .bind(owner, parent_server_id, exception_start)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error deleting calendar exception:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleRecordMeetingResponse(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleRecordMeetingResponse');
  try {
    const body = await readJson(request);
    const { owner = '', request_id = '', calendar_id = '', user_response = 0 } = body;
    if (!owner || !request_id || !calendar_id) {
      return new Response('Missing owner/request_id/calendar_id', { status: 400 });
    }
    const userResponseInt = Number(user_response) || 0;
    await env.EXCHANGE_DB
      .prepare(`INSERT INTO meeting_response (owner, request_id, calendar_id, user_response, created_at)
        VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT(owner, request_id) DO UPDATE SET
          calendar_id = excluded.calendar_id,
          user_response = excluded.user_response,
          created_at = CURRENT_TIMESTAMP`)
      .bind(owner, request_id, calendar_id, userResponseInt)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error recording meeting response:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleGetMeetingResponse(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const requestId = url.searchParams.get('request_id') || '';
  if (!owner || !requestId) {
    return new Response('Missing owner/request_id', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare(`SELECT request_id, calendar_id, user_response, created_at
      FROM meeting_response WHERE owner = ? AND request_id = ? LIMIT 1`)
    .bind(owner, requestId)
    .all();
  return Response.json((result.results || [])[0] || null);
}

async function handleGetCalendarPermission(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const folderId = url.searchParams.get('folder_id') || '';
  const userEmail = url.searchParams.get('user_email') || '';
  if (!owner || !folderId || !userEmail) {
    return new Response('Missing owner/folder_id/user_email', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ? AND folder_id = ? AND user_email = ? LIMIT 1')
    .bind(owner, folderId, userEmail)
    .first();
  return Response.json(result || null);
}

async function handleGetCalendarPermissions(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const folderId = url.searchParams.get('folder_id') || '';
  if (!owner || !folderId) {
    return new Response('Missing owner/folder_id', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ? AND folder_id = ? ORDER BY user_email ASC')
    .bind(owner, folderId)
    .all();
  return Response.json(result.results || []);
}

async function handleGetUserCalendarPermissions(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const userEmail = url.searchParams.get('user_email') || '';
  if (!owner || !userEmail) {
    return new Response('Missing owner/user_email', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ? AND user_email = ? ORDER BY folder_id ASC')
    .bind(owner, userEmail)
    .all();
  return Response.json(result.results || []);
}

async function handleGetDefaultCalendarPermission(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const folderId = url.searchParams.get('folder_id') || '';
  if (!owner || !folderId) {
    return new Response('Missing owner/folder_id', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ? AND folder_id = ? AND is_default = 1 LIMIT 1')
    .bind(owner, folderId)
    .first();
  return Response.json(result || null);
}

async function handleGetAnonymousCalendarPermission(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const folderId = url.searchParams.get('folder_id') || '';
  if (!owner || !folderId) {
    return new Response('Missing owner/folder_id', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ? AND folder_id = ? AND is_anonymous = 1 LIMIT 1')
    .bind(owner, folderId)
    .first();
  return Response.json(result || null);
}

async function handleUpsertCalendarPermission(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleUpsertCalendarPermission');
  try {
    const body = await readJson(request);
    const { id = '', folder_id = '', owner = '', user_email = '', user_name = null, rights = 0, is_default = 0, is_anonymous = 0 } = body;
    if (!id || !folder_id || !owner || !user_email) {
      return new Response('Missing id/folder_id/owner/user_email', { status: 400 });
    }
    const rightsInt = Number(rights) || 0;
    const isDefaultInt = Number(is_default) || 0;
    const isAnonymousInt = Number(is_anonymous) || 0;
    await env.EXCHANGE_DB
      .prepare('INSERT INTO calendar_permission (id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP) ON CONFLICT(owner, folder_id, user_email) DO UPDATE SET rights = excluded.rights, user_name = excluded.user_name, is_default = excluded.is_default, is_anonymous = excluded.is_anonymous, updated_at = CURRENT_TIMESTAMP')
      .bind(id, folder_id, owner, user_email, user_name, rightsInt, isDefaultInt, isAnonymousInt)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error upserting calendar permission:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleDeleteCalendarPermission(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleDeleteCalendarPermission');
  try {
    const body = await readJson(request);
    const { owner = '', folder_id = '', user_email = '' } = body;
    if (!owner || !folder_id || !user_email) {
      return new Response('Missing owner/folder_id/user_email', { status: 400 });
    }
    await env.EXCHANGE_DB
      .prepare('DELETE FROM calendar_permission WHERE owner = ? AND folder_id = ? AND user_email = ?')
      .bind(owner, folder_id, user_email)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error deleting calendar permission:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleGetDelegate(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const delegator = url.searchParams.get('delegator') || '';
  const delegateEmail = url.searchParams.get('delegate_email') || '';
  if (!delegator || !delegateEmail) {
    return new Response('Missing delegator/delegate_email', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private, created_at, updated_at FROM calendar_delegate WHERE delegator = ? AND delegate_email = ? LIMIT 1')
    .bind(delegator, delegateEmail)
    .first();
  return Response.json(result || null);
}

async function handleGetDelegates(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const delegator = url.searchParams.get('delegator') || '';
  if (!delegator) {
    return new Response('Missing delegator', { status: 400 });
  }
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private, created_at, updated_at FROM calendar_delegate WHERE delegator = ? ORDER BY delegate_email ASC')
    .bind(delegator)
    .all();
  return Response.json(result.results || []);
}

async function handleUpsertDelegate(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleUpsertDelegate');
  try {
    const body = await readJson(request);
    const { id = '', delegator = '', delegate_email = '', delegate_name = null, calendar_permission = 0, inbox_permission = 0, tasks_permission = 0, contacts_permission = 0, notes_permission = 0, journal_permission = 0, receive_copies = 0, receive_infos = 0, view_private = 0 } = body;
    if (!id || !delegator || !delegate_email) {
      return new Response('Missing id/delegator/delegate_email', { status: 400 });
    }
    await env.EXCHANGE_DB
      .prepare('INSERT INTO calendar_delegate (id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP) ON CONFLICT(delegator, delegate_email) DO UPDATE SET delegate_name = excluded.delegate_name, calendar_permission = excluded.calendar_permission, inbox_permission = excluded.inbox_permission, tasks_permission = excluded.tasks_permission, contacts_permission = excluded.contacts_permission, notes_permission = excluded.notes_permission, journal_permission = excluded.journal_permission, receive_copies = excluded.receive_copies, receive_infos = excluded.receive_infos, view_private = excluded.view_private, updated_at = CURRENT_TIMESTAMP')
      .bind(id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error upserting delegate:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleDeleteDelegate(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleDeleteDelegate');
  try {
    const body = await readJson(request);
    const { delegator = '', delegate_email = '' } = body;
    if (!delegator || !delegate_email) {
      return new Response('Missing delegator/delegate_email', { status: 400 });
    }
    await env.EXCHANGE_DB
      .prepare('DELETE FROM calendar_delegate WHERE delegator = ? AND delegate_email = ?')
      .bind(delegator, delegate_email)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error deleting delegate:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleAddPermissionAudit(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleAddPermissionAudit');
  try {
    const body = await readJson(request);
    const { id = '', folder_id = '', owner = '', actor_email = '', target_email = '', operation = '', old_rights = null, new_rights = null } = body;
    if (!id || !folder_id || !owner || !actor_email || !target_email || !operation) {
      return new Response('Missing required fields', { status: 400 });
    }
    await env.EXCHANGE_DB
      .prepare('INSERT INTO permission_audit (id, folder_id, owner, actor_email, target_email, operation, old_rights, new_rights, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)')
      .bind(id, folder_id, owner, actor_email, target_email, operation, old_rights, new_rights)
      .run();
    return Response.json({ success: true });
  } catch (error) {
    if (error.message === 'Invalid JSON') {
      return new Response('Invalid JSON body', { status: 400 });
    }
    console.error('Error adding permission audit:', error);
    return new Response('Internal Server Error', { status: 500 });
  }
}

async function handleGetPermissionAuditLog(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const folderId = url.searchParams.get('folder_id') || '';
  const limit = Number(url.searchParams.get('limit') || '100');
  if (!owner || !folderId) {
    return new Response('Missing owner/folder_id', { status: 400 });
  }
  const safeLimit = Math.max(1, Math.min(1000, Number.isFinite(limit) ? limit : 100));
  const result = await env.EXCHANGE_DB
    .prepare('SELECT id, folder_id, owner, actor_email, target_email, operation, old_rights, new_rights, created_at FROM permission_audit WHERE owner = ? AND folder_id = ? ORDER BY created_at DESC LIMIT ?')
    .bind(owner, folderId, safeLimit)
    .all();
  return Response.json(result.results || []);
}


// Attachment API handlers (v7)

async function handleUpsertCalendarAttachment(request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    await checkIdempotency(request, env, 'handleUpsertCalendarAttachment');
    try {
        const body = await readJson(request);
        const {
            id = '', parent_item_server_id = '', owner = '', name = '',
            content_type = 'application/octet-stream', content_size = 0,
            content_base64 = '', is_inline = 0, content_id = null,
            content_location = null, attachment_type = 'file',
            last_modified_time = null
        } = body;
        if (!id || !parent_item_server_id || !owner) {
            return new Response('Missing id/parent_item_server_id/owner', { status: 400 });
        }
        const isInlineInt = Number(is_inline) || 0;
        const contentSizeInt = Number(content_size) || 0;
        await env.EXCHANGE_DB
            .prepare(`INSERT INTO calendar_attachment (id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP) ON CONFLICT(id) DO UPDATE SET name = excluded.name, content_type = excluded.content_type, content_size = excluded.content_size, content_base64 = excluded.content_base64, is_inline = excluded.is_inline, content_id = excluded.content_id, content_location = excluded.content_location, attachment_type = excluded.attachment_type, last_modified_time = excluded.last_modified_time, updated_at = CURRENT_TIMESTAMP`)
            .bind(id, parent_item_server_id, owner, name, content_type, contentSizeInt, content_base64, isInlineInt, content_id, content_location, attachment_type, last_modified_time)
            .run();
        return Response.json({ success: true });
    } catch (error) {
        if (error.message === 'Invalid JSON') {
            return new Response('Invalid JSON body', { status: 400 });
        }
        console.error('Error upserting calendar attachment:', error);
        return new Response('Internal Server Error', { status: 500 });
    }
}

async function handleGetCalendarAttachment(url, request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    const owner = url.searchParams.get('owner') || '';
    const attachmentId = url.searchParams.get('attachment_id') || '';
    if (!owner || !attachmentId) {
        return new Response('Missing owner/attachment_id', { status: 400 });
    }
    const result = await env.EXCHANGE_DB
        .prepare('SELECT id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time, created_at, updated_at FROM calendar_attachment WHERE owner = ? AND id = ? LIMIT 1')
        .bind(owner, attachmentId)
        .first();
    return Response.json(result || null);
}

async function handleGetCalendarAttachmentsForItem(url, request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    const owner = url.searchParams.get('owner') || '';
    const parentItemServerId = url.searchParams.get('parent_item_server_id') || '';
    if (!owner || !parentItemServerId) {
        return new Response('Missing owner/parent_item_server_id', { status: 400 });
    }
    const result = await env.EXCHANGE_DB
        .prepare('SELECT id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time, created_at, updated_at FROM calendar_attachment WHERE owner = ? AND parent_item_server_id = ? ORDER BY name ASC')
        .bind(owner, parentItemServerId)
        .all();
    return Response.json(result.results || []);
}

async function handleDeleteCalendarAttachment(request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    await checkIdempotency(request, env, 'handleDeleteCalendarAttachment');
    try {
        const body = await readJson(request);
        const { owner = '', attachment_id = '' } = body;
        if (!owner || !attachment_id) {
            return new Response('Missing owner/attachment_id', { status: 400 });
        }
        await env.EXCHANGE_DB
            .prepare('DELETE FROM calendar_attachment WHERE owner = ? AND id = ?')
            .bind(owner, attachment_id)
            .run();
        return Response.json({ success: true });
    } catch (error) {
        if (error.message === 'Invalid JSON') {
            return new Response('Invalid JSON body', { status: 400 });
        }
        console.error('Error deleting calendar attachment:', error);
        return new Response('Internal Server Error', { status: 500 });
    }
}

// Room/Resource API handlers (v8)

async function handleUpsertRoomList(request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    await checkIdempotency(request, env, 'handleUpsertRoomList');
    try {
        const body = await readJson(request);
        const { id = '', email = '', name = '' } = body;
        if (!id || !email || !name) {
            return new Response('Missing id/email/name', { status: 400 });
        }
        await env.EXCHANGE_DB
            .prepare(`INSERT INTO room_list (id, email, name, created_at, updated_at) VALUES (?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP) ON CONFLICT(email) DO UPDATE SET name = excluded.name, updated_at = CURRENT_TIMESTAMP`)
            .bind(id, email, name)
            .run();
        return Response.json({ success: true });
    } catch (error) {
        if (error.message === 'Invalid JSON') {
            return new Response('Invalid JSON body', { status: 400 });
        }
        console.error('Error upserting room list:', error);
        return new Response('Internal Server Error', { status: 500 });
    }
}

async function handleGetRoomLists(url, request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    const result = await env.EXCHANGE_DB
        .prepare('SELECT id, email, name, created_at, updated_at FROM room_list ORDER BY name ASC')
        .all();
    return Response.json(result.results || []);
}

async function handleDeleteRoomList(request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    await checkIdempotency(request, env, 'handleDeleteRoomList');
    try {
        const body = await readJson(request);
        const { email = '' } = body;
        if (!email) return new Response('Missing email', { status: 400 });
        await env.EXCHANGE_DB
            .prepare('DELETE FROM room_list WHERE email = ?')
            .bind(email)
            .run();
        return Response.json({ success: true });
    } catch (error) {
        if (error.message === 'Invalid JSON') {
            return new Response('Invalid JSON body', { status: 400 });
        }
        console.error('Error deleting room list:', error);
        return new Response('Internal Server Error', { status: 500 });
    }
}

async function handleUpsertRoom(request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    await checkIdempotency(request, env, 'handleUpsertRoom');
    try {
        const body = await readJson(request);
        const { id = '', room_list_email = null, email = '', name = '', capacity = 0, is_available = 1 } = body;
        if (!id || !email || !name) {
            return new Response('Missing id/email/name', { status: 400 });
        }
        const capacityInt = Number(capacity) || 0;
        const isAvailableInt = is_available !== undefined && is_available !== null ? Number(is_available) : 1;
        await env.EXCHANGE_DB
            .prepare(`INSERT INTO room (id, room_list_email, email, name, capacity, is_available, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP) ON CONFLICT(email) DO UPDATE SET room_list_email = excluded.room_list_email, name = excluded.name, capacity = excluded.capacity, is_available = excluded.is_available, updated_at = CURRENT_TIMESTAMP`)
            .bind(id, room_list_email, email, name, capacityInt, isAvailableInt)
            .run();
        return Response.json({ success: true });
    } catch (error) {
        if (error.message === 'Invalid JSON') {
            return new Response('Invalid JSON body', { status: 400 });
        }
        console.error('Error upserting room:', error);
        return new Response('Internal Server Error', { status: 500 });
    }
}

async function handleGetRoomsForList(url, request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    const roomListEmail = url.searchParams.get('room_list_email') || '';
    if (!roomListEmail) {
        return new Response('Missing room_list_email', { status: 400 });
    }
    const result = await env.EXCHANGE_DB
        .prepare('SELECT id, room_list_email, email, name, capacity, is_available, created_at, updated_at FROM room WHERE room_list_email = ? ORDER BY name ASC')
        .bind(roomListEmail)
        .all();
    return Response.json(result.results || []);
}

async function handleGetAllRooms(url, request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    const result = await env.EXCHANGE_DB
        .prepare('SELECT id, room_list_email, email, name, capacity, is_available, created_at, updated_at FROM room ORDER BY name ASC')
        .all();
    return Response.json(result.results || []);
}

async function handleDeleteRoom(request, env) {
    if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
    await checkIdempotency(request, env, 'handleDeleteRoom');
    try {
        const body = await readJson(request);
        const { email = '' } = body;
        if (!email) return new Response('Missing email', { status: 400 });
        await env.EXCHANGE_DB
            .prepare('DELETE FROM room WHERE email = ?')
            .bind(email)
            .run();
        return Response.json({ success: true });
    } catch (error) {
        if (error.message === 'Invalid JSON') {
            return new Response('Invalid JSON body', { status: 400 });
        }
        console.error('Error deleting room:', error);
        return new Response('Internal Server Error', { status: 500 });
    }
}
