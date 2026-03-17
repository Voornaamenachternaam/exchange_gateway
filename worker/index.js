const FORWARDED_PATH_PREFIXES = ['/ews', '/microsoft-server-activesync'];
const DEFAULT_MAX_BODY_BYTES = 1024 * 1024;

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname.toLowerCase();

    if (isForwardedPath(path)) {
      return handleGatewayForward(request, env, ctx);
    }

    if (path === '/api/set_sync_key') return handleSetSyncKey(request, env);
    if (path === '/api/upsert_item_map') return handleUpsertItemMap(request, env);
    if (path === '/api/delete_item_by_server_id') return handleDeleteItemByServerId(request, env);
    if (path === '/api/list_changes_since') return handleListChangesSince(url, request, env);
    if (path === '/api/set_provision_policy') return handleSetProvisionPolicy(request, env);
    if (path === '/api/get_provision_policy') return handleGetProvisionPolicy(url, request, env);
    if (path === '/api/list_ews_items') return handleListEwsItems(url, request, env);
    if (path === '/api/get_ews_sync_state') return handleGetEwsSyncState(url, request, env);
    if (path === '/api/set_ews_sync_state') return handleSetEwsSyncState(request, env);
    if (path === '/api/get_ews_item_by_id') return handleGetEwsItemById(url, request, env);

    if (path.startsWith('/api/')) {
      return handleApiRequest(request, env);
    }

    if (
      path.includes('/autodiscover/') ||
      path.endsWith('/autodiscover.xml') ||
      path.endsWith('/autodiscover.svc') ||
      path.includes('/autodiscover.json')
    ) {
      if (path.includes('.json')) {
        return handleAutodiscoverJson(env);
      }
      if (path.endsWith('.svc')) {
        return handleAutodiscoverSoap(request, env);
      }
      return handleAutodiscoverXml(request, env);
    }

    return new Response('Not Found', { status: 404 });
  }
};

function isForwardedPath(path) {
  return FORWARDED_PATH_PREFIXES.some((prefix) => path.startsWith(prefix));
}

function isAuthorized(request, env) {
  const bearer = request.headers.get('Authorization');
  const xSecret = request.headers.get('x-gateway-secret');
  const expectedBearer = `Bearer ${env.GATEWAY_SECRET}`;
  return subtleEqual(bearer ?? '', expectedBearer ?? '') || subtleEqual(xSecret ?? '', env.GATEWAY_SECRET ?? '');
}

function subtleEqual(a, b) {
  if (typeof a !== 'string' || typeof b !== 'string') return false;
  if (a.length !== b.length) return false;
  let mismatch = 0;
  for (let i = 0; i < a.length; i += 1) mismatch |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return mismatch === 0;
}

async function readJson(request) {
  try {
    return await request.json();
  } catch {
    throw new Error('Invalid JSON');
  }
}

async function checkIdempotency(request, env, routeName) {
  const key = request.headers.get('Idempotency-Key');
  if (!key) return;
  await env.EXCHANGE_DB
    .prepare(`
      INSERT INTO api_idempotency (idempotency_key, route_name, created_at)
      VALUES (?, ?, CURRENT_TIMESTAMP)
      ON CONFLICT(idempotency_key) DO NOTHING
    `)
    .bind(key, routeName)
    .run();
}

async function handleGatewayForward(request, env, ctx) {
  if (!env.ORIGIN_BASE_URL) {
    return new Response('Worker misconfigured: ORIGIN_BASE_URL is required', { status: 500 });
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
      headers: {
        'Retry-After': String(rateLimitResult.retryAfterSec),
        'Cache-Control': 'private, no-store'
      }
    });
  }

  const incoming = new URL(request.url);
  const upstream = new URL(incoming.pathname + incoming.search, env.ORIGIN_BASE_URL);
  const forwardedHeaders = new Headers(request.headers);
  forwardedHeaders.set('X-Forwarded-Proto', 'https');
  forwardedHeaders.set('X-Forwarded-Host', incoming.host);
  forwardedHeaders.set('X-Forwarded-For', request.headers.get('CF-Connecting-IP') || '');

  const method = (request.method || 'GET').toUpperCase();
  const upstreamRequest = new Request(upstream.toString(), {
    method,
    headers: forwardedHeaders,
    body: method === 'GET' || method === 'HEAD' ? undefined : request.body,
    redirect: 'manual'
  });

  const upstreamResponse = await fetch(upstreamRequest);
  return withCommonSecurityHeaders(upstreamResponse);
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
  try {
    current = currentRaw ? JSON.parse(currentRaw) : { count: 0, resetAt: now + windowSec };
  } catch {
    current = { count: 0, resetAt: now + windowSec };
  }
  if (now >= current.resetAt) {
    current.count = 0;
    current.resetAt = now + windowSec;
  }

  current.count += 1;
  const ttl = Math.max(1, current.resetAt - now);
  ctx.waitUntil(env.RATE_LIMIT_KV.put(key, JSON.stringify(current), { expirationTtl: ttl }));

  const allowed = current.count <= max;
  return { allowed, retryAfterSec: Math.max(1, current.resetAt - now) };
}


