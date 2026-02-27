export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname.toLowerCase();

    // 1. Database API Proxy
    // Route: /api/query (Matches the CF_D1_API_URL in .env)
    if (path.startsWith("/api/")) {
      return handleApiRequest(request, env);
    }

    // 2. AutoDiscover V2 (JSON) - Modern Outlook
    if (path.includes("autodiscover") && path.includes(".json")) {
      return handleAutodiscoverJson(url, env);
    }
    
    // 3. AutoDiscover V1 (XML) - Legacy/Android
    if (path.includes("autodiscover")) {
      return handleAutodiscoverXml(request, env);
    }

    // 4. Proxy to Tunnel (ActiveSync & EWS)
    // Passes request to the tunnel service binding defined in the dashboard
    if (env.GATEWAY_SERVICE) {
      const newHeaders = new Headers(request.headers);
      newHeaders.set("X-Forwarded-Proto", "https");
      // Forward to the Tunnel Service Binding
      return env.GATEWAY_SERVICE.fetch(new Request(request, { headers: newHeaders }));
    }

    return new Response("Service Unavailable", { status: 503 });
  }
};

/**
 * Handles Database Requests from the Exchange Gateway Container
 */
async function handleApiRequest(request, env) {
  // 1. Validate Secret
  // The container sends 'Authorization: Bearer <GATEWAY_SECRET>'
  const authHeader = request.headers.get("Authorization");
  const expectedSecret = `Bearer ${env.GATEWAY_SECRET}`;

  if (authHeader !== expectedSecret) {
    return new Response("Unauthorized", { status: 401 });
  }

  // 2. Parse Request Body
  // Expected JSON: { "query": "SQL_STRING", "params": ["param1", "param2"] }
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

  // 3. Execute against D1 Binding
  try {
    // Prepare the statement
    let stmt = env.EXCHANGE_DB.prepare(query);
    
    // Bind parameters if they exist
    if (params && Array.isArray(params)) {
      stmt = stmt.bind(...params);
    }

    // Run the query
    const result = await stmt.all();
    
    // 4. Format response to match Cloudflare API format expected by Rust code
    // Rust expects: { "result": [ { "results": [...] } ] }
    return Response.json({
      result: [
        { results: result.results }
      ]
    });

  } catch (e) {
    console.error("D1 Error:", e);
    return Response.json({ 
      error: { message: e.message } 
    }, { status: 500 });
  }
}

/**
 * AutoDiscover Handlers
 */
async function handleAutodiscoverJson(url, env) {
  const domain = env.GATEWAY_HOST;
  if (!domain) return new Response("Config Error", { status: 500 });
  
  const json = {
    "Protocol": "Exchange",
    "Url": `https://${domain}/EWS/Exchange.asmx`
  };

  return new Response(JSON.stringify(json), {
    headers: { "Content-Type": "application/json" }
  });
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
