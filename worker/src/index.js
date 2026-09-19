// RRFlux edge translator — Cloudflare Worker (free tier, no Blaze needed).
//
// The Nov 2022 game client only speaks Rec Room's HTTP API
// (/Account/LoginWithToken, /api/*). Firebase speaks Firebase. Photon does
// multiplayer but not login. This Worker is the tiny translator between them:
//   - verifies Firebase ID tokens using Google's public certs (Web Crypto)
//   - serves version/config/player/sanitize endpoints
//   - sinks telemetry, graceful JSON 404s for everything else
//
// Stateless MVP: player data derives from token claims. Firestore REST
// integration lands after traffic capture proves the real schemas.

const PROJECT_ID = "flux-544a6";
const ISSUER = `https://securetoken.google.com/${PROJECT_ID}`;
const JWKS_URL =
  "https://www.googleapis.com/service_accounts/v1/jwk/securetoken@system.gserviceaccount.com";

let jwksCache = null; // { keys, expiresAt } — reused across warm invocations

function b64urlDecode(s) {
  s = s.replace(/-/g, "+").replace(/_/g, "/");
  while (s.length % 4) s += "=";
  return Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
}

async function getJwks() {
  const now = Date.now();
  if (jwksCache && jwksCache.expiresAt > now) return jwksCache.keys;
  const res = await fetch(JWKS_URL);
  if (!res.ok) throw new Error("jwks fetch failed");
  const maxAge = /max-age=(\d+)/.exec(res.headers.get("cache-control") || "");
  const ttl = maxAge ? parseInt(maxAge[1], 10) * 1000 : 3600_000;
  const { keys } = await res.json();
  jwksCache = { keys, expiresAt: now + ttl };
  return keys;
}

// Verify a Firebase ID token per Firebase docs (RS256, aud/iss/exp checks).
async function verifyIdToken(token) {
  const parts = token.split(".");
  if (parts.length !== 3) throw new Error("bad token shape");
  const [hB64, pB64, sB64] = parts;
  const header = JSON.parse(new TextDecoder().decode(b64urlDecode(hB64)));
  const payload = JSON.parse(new TextDecoder().decode(b64urlDecode(pB64)));

  const keys = await getJwks();
  const jwk = keys.find((k) => k.kid === header.kid);
  if (!jwk) throw new Error("unknown kid");

  const key = await crypto.subtle.importKey(
    "jwk",
    jwk,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["verify"]
  );
  const data = new TextEncoder().encode(`${hB64}.${pB64}`);
  const ok = await crypto.subtle.verify(
    "RSASSA-PKCS1-v1_5",
    key,
    b64urlDecode(sB64),
    data
  );
  if (!ok) throw new Error("bad signature");

  const now = Math.floor(Date.now() / 1000);
  if (payload.aud !== PROJECT_ID) throw new Error("bad aud");
  if (payload.iss !== ISSUER) throw new Error("bad iss");
  if (!payload.sub || typeof payload.sub !== "string") throw new Error("bad sub");
  if (payload.exp <= now) throw new Error("expired");
  if (payload.iat > now + 300) throw new Error("bad iat");
  return payload;
}

function usernameFromClaims(c) {
  return c.name || (c.email || "").split("@")[0] || `player-${c.sub.slice(0, 6)}`;
}

async function bearerClaims(req) {
  const m = /^Bearer (.+)$/.exec(req.headers.get("Authorization") || "");
  if (!m) return null;
  try {
    return await verifyIdToken(m[1]);
  } catch {
    return null;
  }
}

const json = (obj, status = 200) =>
  new Response(JSON.stringify(obj), {
    status,
    headers: {
      "content-type": "application/json",
      "access-control-allow-origin": "*",
    },
  });

export default {
  async fetch(req) {
    const url = new URL(req.url);
    const path = url.pathname;

    if (req.method === "OPTIONS") {
      return new Response(null, {
        status: 204,
        headers: {
          "access-control-allow-origin": "*",
          "access-control-allow-methods": "GET, POST, OPTIONS",
          "access-control-allow-headers": "Content-Type, Authorization",
        },
      });
    }

    // ---- game auth: the client calls this on launch ----
    if (path === "/Account/LoginWithToken") {
      const loginToken = url.searchParams.get("loginToken");
      if (!loginToken) return json({ error: "loginToken required" }, 401);
      let claims;
      try {
        claims = await verifyIdToken(loginToken);
      } catch {
        return json({ error: "invalid loginToken" }, 401);
      }
      return json({
        playerId: claims.sub,
        username: usernameFromClaims(claims),
        authToken: loginToken, // the verified ID token doubles as session token
        expiresIn: 3600,
      });
    }

    // ---- version check: tell the client it's up to date ----
    if (path.startsWith("/api/versioncheck/")) {
      return json({ ok: true, updateRequired: false });
    }

    // ---- remote config: permissive defaults ----
    if (path.startsWith("/api/config/")) {
      return json({});
    }

    // ---- player profile (stateless MVP: derived from token claims) ----
    if (path === "/api/players/v2/me") {
      const claims = await bearerClaims(req);
      if (!claims) return json({ error: "auth required" }, 401);
      return json({ playerId: claims.sub, username: usernameFromClaims(claims) });
    }
    if (/^\/api\/players\/v2\/[^/]+$/.test(path)) {
      return json({ error: "unknown player" }, 404);
    }

    // ---- text sanitization: private server, pass through ----
    if (path.startsWith("/api/sanitize/")) {
      let text = url.searchParams.get("text") || "";
      if (req.method === "POST") {
        try {
          text = (await req.json()).text || "";
        } catch {
          /* keep query/default */
        }
      }
      return json({ text });
    }

    // ---- telemetry sink (Amplitude host points here) ----
    if (path.startsWith("/2/httpapi") || path.startsWith("/telemetry")) {
      return json({ ok: true });
    }

    // ---- launcher endpoints ----
    if (path === "/v1/manifest/version") return json({ version: "0.1.0" });
    if (path === "/v1/manifest") return json({ error: "no manifest published" }, 404);
    if (path.startsWith("/v1/")) return json({ error: "unknown endpoint" }, 404);

    // ---- graceful 404 for the rest of the game surface (friends, rooms,
    // store, images, …). Implemented as the client proves it needs them. ----
    if (path.startsWith("/api/")) {
      return json({ error: "not implemented in RRFlux v1" }, 404);
    }

    return json({ error: "not found" }, 404);
  },
};