function withCommonSecurityHeaders(response) {
  const headers = new Headers(response.headers);
  headers.set('X-Content-Type-Options', 'nosniff');
  headers.set('Referrer-Policy', 'no-referrer');
  headers.set('Cache-Control', headers.get('Cache-Control') || 'private, no-store');
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers
  });
}
async function handleSetSyncKey(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleSetSyncKey');
  const body = await readJson(request);
  const owner = body.owner || '';
  const collectionId = body.collection_id || '';
  const syncKey = body.sync_key || '';
  const token = body.token || null;
  if (!owner || !collectionId || !syncKey) {
    return new Response('Missing owner/collection_id/sync_key', { status: 400 });
  }

  await env.EXCHANGE_DB
    .prepare(`
      INSERT INTO sync_state (owner, collection_id, sync_key, token, updated_at)
      VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
      ON CONFLICT(owner, collection_id)
      DO UPDATE SET sync_key = excluded.sync_key, token = excluded.token, updated_at = CURRENT_TIMESTAMP
    `)
    .bind(owner, collectionId, syncKey, token)
    .run();

  return Response.json({ success: true });
}

async function handleUpsertItemMap(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleUpsertItemMap');
  const body = await readJson(request);
  const { owner = '', caldav_href = '', resource_href = '', server_id = '', uid = '', etag = '' } = body;
  if (!owner || !resource_href || !server_id) {
    return new Response('Missing owner/resource_href/server_id', { status: 400 });
  }

  await env.EXCHANGE_DB
    .prepare(`
      INSERT INTO item_map (owner, caldav_href, resource_href, server_id, uid, etag, updated_at)
      VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
      ON CONFLICT(owner, server_id)
      DO UPDATE SET
        caldav_href = excluded.caldav_href,
        resource_href = excluded.resource_href,
        uid = excluded.uid,
        etag = excluded.etag,
        updated_at = CURRENT_TIMESTAMP
    `)
    .bind(owner, caldav_href, resource_href, server_id, uid, etag)
    .run();

  return Response.json({ success: true });
}

async function handleDeleteItemByServerId(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleDeleteItemByServerId');
  const body = await readJson(request);
  const owner = body.owner || '';
  const serverId = body.server_id || '';
  if (!owner || !serverId) return new Response('Missing owner/server_id', { status: 400 });

  await env.EXCHANGE_DB
    .prepare('DELETE FROM item_map WHERE owner = ? AND server_id = ?')
    .bind(owner, serverId)
    .run();

  return Response.json({ success: true });
}

async function handleListChangesSince(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const since = url.searchParams.get('since') || '0';
  if (!owner) return new Response('Missing owner', { status: 400 });

  const sinceExpr = Number.isFinite(Number(since)) ? Number(since) : 0;
  const result = await env.EXCHANGE_DB
    .prepare(`
      SELECT server_id, resource_href
      FROM item_map
      WHERE owner = ?
        AND strftime('%s', updated_at) >= ?
      ORDER BY updated_at ASC
    `)
    .bind(owner, sinceExpr)
    .all();

  return Response.json(result.results || []);
}

