export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname.toLowerCase();

    if (path.startsWith("/api/")) {
      return handleApiRequest(request, env);
    }

    if (path.includes("autodiscover")) {
      if (path.includes(".json")) {
        return handleAutodiscoverJson(url, env);
      }
      return handleAutodiscoverXml(request, env);
    }

    return new Response("Not Found", { status: 404 });
  }
};

async function handleApiRequest(request, env) {
  const authHeader = request.headers.get("Authorization");
  const expectedSecret = `Bearer ${env.GATEWAY_SECRET}`;

  if (authHeader !== expectedSecret) {
    return new Response("Unauthorized", { status: 401 });
  }

  let body;
  try {
    body = await request.json();
  } catch (e) {
    return new Response("Invalid JSON", { status: 400 });
  }

  const { query, params } = body;

  if (!query) {
    return new Response("Missing 'query' field", { status: 400 });
  }

  try {
    let stmt = env.EXCHANGE_DB.prepare(query);
    if (params && Array.isArray(params)) {
      stmt = stmt.bind(...params);
    }
    
    const result = await stmt.all();
    
    return Response.json({
      success: result.success ?? true,
      errors: result.success === false
        ? [{ message: result.errors?.[0]?.message ?? "DB query failed" }]
        : [],
      result: [
        { results: result.results, meta: result.meta }
      ]
    });
  } catch (e) {
    console.error("D1 Error:", e);
    return Response.json({ error: { message: e.message } }, { status: 500 });
  }
}

async function handleAutodiscoverJson(url, env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response("Config Error", { status: 500 });
  
  return new Response(JSON.stringify({
    "Protocol": "Exchange",
    "Url": `https://${domain}/EWS/Exchange.asmx`
  }), { headers: { "Content-Type": "application/json" }});
}

async function handleAutodiscoverXml(request, env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response("Config Error", { status: 500 });

  let email = "";
  try {
    const body = await request.text();
    const match = body.match(/<EMailAddress>(.*?)<\/EMailAddress>/i);
    if (match) email = match[1];
  } catch (e) {}

  const xml = `<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <User>
      <DisplayName>Stalwart Mail</DisplayName>
      <EMailAddress>${escapeXml(email)}</EMailAddress>
    </User>
    <Account>
      <AccountType>email</AccountType>
      <Action>settings</Action>
      <Protocol>
        <Type>EXCH</Type>
        <Server>${domain}</Server>
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
    headers: { "Content-Type": "application/xml; charset=utf-8" }
  });
}

function escapeXml(unsafe) {
    return unsafe.replace(/[<>&'"]/g, function (c) {
        switch (c) {
            case '<': return '&lt;'; case '>': return '&gt;';
            case '&': return '&amp;'; case '\'': return '&apos;';
            case '"': return '&quot;';
        }
    });
}
