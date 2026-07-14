// Maelstrom all-in-one test server. One HTTP port (8900) with routes for every
// feature, plus an HTTPS port (8901) that requires a client certificate (mTLS).
//
//   node mock-server.mjs
//
import http from "node:http";
import https from "node:https";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const TOKEN_TTL_MS = 20_000; // short, so auto-refresh visibly kicks in under load
const issued = new Map(); // token -> issuedAt
const echoSeen = []; // recorded query params for the data-providers test
let counter = 0;

const json = (res, code, obj) => {
  res.writeHead(code, { "content-type": "application/json" });
  res.end(JSON.stringify(obj));
};
const readBody = (req) =>
  new Promise((r) => {
    const c = [];
    req.on("data", (x) => c.push(x));
    req.on("end", () => r(Buffer.concat(c)));
  });

function parseMultipart(buf, boundary) {
  const parts = [];
  const delim = Buffer.from("--" + boundary);
  let start = buf.indexOf(delim);
  if (start === -1) return parts;
  start += delim.length;
  while (start < buf.length && !(buf[start] === 0x2d && buf[start + 1] === 0x2d)) {
    start += 2;
    const he = buf.indexOf(Buffer.from("\r\n\r\n"), start);
    if (he === -1) break;
    const headers = buf.slice(start, he).toString("utf8");
    const bs = he + 4;
    const next = buf.indexOf(delim, bs);
    if (next === -1) break;
    const content = buf.slice(bs, next - 2);
    parts.push({
      name: (/name="([^"]*)"/i.exec(headers) || [])[1] || null,
      filename: (/filename="([^"]*)"/i.exec(headers) || [])[1] || null,
      contentType: (/content-type:\s*([^\r\n]+)/i.exec(headers) || [])[1]?.trim() || null,
      size: content.length,
    });
    start = next + delim.length;
  }
  return parts;
}

async function handler(req, res) {
  const u = new URL(req.url, "http://x");
  const p = u.pathname;

  // ---- plain API ----
  if (p === "/api/hello") return json(res, 200, { ok: true, msg: "привет от Maelstrom", ts: Date.now() });

  if (p === "/api/slow") {
    await new Promise((r) => setTimeout(r, Math.random() * 60 + 5));
    return json(res, 200, { ok: true, waited: true });
  }

  if (p === "/api/flaky") {
    await new Promise((r) => setTimeout(r, Math.random() * 20 + 3));
    if (Math.random() < 0.05) return json(res, 500, { error: "random failure" });
    return json(res, 200, { ok: true });
  }

  // ---- OAuth2: token endpoint + protected resource ----
  // Validates the client credentials so a wrong secret is rejected (401).
  // Expected: client_id = "svc", client_secret = "s3cret".
  if (p === "/oauth/token" && req.method === "POST") {
    const body = (await readBody(req)).toString("utf8");
    const form = new URLSearchParams(body);
    let id = form.get("client_id");
    let secret = form.get("client_secret");
    // credentials may instead arrive via HTTP Basic (client_auth = "basic")
    const basic = /^Basic\s+(.+)$/i.exec(req.headers.authorization || "");
    if (basic) {
      const [bId, bSecret] = Buffer.from(basic[1], "base64").toString("utf8").split(":");
      id = id || bId;
      secret = secret || bSecret;
    }
    if (id !== "svc" || secret !== "s3cret") {
      return json(res, 401, { error: "invalid_client", detail: "ожидается client_id=svc, client_secret=s3cret" });
    }
    const token = "tok_" + (++counter);
    issued.set(token, Date.now());
    return json(res, 200, { access_token: token, token_type: "Bearer", expires_in: TOKEN_TTL_MS / 1000 });
  }
  if (p === "/api/protected") {
    const tok = (req.headers.authorization || "").replace(/^Bearer\s+/i, "");
    const at = issued.get(tok);
    if (at && Date.now() - at <= TOKEN_TTL_MS + 500) return json(res, 200, { ok: true, user: "svc" });
    return json(res, 401, { error: "invalid_or_expired_token" });
  }

  // ---- multipart upload ----
  if (p === "/api/upload" && req.method === "POST") {
    const ct = req.headers["content-type"] || "";
    const m = /boundary=(.+)$/.exec(ct);
    const buf = await readBody(req);
    const parts = m ? parseMultipart(buf, m[1]) : [];
    return json(res, 200, { ok: true, total_bytes: buf.length, parts });
  }

  // ---- data providers: recorder ----
  if (p === "/api/echo") {
    echoSeen.push(Object.fromEntries(u.searchParams.entries()));
    if (echoSeen.length > 5000) echoSeen.shift();
    return json(res, 200, { ok: true });
  }
  if (p === "/api/echo/stats") {
    const keys = [...new Set(echoSeen.flatMap((o) => Object.keys(o)))];
    const distinct = {};
    for (const k of keys) {
      const vals = [...new Set(echoSeen.map((o) => o[k]).filter((v) => v != null))];
      distinct[k] = { distinct: vals.length, sample: vals.slice(0, 8) };
    }
    return json(res, 200, { count: echoSeen.length, fields: distinct, last5: echoSeen.slice(-5) });
  }

  return json(res, 404, { error: "not found", path: p });
}

http.createServer(handler).listen(8900, "127.0.0.1", () =>
  console.log("HTTP  test server → http://127.0.0.1:8900")
);

// ---- mTLS server (requires a client cert signed by our CA) ----
try {
  const opts = {
    key: fs.readFileSync(path.join(DIR, "certs", "server.key")),
    cert: fs.readFileSync(path.join(DIR, "certs", "server.pem")),
    ca: fs.readFileSync(path.join(DIR, "certs", "ca.pem")),
    requestCert: true,
    rejectUnauthorized: true,
  };
  https
    .createServer(opts, (req, res) => {
      const cert = req.socket.getPeerCertificate();
      json(res, 200, { ok: true, mtls: true, client_cn: cert?.subject?.CN || null });
    })
    .listen(8901, "127.0.0.1", () => console.log("HTTPS mTLS server → https://127.0.0.1:8901  (needs client cert)"));
} catch (e) {
  console.log("mTLS server not started (certs missing):", e.message);
}
