// worker/index.js
const FORWARDED_PATH_PREFIXES = ['/ews', '/oab', '/microsoft-server-activesync'];
const DEFAULT_MAX_FORWARD_BODY_BYTES = 4 * 1024 * 1024;
const DEFAULT_UPSTREAM_TIMEOUT_MS = 30000;
const HOP_BY_HOP_HEADERS = [
  'connection',
  'keep-alive',
  'proxy-authenticate',
  'proxy-authorization',
  'te',
  'trailer',
  'transfer-encoding',
  'upgrade'
];

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const path = url.pathname.toLowerCase();
    const handledPath = isHandledPath(path);

    if (request.method === 'OPTIONS' && handledPath) {
      return new Response(null, { status: 204, headers: corsHeaders() });
    }

    if (isAutodiscover(path)) {
      return withSecurityHeaders(await forward(request, env));
    }

    if (isForwardedPath(path)) {
      return withSecurityHeaders(await forward(request, env));
    }

    if (path === '/health') {
      return withSecurityHeaders(new Response('ok', { status: 200 }));
    }

    return withSecurityHeaders(new Response('Not Found', { status: 404 }));
  }
};

function isForwardedPath(path) {
  return FORWARDED_PATH_PREFIXES.some((prefix) => path.startsWith(prefix));
}

function isAutodiscover(path) {
  return path.includes('/autodiscover/') || path.endsWith('/autodiscover.xml') || path.endsWith('/autodiscover.svc') || path.endsWith('/autodiscover.json');
}

function isHandledPath(path) {
  return isAutodiscover(path) || isForwardedPath(path) || path === '/health';
}

async function forward(request, env) {
  if (!env.ORIGIN_BASE_URL) {
    return new Response('Worker misconfigured: ORIGIN_BASE_URL is required', { status: 500 });
  }
  if (!isValidOriginBaseUrl(env.ORIGIN_BASE_URL)) {
    return new Response('Worker misconfigured: ORIGIN_BASE_URL must be http/https', { status: 500 });
  }
  const incoming = new URL(request.url);
  const method = (request.method || 'GET').toUpperCase();
  const allow = allowedMethodsForPath(incoming.pathname.toLowerCase());
  if (!allow.includes(method)) {
    return new Response('Method Not Allowed', { status: 405, headers: { Allow: allow.join(', ') } });
  }
  const contentLength = Number.parseInt(request.headers.get('content-length') || '0', 10);
  const maxBodyBytes = Math.max(
    1024,
    Number.parseInt(env.MAX_FORWARD_BODY_BYTES || `${DEFAULT_MAX_FORWARD_BODY_BYTES}`, 10) || DEFAULT_MAX_FORWARD_BODY_BYTES
  );
  if (Number.isFinite(contentLength) && contentLength > maxBodyBytes) {
    return new Response('Payload Too Large', { status: 413 });
  }
  const rateLimit = await enforceRateLimit(request, env);
  if (!rateLimit.allowed) {
    return new Response('Too Many Requests', {
      status: 429,
      headers: { 'Retry-After': String(rateLimit.retryAfterSec) }
    });
  }
  const upstream = new URL(incoming.pathname + incoming.search, env.ORIGIN_BASE_URL);
  const headers = sanitizeForwardHeaders(request.headers);
  headers.set('X-Forwarded-Proto', 'https');
  headers.set('X-Forwarded-Host', incoming.host);
  const clientIp = request.headers.get('CF-Connecting-IP') || '';
  const priorForwardedFor = request.headers.get('X-Forwarded-For');
  headers.set('X-Forwarded-For', priorForwardedFor ? `${priorForwardedFor}, ${clientIp}` : clientIp);
  const timeoutMs = Math.max(
    1000,
    Number.parseInt(env.UPSTREAM_TIMEOUT_MS || `${DEFAULT_UPSTREAM_TIMEOUT_MS}`, 10) || DEFAULT_UPSTREAM_TIMEOUT_MS
  );
  const ctrl = new AbortController();
  const timeout = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    return await fetch(new Request(upstream.toString(), {
      method: request.method,
      headers,
      body: request.method === 'GET' || request.method === 'HEAD' ? undefined : request.body,
      redirect: 'manual',
      signal: ctrl.signal
    }));
  } catch {
    return new Response('Bad Gateway', { status: 502, headers: { 'Cache-Control': 'private, no-store' } });
  } finally {
    clearTimeout(timeout);
  }
}

