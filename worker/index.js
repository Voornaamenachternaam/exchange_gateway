export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname.toLowerCase();

    // Autodiscover V2 (JSON) - Modern Outlook
    if (path.includes("autodiscover") && path.includes(".json")) {
      return handleAutodiscoverJson(url, env);
    }
    
    // Autodiscover V1 (XML) - Legacy/Android
    if (path.includes("autodiscover")) {
      return handleAutodiscoverXml(request, env);
    }

    // Proxy to Tunnel
    if (env.GATEWAY_SERVICE) {
      const newHeaders = new Headers(request.headers);
      newHeaders.set("X-Forwarded-Proto", "https");
      return env.GATEWAY_SERVICE.fetch(new Request(request, { headers: newHeaders }));
    }

    return new Response("Service Unavailable", { status: 503 });
  }
};

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
