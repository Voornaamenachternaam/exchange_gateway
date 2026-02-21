/**
 * Cloudflare Worker - exchange-gateway-db
 *
 * Bindings required:
 * - D1 database binding named "EXCHANGE_DB"
 * - Environment variable/secret: GATEWAY_SECRET
 *
 * Routes:
 * - POST /set_sync_key
 * - GET  /get_sync_key?owner=...&collection_id=...
 * - POST /upsert_item_map
 * - GET  /get_item_by_server_id?server_id=...
 * - POST /delete_item_by_server_id
 * - GET  /list_changes_since?owner=...&since=...
 *
 * Authentication: requires header 'x-gateway-secret' == GATEWAY_SECRET
 */

export default {
  async fetch(request, env) {
    try {
      // Basic auth: secret header
      const provided = request.headers.get("x-gateway-secret") || "";
      if (!env.GATEWAY_SECRET || provided !== env.GATEWAY_SECRET) {
        return new Response(JSON.stringify({ error: "unauthorized" }), { status: 401, headers: { "content-type": "application/json" } });
      }

      const url = new URL(request.url);
      const path = url.pathname.replace(/^\/+/, ""); // remove leading slash

      if (request.method === "GET" && path === "get_sync_key") {
        const owner = url.searchParams.get("owner") || "";
        const collection_id = url.searchParams.get("collection_id") || "";
        if (!owner || !collection_id) {
          return new Response(JSON.stringify({ error: "missing params" }), { status: 400, headers: { "content-type": "application/json" } });
        }
        const sql = "SELECT sync_key FROM sync_state WHERE owner = ? AND collection_id = ?";
        const r = await env.EXCHANGE_DB.prepare(sql).bind(owner, collection_id).first();
        const sync_key = r?.sync_key ?? null;
        return new Response(JSON.stringify({ sync_key }), { status: 200, headers: { "content-type": "application/json" } });
      }

      if (request.method === "POST" && path === "set_sync_key") {
        const body = await request.json();
        const owner = body.owner || "";
        const collection_id = body.collection_id || "";
        const sync_key = body.sync_key || "";
        const token = body.token || "";
        if (!owner || !collection_id || !sync_key) {
          return new Response(JSON.stringify({ error: "missing fields" }), { status: 400, headers: { "content-type": "application/json" } });
        }
        const sql = `INSERT INTO sync_state (owner, collection_id, sync_key, last_sync_token, last_sync_ts)
                     VALUES (?, ?, ?, ?, strftime('%s','now'))
                     ON CONFLICT(owner, collection_id) DO UPDATE SET
                       sync_key = excluded.sync_key,
                       last_sync_token = excluded.last_sync_token,
                       last_sync_ts = strftime('%s','now')`;
        await env.EXCHANGE_DB.prepare(sql).bind(owner, collection_id, sync_key, token).run();
        return new Response(JSON.stringify({ ok: true }), { status: 200, headers: { "content-type": "application/json" } });
      }

      if (request.method === "POST" && path === "upsert_item_map") {
        const body = await request.json();
        const owner = body.owner || "";
        const caldav_href = body.caldav_href || "";
        const resource_href = body.resource_href || "";
        const server_id = body.server_id || "";
        const uid = body.uid || "";
        const etag = body.etag || "";
        if (!owner || !resource_href || !server_id) {
          return new Response(JSON.stringify({ error: "missing fields" }), { status: 400, headers: { "content-type": "application/json" } });
        }

        const sql = `INSERT INTO items_map (owner, caldav_href, resource_href, server_id, uid, etag, last_sync)
                     VALUES (?, ?, ?, ?, ?, ?, strftime('%s','now'))
                     ON CONFLICT(server_id) DO UPDATE SET
                       caldav_href = excluded.caldav_href,
                       resource_href = excluded.resource_href,
                       uid = excluded.uid,
                       etag = excluded.etag,
                       last_sync = strftime('%s','now')`;
        await env.EXCHANGE_DB.prepare(sql).bind(owner, caldav_href, resource_href, server_id, uid, etag).run();
        return new Response(JSON.stringify({ ok: true }), { status: 200, headers: { "content-type": "application/json" } });
      }

      if (request.method === "GET" && path === "get_item_by_server_id") {
        const server_id = url.searchParams.get("server_id") || "";
        if (!server_id) {
          return new Response(JSON.stringify({ error: "missing server_id" }), { status: 400, headers: { "content-type": "application/json" } });
        }
        const sql = "SELECT id, resource_href FROM items_map WHERE server_id = ?";
        const r = await env.EXCHANGE_DB.prepare(sql).bind(server_id).first();
        if (!r) {
          return new Response(JSON.stringify({ found: false }), { status: 200, headers: { "content-type": "application/json" } });
        }
        return new Response(JSON.stringify({ found: true, id: r.id, resource_href: r.resource_href }), { status: 200, headers: { "content-type": "application/json" } });
      }

      if (request.method === "POST" && path === "delete_item_by_server_id") {
        const body = await request.json();
        const server_id = body.server_id || "";
        if (!server_id) {
          return new Response(JSON.stringify({ error: "missing server_id" }), { status: 400, headers: { "content-type": "application/json" } });
        }
        const sql = "DELETE FROM items_map WHERE server_id = ?";
        await env.EXCHANGE_DB.prepare(sql).bind(server_id).run();
        return new Response(JSON.stringify({ ok: true }), { status: 200, headers: { "content-type": "application/json" } });
      }

      if (request.method === "GET" && path === "list_changes_since") {
        const owner = url.searchParams.get("owner") || "";
        const since = url.searchParams.get("since") || "0";
        if (!owner) {
          return new Response(JSON.stringify({ error: "missing owner" }), { status: 400, headers: { "content-type": "application/json" } });
        }
        const sql = "SELECT server_id, resource_href FROM items_map WHERE owner = ? AND last_sync >= ?";
        const r = await env.EXCHANGE_DB.prepare(sql).bind(owner, parseInt(since, 10)).all();
        const rows = r.results || [];
        return new Response(JSON.stringify(rows), { status: 200, headers: { "content-type": "application/json" } });
      }

      // unknown route
      return new Response(JSON.stringify({ error: "not_found" }), { status: 404, headers: { "content-type": "application/json" } });
    } catch (err) {
      return new Response(JSON.stringify({ error: err.message || String(err) }), { status: 500, headers: { "content-type": "application/json" } });
    }
  }
};
