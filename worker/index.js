// worker/index.js
const HOP_BY_HOP_HEADERS = [
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade"
];

const FORWARDED_PATH_PREFIXES = [
  "/ews",
  "/microsoft-server-activesync",
  "/autodiscover",
  "/oab"
];

function getUpstreamHost(env) {
  return env.UPSTREAM_HOST || "exchange-origin.example.com";
}

function getUpstreamPort(env) {
  return env.UPSTREAM_PORT || "443";
}

function shouldForward(path) {
  const lower = path.toLowerCase();
  return FORWARDED_PATH_PREFIXES.some(prefix => lower.startsWith(prefix));
}

function removeHopByHopHeaders(headers) {
  const result = new Headers();
  for (const [key, value] of headers.entries()) {
    if (!HOP_BY_HOP_HEADERS.includes(key.toLowerCase())) {
      result.set(key, value);
    }
  }
  return result;
}

function copyHeaders(from, to, exclude = []) {
  for (const [key, value] of from.entries()) {
    if (!exclude.includes(key.toLowerCase()) && !HOP_BY_HOP_HEADERS.includes(key.toLowerCase())) {
      to.set(key, value);
    }
  }
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname;
    const upstreamHost = getUpstreamHost(env);
    const upstreamPort = getUpstreamPort(env);

    if (request.method === "OPTIONS") {
      const corsHeaders = {
        "Access-Control-Allow-Origin": "*",
        "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
        "Access-Control-Allow-Headers": request.headers.get("Access-Control-Request-Headers") || "*",
        "Access-Control-Max-Age": "86400"
      };
      return new Response(null, { status: 204, headers: corsHeaders });
    }

    if (shouldForward(path)) {
      try {
        const upstreamUrl = new URL(request.url);
        upstreamUrl.hostname = upstreamHost;
        upstreamUrl.port = upstreamPort;
        upstreamUrl.protocol = "https:";

        const headers = removeHopByHopHeaders(request.headers);
        headers.set("X-Forwarded-For", request.headers.get("CF-Connecting-IP") || "unknown");
        headers.set("X-Forwarded-Proto", "https");
        headers.set("X-Forwarded-Host", url.host);

        const upstreamResponse = await fetch(upstreamUrl.toString(), {
          method: request.method,
          headers: headers,
          body: request.body,
          redirect: "manual",
          cf: { tlsMinVersion: "TLSv1.2" }
        });

        const responseHeaders = new Headers();
        copyHeaders(upstreamResponse.headers, responseHeaders);
        responseHeaders.set("Access-Control-Allow-Origin", "*");
        responseHeaders.set("Access-Control-Expose-Headers", "*");

        if (upstreamResponse.status === 301 || upstreamResponse.status === 302 || upstreamResponse.status === 307 || upstreamResponse.status === 308) {
          const location = upstreamResponse.headers.get("Location");
          if (location) {
            const newLocation = location.replace(
              new RegExp(`https?://${upstreamHost}(:${upstreamPort})?`, "g"),
              `${url.protocol}//${url.host}`
            );
            responseHeaders.set("Location", newLocation);
          }
        }

        const body = upstreamResponse.body;
        if (body === null) {
          return new Response(null, {
            status: upstreamResponse.status,
            statusText: upstreamResponse.statusText,
            headers: responseHeaders
          });
        }

        return new Response(body, {
          status: upstreamResponse.status,
          statusText: upstreamResponse.statusText,
          headers: responseHeaders
        });

      } catch (error) {
        console.error("Proxy error:", error);
        return new Response("Bad Gateway", { status: 502 });
      }
    }

    if (path === "/") {
      return new Response("Exchange Gateway Worker is running", {
        status: 200,
        headers: {
          "Content-Type": "text/plain",
          "Access-Control-Allow-Origin": "*"
        }
      });
    }

    return new Response("Not Found", { status: 404 });
  }
};