function allowedMethodsForPath(path) {
  if (path.startsWith('/ews/')) return ['POST', 'OPTIONS'];
  if (path.startsWith('/microsoft-server-activesync')) return ['POST', 'OPTIONS'];
  if (path.startsWith('/oab/')) return ['GET', 'HEAD', 'OPTIONS'];
  if (isAutodiscover(path)) return ['GET', 'POST', 'OPTIONS'];
  if (path === '/health') return ['GET', 'HEAD', 'OPTIONS'];
  return ['GET', 'POST', 'HEAD', 'OPTIONS'];
}

function sanitizeForwardHeaders(inputHeaders) {
  const headers = new Headers(inputHeaders);
  for (const h of HOP_BY_HOP_HEADERS) headers.delete(h);
  return headers;
}

function withSecurityHeaders(response) {
  const headers = new Headers(response.headers);
  const cors = corsHeaders();
  for (const [k, v] of Object.entries(cors)) headers.set(k, v);
  headers.set('X-Content-Type-Options', 'nosniff');
  headers.set('X-Frame-Options', 'DENY');
  headers.set('Referrer-Policy', 'strict-origin-when-cross-origin');
  headers.set('Content-Security-Policy', "default-src 'none'; frame-ancestors 'none'; sandbox");
  headers.set('Strict-Transport-Security', 'max-age=63072000; includeSubDomains');
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

function corsHeaders() {
  return {
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
    'Access-Control-Allow-Headers': 'Content-Type, Authorization',
    'Access-Control-Max-Age': '86400'
  };
}

function isValidOriginBaseUrl(raw) {
  try {
    const parsed = new URL(raw);
    return parsed.protocol === 'https:' || parsed.protocol === 'http:';
  } catch {
    return false;
  }
}

async function enforceRateLimit(request, env) {
  if (!env.RATE_LIMIT_KV) return { allowed: true, retryAfterSec: 0 };
  const enabled = String(env.RATE_LIMIT_ENABLED || 'true').toLowerCase() === 'true';
  if (!enabled) return { allowed: true, retryAfterSec: 0 };
  const max = Math.max(1, Number.parseInt(env.RATE_LIMIT_MAX || '120', 10) || 120);
  const windowSec = Math.max(10, Number.parseInt(env.RATE_LIMIT_WINDOW_SEC || '60', 10) || 60);
  const ip = request.headers.get('CF-Connecting-IP') || 'unknown';
  const path = new URL(request.url).pathname.toLowerCase();
  const key = `rl:${ip}:${path}`;
  const now = Math.floor(Date.now() / 1000);
  let existing;
  try {
    existing = await env.RATE_LIMIT_KV.get(key);
  } catch {
    return { allowed: true, retryAfterSec: 0 };
  }
  let state;
  try {
    state = existing ? JSON.parse(existing) : { count: 0, resetAt: now + windowSec };
  } catch {
    state = { count: 0, resetAt: now + windowSec };
  }
  if (now >= state.resetAt) state = { count: 0, resetAt: now + windowSec };
  state.count += 1;
  const retryAfterSec = Math.max(1, state.resetAt - now);
  try {
    await env.RATE_LIMIT_KV.put(key, JSON.stringify(state), { expirationTtl: retryAfterSec });
  } catch {
    return { allowed: true, retryAfterSec: 0 };
  }
  return { allowed: state.count <= max, retryAfterSec };
}