async function handleApiRequest(request, env) {
  if (!isAuthorized(request, env)) {
    return new Response('Unauthorized', { status: 401 });
  }

  const sqlApiEnabled = String(env.SQL_API_ENABLED || 'false').toLowerCase() === 'true';
  if (!sqlApiEnabled) {
    return new Response('SQL API disabled', { status: 403 });
  }

  const contentLength = Number.parseInt(request.headers.get('content-length') || '0', 10);
  const maxBodyBytes = Math.max(1024, Number.parseInt(env.MAX_API_BODY_BYTES || `${DEFAULT_MAX_BODY_BYTES}`, 10) || DEFAULT_MAX_BODY_BYTES);
  if (Number.isFinite(contentLength) && contentLength > maxBodyBytes) {
    return new Response('Payload too large', { status: 413 });
  }

  let body;
  try {
    body = await request.json();
  } catch {
    return new Response('Invalid JSON', { status: 400 });
  }

  const { query, params } = body;
  if (!query || typeof query !== 'string') {
    return new Response("Missing 'query' field", { status: 400 });
  }

  const trimmed = query.trim().toUpperCase();
  if (!trimmed.startsWith('SELECT')) {
    return new Response('Only SELECT queries are permitted', { status: 403 });
  }

  try {
    let stmt = env.EXCHANGE_DB.prepare(query);
    if (params && Array.isArray(params)) {
      stmt = stmt.bind(...params);
    }

    const result = await stmt.all();
    return Response.json({
      success: result.success ?? true,
      errors:
        result.success === false
          ? [{ message: result.errors?.[0]?.message ?? 'DB query failed' }]
          : [],
      result: [{ results: result.results, meta: result.meta }]
    });
  } catch (e) {
    console.error('D1 Error:', e);
    return Response.json({ error: { message: e.message } }, { status: 500 });
  }
}

async function handleAutodiscoverJson(env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response('Config Error', { status: 500 });

  const payload = {
    Protocol: 'Exchange',
    Url: `https://${domain}/EWS/Exchange.asmx`,
    EwsUrl: `https://${domain}/EWS/Exchange.asmx`,
    ActiveSyncUrl: `https://${domain}/Microsoft-Server-ActiveSync`
  };

  return new Response(JSON.stringify(payload), {
    headers: {
      'Content-Type': 'application/json',
      'Cache-Control': 'private, no-store'
    }
  });
}

async function handleAutodiscoverXml(request, env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response('Config Error', { status: 500 });

  let email = '';
  try {
    const body = await request.text();
    const match = body.match(/<EMailAddress>(.*?)<\/EMailAddress>/i);
    if (match) email = match[1];
  } catch {}

  const xml = `<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <User>
      <DisplayName>Stalwart Mail</DisplayName>
      <EMailAddress>${escapeXml(email)}</EMailAddress>
      <DeploymentId>00000000-0000-0000-0000-000000000000</DeploymentId>
    </User>
    <Account>
      <AccountType>email</AccountType>
      <Action>settings</Action>
      <Protocol>
        <Type>EXCH</Type>
        <Server>${domain}</Server>
        <ServerDN>/o=Exchange/ou=Exchange Administrative Group/cn=Recipients/cn=user</ServerDN>
        <ASUrl>https://${domain}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://${domain}/EWS/Exchange.asmx</EwsUrl>
        <EmwsUrl>https://${domain}/EWS/Exchange.asmx</EmwsUrl>
      </Protocol>
      <Protocol>
        <Type>EXPR</Type>
        <Server>${domain}</Server>
        <SSL>On</SSL>
        <AuthPackage>Basic</AuthPackage>
        <ASUrl>https://${domain}/Microsoft-Server-ActiveSync</ASUrl>
        <EwsUrl>https://${domain}/EWS/Exchange.asmx</EwsUrl>
      </Protocol>
      <Protocol>
        <Type>MobileSync</Type>
        <Url>https://${domain}/Microsoft-Server-ActiveSync</Url>
      </Protocol>
    </Account>
  </Response>
</Autodiscover>`;

  return new Response(xml, {
    headers: {
      'Content-Type': 'application/xml; charset=utf-8',
      'Cache-Control': 'private, no-store'
    }
  });
}

async function handleAutodiscoverSoap(request, env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response('Config Error', { status: 500 });

  const body = await request.text();
  const emailMatch = body.match(/<a:EMailAddress>(.*?)<\/a:EMailAddress>/i);
  const email = emailMatch ? emailMatch[1] : '';

  const xml = `<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Body>
    <a:GetUserSettingsResponseMessage>
      <a:Response>
        <a:ErrorCode>NoError</a:ErrorCode>
        <a:ErrorMessage />
        <a:UserResponses>
          <a:UserResponse>
            <a:ErrorCode>NoError</a:ErrorCode>
            <a:ErrorMessage />
            <a:RedirectTarget />
            <a:UserSettingErrors />
            <a:UserSettings>
              <a:UserSetting>
                <a:Name>UserDisplayName</a:Name>
                <a:Value>Stalwart Mail</a:Value>
              </a:UserSetting>
              <a:UserSetting>
                <a:Name>UserDN</a:Name>
                <a:Value>${escapeXml(email)}</a:Value>
              </a:UserSetting>
              <a:UserSetting>
                <a:Name>ExternalEwsUrl</a:Name>
                <a:Value>https://${domain}/EWS/Exchange.asmx</a:Value>
              </a:UserSetting>
              <a:UserSetting>
                <a:Name>InternalEwsUrl</a:Name>
                <a:Value>https://${domain}/EWS/Exchange.asmx</a:Value>
              </a:UserSetting>
              <a:UserSetting>
                <a:Name>MobileSyncServer</a:Name>
                <a:Value>${domain}</a:Value>
              </a:UserSetting>
            </a:UserSettings>
          </a:UserResponse>
        </a:UserResponses>
      </a:Response>
    </a:GetUserSettingsResponseMessage>
  </s:Body>
</s:Envelope>`;

  return new Response(xml, {
    headers: {
      'Content-Type': 'application/soap+xml; charset=utf-8',
      'Cache-Control': 'private, no-store'
    }
  });
}

