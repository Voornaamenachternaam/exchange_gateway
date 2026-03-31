# worker/index.js — Required additions for GAP-42

Add the following line to the routing block in `fetch()`, after the existing
typed API routes (e.g. after the `get_ews_item_by_id` route):

```javascript
if (path === '/api/upsert_device_info') return handleUpsertDeviceInfo(request, env);
```

Add the following function implementation anywhere in the file:

```javascript
async function handleUpsertDeviceInfo(request, env) {
  if (!isAuthorized(request, env)) return new Response('Unauthorized', { status: 401 });
  await checkIdempotency(request, env, 'handleUpsertDeviceInfo');
  const body = await readJson(request);
  const {
    owner = '',
    device_id = '',
    friendly_name = '',
    model = '',
    os = '',
    phone_number = '',
    imei = '',
    user_agent = '',
  } = body;
  if (!owner || !device_id) {
    return new Response('Missing owner/device_id', { status: 400 });
  }
  await env.EXCHANGE_DB
    .prepare(`
      INSERT INTO device_info (user_email, device_id, friendly_name, last_seen)
      VALUES (?, ?, ?, CURRENT_TIMESTAMP)
      ON CONFLICT(device_id)
      DO UPDATE SET
        user_email = excluded.user_email,
        friendly_name = excluded.friendly_name,
        last_seen = CURRENT_TIMESTAMP
    `)
    .bind(owner, device_id, friendly_name || model || 'Unknown')
    .run();
  return Response.json({ success: true });
}
```