function escapeXml(unsafe = '') {
  return String(unsafe).replace(/[<>&'\"]/g, function (c) {
    switch (c) {
      case '<': return '&lt;';
      case '>': return '&gt;';
      case '&': return '&amp;';
      case '\'': return '&apos;';
      case '"': return '&quot;';
      default: return c;
    }
  });
}

async function handleSetProvisionPolicy(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleSetProvisionPolicy');
  const body = await readJson(request);
  const owner = body.owner || '';
  const deviceId = body.device_id || '';
  const policyKey = body.policy_key || '';
  const policyStatus = body.policy_status || '';
  if (!owner || !deviceId || !policyKey || !policyStatus) {
    return new Response('Missing owner/device_id/policy_key/policy_status', { status: 400 });
  }

  await env.EXCHANGE_DB
    .prepare(`
      INSERT INTO provision_state (owner, device_id, policy_key, policy_status, updated_at)
      VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
      ON CONFLICT(owner, device_id)
      DO UPDATE SET
        policy_key = excluded.policy_key,
        policy_status = excluded.policy_status,
        updated_at = CURRENT_TIMESTAMP
    `)
    .bind(owner, deviceId, policyKey, policyStatus)
    .run();

  return Response.json({ success: true });
}

async function handleGetProvisionPolicy(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const deviceId = url.searchParams.get('device_id') || '';
  if (!owner || !deviceId) return new Response('Missing owner/device_id', { status: 400 });

  const result = await env.EXCHANGE_DB
    .prepare(`
      SELECT policy_key, policy_status
      FROM provision_state
      WHERE owner = ? AND device_id = ?
      LIMIT 1
    `)
    .bind(owner, deviceId)
    .all();

  const row = (result.results || [])[0] || null;
  return Response.json(row);
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
    .prepare(`
      SELECT server_id, resource_href, uid, etag, updated_at
      FROM item_map
      WHERE owner = ?
      ORDER BY updated_at DESC, server_id ASC
      LIMIT ? OFFSET ?
    `)
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
    .prepare(`
      SELECT sync_state
      FROM ews_sync_state
      WHERE user_email = ? AND folder_id = ?
      LIMIT 1
    `)
    .bind(owner, folderId)
    .all();

  const row = (result.results || [])[0] || null;
  return Response.json(row);
}

async function handleSetEwsSyncState(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleSetEwsSyncState');
  const body = await readJson(request);
  const owner = body.owner || '';
  const folderId = body.folder_id || '';
  const syncState = body.sync_state || '';
  if (!owner || !folderId || !syncState) {
    return new Response('Missing owner/folder_id/sync_state', { status: 400 });
  }

  await env.EXCHANGE_DB
    .prepare(`
      INSERT INTO ews_sync_state (user_email, folder_id, sync_state, created_at)
      VALUES (?, ?, ?, CURRENT_TIMESTAMP)
      ON CONFLICT(user_email, folder_id)
      DO UPDATE SET sync_state = excluded.sync_state, created_at = CURRENT_TIMESTAMP
    `)
    .bind(owner, folderId, syncState)
    .run();

  return Response.json({ success: true });
}

async function handleGetEwsItemById(url, request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  const owner = url.searchParams.get('owner') || '';
  const serverId = url.searchParams.get('server_id') || '';
  if (!owner || !serverId) return new Response('Missing owner/server_id', { status: 400 });

  const result = await env.EXCHANGE_DB
    .prepare(`
      SELECT server_id, resource_href, uid, etag, updated_at
      FROM item_map
      WHERE owner = ? AND server_id = ?
      LIMIT 1
    `)
    .bind(owner, serverId)
    .all();

  const row = (result.results || [])[0] || null;
  return Response.json(row);
}
