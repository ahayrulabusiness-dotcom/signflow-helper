// A throwaway stand-in for SignFlow's helper API (:4700) and Vercel Blob's
// upload API (:4701), for the end-to-end run on a GitHub Windows runner. It
// follows the same rules as SignFlow's real routes (app/api/helper, live
// since 668a8c5): a single-use pass good for 2 minutes, an edit key, one open
// edit per file with a 15-minute lapse, the stale check and the three
// choices; and Blob's token checks (path, size, expiry, no overwrite). No
// real SignFlow, no real store, no secret: everything is made up and lives in
// memory and a scratch folder. Node built-ins only.
//
// Test-only routes under /__test make passes (as her browser would), simulate
// someone else's save, age a lock, turn an account off, and report state.

import http from "node:http";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { pipeline } from "node:stream/promises";

const API_PORT = Number(process.env.STANDIN_API_PORT || 4700);
const BLOB_PORT = Number(process.env.STANDIN_BLOB_PORT || 4701);
const DATA = path.resolve(process.env.STANDIN_DATA || "standin-data");
const BLOB_BASE = `http://127.0.0.1:${BLOB_PORT}`;
const STORE_ID = "standin";
const SECRET = crypto.randomBytes(32);
const PASS_TTL = 2 * 60_000, KEY_MAX = 12 * 3600_000, LAPSE = 15 * 60_000, MAX = 500 * 1024 * 1024;
fs.mkdirSync(path.join(DATA, "blobs"), { recursive: true });
fs.mkdirSync(path.join(DATA, "parts"), { recursive: true });

const sha256 = (s) => crypto.createHash("sha256").update(s).digest("hex");
const hmac = (s) => crypto.createHmac("sha256", SECRET).update(s).digest("base64url");
const newSecret = () => crypto.randomBytes(32).toString("base64url");
const tagOf = (storedName) => `v-${sha256(storedName).slice(0, 16)}`;
const now = () => Date.now();
const log = (...a) => console.log(new Date().toISOString(), ...a);

const users = { carlee: { name: "Carlee", active: true }, dana: { name: "Dana", active: true } };
const files = new Map(); // id → { id, jobId, name, storedName, versions: [storedName…], lastChange }
const blobs = new Map(); // pathname → { disk, size, sha256 }
const edits = []; // the FileEditKey rows
const events = [];
const blobFile = (pathname) => path.join(DATA, "blobs", sha256(pathname));
const urlOf = (pathname) => `${BLOB_BASE}/store/${pathname.split("/").map(encodeURIComponent).join("/")}`;
const pathnameOfUrl = (url) => {
  if (typeof url !== "string" || !url.startsWith(`${BLOB_BASE}/store/`)) return null;
  try { return decodeURIComponent(new URL(url).pathname.slice("/store/".length)); } catch { return null; }
};
const referenced = (url) => [...files.values()].some((f) => f.storedName === url || f.versions.includes(url));

async function hashFile(disk) {
  const h = crypto.createHash("sha256");
  await pipeline(fs.createReadStream(disk), h);
  return h.digest("hex");
}

async function putBlob(pathname, sourceStreamOrBuffer) {
  const disk = blobFile(pathname);
  if (Buffer.isBuffer(sourceStreamOrBuffer)) fs.writeFileSync(disk, sourceStreamOrBuffer);
  else await pipeline(sourceStreamOrBuffer, fs.createWriteStream(disk));
  const size = fs.statSync(disk).size;
  const b = { disk, size, sha256: await hashFile(disk) };
  blobs.set(pathname, b);
  return b;
}

function send(res, status, body) {
  const text = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
  res.end(text);
}
async function readJson(req, max = 64 * 1024) {
  let size = 0;
  const chunks = [];
  for await (const c of req) {
    size += c.length;
    if (size > max) return null;
    chunks.push(c);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}"); } catch { return null; }
}

// ------------------------------------------------------------- SignFlow API
const lockHolder = (fileId, t) => edits.find((e) => e.fileId === fileId && e.keyHash && !e.closedAt && e.keyExpiresAt > t && t - e.lastSeenAt < LAPSE);
const close = (e, reason) => { if (!e.closedAt) { e.closedAt = now(); e.closedReason = reason; events.push({ at: now(), what: `edit closed: ${reason}`, fileId: e.fileId, user: e.userId }); } };
const closedMessage = (r) => ({
  lapsed: "SignFlow didn't hear from this computer for 15 minutes, and someone else opened the file meanwhile, so this edit ended. Open the file again from SignFlow.",
  expired: "This edit has reached its 12 hours. Open the file again from SignFlow.",
  released: "Someone released this edit in SignFlow. Open the file again from SignFlow.",
  reopened: "This file was opened again in Flexi, so this edit ended.",
  "access-lost": "This edit has ended. Open the file again from SignFlow.",
}[r] || "This edit has ended. Open the file again from SignFlow.");

function withKey(req) {
  const m = /^Bearer ([A-Za-z0-9_-]{43})$/.exec(req.headers.authorization || "");
  if (!m) return { err: [401, "This edit isn't open any more. Open the file again from SignFlow."] };
  const e = edits.find((x) => x.keyHash === sha256(m[1]));
  if (!e) return { err: [401, "This edit isn't open any more. Open the file again from SignFlow."] };
  if (e.closedAt) return { err: [401, closedMessage(e.closedReason)] };
  if (e.keyExpiresAt <= now()) { close(e, "expired"); return { err: [401, closedMessage("expired")] }; }
  if (!users[e.userId].active) { close(e, "access-lost"); return { err: [403, "This SignFlow account has been turned off."] }; }
  e.lastSeenAt = now();
  return { e, file: files.get(e.fileId) };
}

const api = {
  async "exchange-pass"(req, res) {
    const body = await readJson(req);
    const pass = body?.pass;
    if (typeof pass !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(pass)) return send(res, 401, { ok: false, error: "This link isn't valid. Open the file again from SignFlow." });
    const e = edits.find((x) => x.passHash === sha256(pass));
    if (!e) return send(res, 401, { ok: false, error: "This link isn't valid. Open the file again from SignFlow." });
    if (e.passUsedAt || e.closedAt) return send(res, 401, { ok: false, error: "This link was already used. Open the file again from SignFlow." });
    if (e.passExpiresAt <= now()) return send(res, 401, { ok: false, error: "This link has run out. Open the file again from SignFlow." });
    if (!users[e.userId].active) { close(e, "access-lost"); return send(res, 403, { ok: false, error: "This SignFlow account has been turned off." }); }
    const t = now();
    const open = edits.find((x) => x !== e && x.fileId === e.fileId && x.keyHash && !x.closedAt);
    if (open) {
      if (open.keyExpiresAt <= t) close(open, "expired");
      else if (t - open.lastSeenAt >= LAPSE) close(open, "lapsed");
      else if (open.userId === e.userId) close(open, "reopened");
      else return send(res, 409, { ok: false, error: `${users[open.userId].name} is already editing this in Flexi.` });
    }
    const key = newSecret();
    const file = files.get(e.fileId);
    Object.assign(e, { passUsedAt: t, keyHash: sha256(key), keyExpiresAt: t + KEY_MAX, lastSeenAt: t, base: tagOf(file.storedName) });
    events.push({ at: t, what: "edit started", fileId: file.id, user: e.userId });
    send(res, 200, { ok: true, key, keyExpiresAt: new Date(e.keyExpiresAt).toISOString(), heartbeatSeconds: Number(process.env.STANDIN_HEARTBEAT || 60), lapseSeconds: 900, file: { id: file.id, name: file.name, sizeBytes: blobs.get(pathnameOfUrl(file.storedName)).size, version: tagOf(file.storedName) } });
  },
  async "download-link"(req, res) {
    const k = withKey(req);
    if (k.err) return send(res, k.err[0], { ok: false, error: k.err[1] });
    const pathname = pathnameOfUrl(k.file.storedName);
    const exp = now() + 5 * 60_000;
    const url = `${urlOf(pathname)}?exp=${exp}&sig=${hmac(`${pathname}|${exp}`)}`;
    k.e.base = tagOf(k.file.storedName);
    send(res, 200, { ok: true, url, expiresAt: new Date(exp).toISOString(), fileName: k.file.name, sizeBytes: blobs.get(pathname).size, version: k.e.base });
  },
  async "upload-token"(req, res) {
    const k = withKey(req);
    if (k.err) return send(res, k.err[0], { ok: false, error: k.err[1] });
    const pathname = `jobs/${k.file.jobId}/${crypto.randomUUID()}/${k.file.name}`;
    const validUntil = now() + 3600_000;
    const payload = Buffer.from(JSON.stringify({ pathname, maximumSizeInBytes: MAX, validUntil, addRandomSuffix: false, allowOverwrite: false })).toString("base64");
    const clientToken = `vercel_blob_client_${STORE_ID}_${Buffer.from(`${hmac(payload)}.${payload}`).toString("base64")}`;
    send(res, 200, { ok: true, clientToken, pathname, maximumSizeInBytes: MAX, validUntil: new Date(validUntil).toISOString() });
  },
  async replace(req, res) {
    const k = withKey(req);
    if (k.err) return send(res, k.err[0], { ok: false, error: k.err[1] });
    const body = await readJson(req);
    const choice = body?.choice ?? null;
    if (choice !== null && !["keep-theirs", "upload-mine-as-newest", "save-mine-as-copy"].includes(choice)) return send(res, 400, { ok: false, error: "Choose keep theirs, upload mine as newest, or save mine as a copy." });
    const pathname = pathnameOfUrl(body?.url);
    const blob = pathname && blobs.get(pathname);
    const file = k.file;
    if (!blob || !pathname.startsWith(`jobs/${file.jobId}/`)) return send(res, 409, { ok: false, error: "The upload couldn't be found. Please try again." });
    if (referenced(body.url) && file.storedName !== body.url) return send(res, 409, { ok: false, error: "That upload is already one of the job's files." });
    if (blob.size > MAX) return send(res, 409, { ok: false, error: "Files must be 500MB or smaller." });
    const user = users[k.e.userId].name;
    const replaceNow = () => {
      if (file.storedName === body.url) return send(res, 200, { ok: true, saved: "replaced", version: tagOf(file.storedName) });
      file.versions.unshift(file.storedName);
      file.storedName = body.url;
      file.lastChange = { by: user, at: new Date().toISOString() };
      k.e.base = tagOf(file.storedName);
      events.push({ at: now(), what: "replaced", fileId: file.id, user: k.e.userId, sha256: blob.sha256, size: blob.size });
      send(res, 200, { ok: true, saved: "replaced", version: k.e.base });
    };
    if (choice === null) {
      if (tagOf(file.storedName) !== k.e.base) {
        events.push({ at: now(), what: "stale save", fileId: file.id });
        return send(res, 409, { ok: false, error: "This file changed since you opened it", stale: { changedBy: file.lastChange?.by ?? null, changedAt: file.lastChange?.at ?? null }, choices: ["keep-theirs", "upload-mine-as-newest", "save-mine-as-copy"] });
      }
      return replaceNow();
    }
    if (choice === "keep-theirs") {
      if (!referenced(body.url)) { fs.rmSync(blob.disk, { force: true }); blobs.delete(pathname); }
      k.e.base = tagOf(file.storedName);
      events.push({ at: now(), what: "kept theirs", fileId: file.id });
      return send(res, 200, { ok: true, saved: "kept-theirs", version: k.e.base });
    }
    if (choice === "upload-mine-as-newest") return replaceNow();
    const dot = file.name.lastIndexOf(".");
    const copyName = dot > 0 ? `${file.name.slice(0, dot)} (copy)${file.name.slice(dot)}` : `${file.name} (copy)`;
    const copyId = `copy${crypto.randomBytes(6).toString("hex")}`;
    files.set(copyId, { id: copyId, jobId: file.jobId, name: copyName, storedName: body.url, versions: [], lastChange: null });
    events.push({ at: now(), what: "saved as copy", fileId: copyId, sha256: blob.sha256 });
    send(res, 200, { ok: true, saved: "copy", copyId, copyName });
  },
  async heartbeat(req, res) {
    const k = withKey(req);
    if (k.err) return send(res, k.err[0], { ok: false, error: k.err[1] });
    events.push({ at: now(), what: "heartbeat", fileId: k.file.id });
    send(res, 200, { ok: true, keyExpiresAt: new Date(k.e.keyExpiresAt).toISOString(), heartbeatSeconds: 60, lapseSeconds: 900 });
  },
  async done(req, res) {
    const m = /^Bearer ([A-Za-z0-9_-]{43})$/.exec(req.headers.authorization || "");
    const e = m && edits.find((x) => x.keyHash === sha256(m[1]));
    if (!e) return send(res, 401, { ok: false, error: "This edit isn't open any more. Open the file again from SignFlow." });
    close(e, "done");
    send(res, 200, { ok: true });
  },
};

// The test's own routes: what her browser and other people would do.
const admin = {
  async file(req, res) {
    const b = await readJson(req, 16 * 1024 * 1024);
    const id = `f${crypto.randomBytes(6).toString("hex")}`;
    const pathname = `jobs/job1/${crypto.randomUUID()}/${b.name}`;
    await putBlob(pathname, Buffer.from(b.content ?? "", "base64"));
    files.set(id, { id, jobId: "job1", name: b.name, storedName: urlOf(pathname), versions: [], lastChange: null });
    send(res, 200, { id });
  },
  async pass(req, res) {
    const b = await readJson(req);
    const file = files.get(b.fileId);
    const t = now();
    const holder = lockHolder(b.fileId, t);
    if (holder && holder.userId !== b.user) return send(res, 409, { ok: false, error: `${users[holder.userId].name} is already editing this in Flexi.` });
    const pass = newSecret();
    edits.push({ fileId: file.id, userId: b.user, passHash: sha256(pass), passExpiresAt: b.expired ? t - 1000 : t + PASS_TTL, passUsedAt: null, keyHash: null, keyExpiresAt: 0, lastSeenAt: 0, base: null, closedAt: null, closedReason: null });
    send(res, 200, { pass });
  },
  async lapse(req, res) {
    const b = await readJson(req);
    for (const e of edits) if (e.fileId === b.fileId && e.keyHash && !e.closedAt) e.lastSeenAt = now() - LAPSE - 60_000;
    send(res, 200, { ok: true });
  },
  async "replace-as"(req, res) {
    const b = await readJson(req, 16 * 1024 * 1024);
    const file = files.get(b.fileId);
    const holder = lockHolder(b.fileId, now());
    if (holder && holder.userId !== b.user) return send(res, 409, { ok: false, error: `${users[holder.userId].name} is editing this in Flexi. It can be replaced once that edit ends.` });
    const pathname = `jobs/${file.jobId}/${crypto.randomUUID()}/${file.name}`;
    const blob = await putBlob(pathname, Buffer.from(b.content, "base64"));
    file.versions.unshift(file.storedName);
    file.storedName = urlOf(pathname);
    file.lastChange = { by: users[b.user].name, at: new Date().toISOString() };
    events.push({ at: now(), what: "replaced by someone else", fileId: file.id, user: b.user, sha256: blob.sha256 });
    send(res, 200, { ok: true });
  },
  async account(req, res) {
    const b = await readJson(req);
    users[b.user].active = !!b.active;
    send(res, 200, { ok: true });
  },
  async state(_req, res) {
    send(res, 200, {
      files: [...files.values()].map((f) => {
        const b = blobs.get(pathnameOfUrl(f.storedName));
        return { id: f.id, name: f.name, version: tagOf(f.storedName), sha256: b?.sha256, size: b?.size, versions: f.versions.map((v) => blobs.get(pathnameOfUrl(v))?.sha256), lastChange: f.lastChange };
      }),
      edits: edits.map((e) => ({ fileId: e.fileId, user: e.userId, open: !!e.keyHash && !e.closedAt, closedReason: e.closedReason, base: e.base })),
      events,
    });
  },
};

http
  .createServer(async (req, res) => {
    try {
      const u = new URL(req.url, `http://127.0.0.1:${API_PORT}`);
      if (req.method === "POST" && u.pathname.startsWith("/api/helper/") && api[u.pathname.slice(12)]) return await api[u.pathname.slice(12)](req, res);
      if (u.pathname.startsWith("/__test/") && admin[u.pathname.slice(8)]) return await admin[u.pathname.slice(8)](req, res);
      send(res, 404, { ok: false, error: "not here" });
    } catch (e) {
      log("api error", e);
      send(res, 500, { ok: false, error: "stand-in error" });
    }
  })
  .listen(API_PORT, "127.0.0.1", () => log(`stand-in SignFlow on :${API_PORT}`));

// ---------------------------------------------------------- Blob stand-in
function checkToken(req, pathname) {
  const h = req.headers;
  const m = /^Bearer vercel_blob_client_([^_]+)_(.+)$/.exec(h.authorization || "");
  if (!m || m[1] !== STORE_ID) return "bad token";
  if (h["x-vercel-blob-store-id"] !== STORE_ID || h["x-api-version"] !== "12" || h["x-vercel-blob-access"] !== "private") return "bad headers";
  const inner = Buffer.from(m[2], "base64").toString("utf8");
  const dot = inner.indexOf(".");
  const [sig, payloadB64] = [inner.slice(0, dot), inner.slice(dot + 1)];
  if (hmac(payloadB64) !== sig) return "bad signature";
  const p = JSON.parse(Buffer.from(payloadB64, "base64").toString("utf8"));
  if (p.pathname !== pathname) return "wrong pathname";
  if (p.validUntil < now()) return "expired";
  return p;
}
const refuse = (res, status, message) => send(res, status, { error: { code: status === 403 ? "forbidden" : "bad_request", message } });
const uploads = new Map(); // uploadId → { pathname, parts: Map(n → disk) }

http
  .createServer(async (req, res) => {
    try {
      const u = new URL(req.url, BLOB_BASE);
      if (req.method === "GET" && u.pathname.startsWith("/store/")) {
        const pathname = decodeURIComponent(u.pathname.slice(7));
        const exp = Number(u.searchParams.get("exp"));
        if (!(exp > now()) || u.searchParams.get("sig") !== hmac(`${pathname}|${exp}`)) return refuse(res, 403, "link expired or bad");
        const b = blobs.get(pathname);
        if (!b) return refuse(res, 404, "not found");
        res.writeHead(200, { "content-type": "application/octet-stream", "content-length": b.size });
        return await pipeline(fs.createReadStream(b.disk), res);
      }
      const pathname = u.searchParams.get("pathname") || "";
      const token = checkToken(req, pathname);
      if (typeof token === "string") return refuse(res, 403, token);
      if (req.method === "PUT" && u.pathname === "/api/blob/") {
        if (blobs.has(pathname)) return refuse(res, 400, "This blob already exists");
        const declared = Number(req.headers["x-content-length"] || req.headers["content-length"] || 0);
        if (declared > token.maximumSizeInBytes) return refuse(res, 400, "File too large");
        const b = await putBlob(pathname, req);
        if (b.size > token.maximumSizeInBytes) { blobs.delete(pathname); fs.rmSync(b.disk, { force: true }); return refuse(res, 400, "File too large"); }
        events.push({ at: now(), what: "blob put", size: b.size });
        return send(res, 200, { url: urlOf(pathname), downloadUrl: urlOf(pathname), pathname, contentType: "application/octet-stream", contentDisposition: "attachment" });
      }
      if (req.method === "POST" && u.pathname === "/api/blob/mpu") {
        const action = req.headers["x-mpu-action"];
        if (action === "create") {
          if (blobs.has(pathname)) return refuse(res, 400, "This blob already exists");
          const uploadId = crypto.randomUUID();
          uploads.set(uploadId, { pathname, parts: new Map(), max: token.maximumSizeInBytes });
          return send(res, 200, { key: pathname, uploadId });
        }
        const up = uploads.get(req.headers["x-mpu-upload-id"]);
        if (!up || up.pathname !== pathname || decodeURIComponent(req.headers["x-mpu-key"] || "") !== pathname) return refuse(res, 400, "unknown upload");
        if (action === "upload") {
          const n = Number(req.headers["x-mpu-part-number"]);
          const disk = path.join(DATA, "parts", `${req.headers["x-mpu-upload-id"]}-${n}`);
          await pipeline(req, fs.createWriteStream(disk));
          up.parts.set(n, disk);
          const sent = [...up.parts.values()].reduce((a, d) => a + fs.statSync(d).size, 0);
          if (sent > up.max) return refuse(res, 400, "File too large");
          return send(res, 200, { etag: `e${n}` });
        }
        if (action === "complete") {
          const list = await readJson(req);
          const order = [...list].sort((a, b) => a.partNumber - b.partNumber);
          const disk = blobFile(pathname);
          const out = fs.createWriteStream(disk);
          for (const p of order) await pipeline(fs.createReadStream(up.parts.get(p.partNumber)), out, { end: false });
          await new Promise((r) => out.end(r));
          for (const d of up.parts.values()) fs.rmSync(d, { force: true });
          uploads.delete(req.headers["x-mpu-upload-id"]);
          const size = fs.statSync(disk).size;
          blobs.set(pathname, { disk, size, sha256: await hashFile(disk) });
          events.push({ at: now(), what: "blob multipart", size, parts: order.length });
          return send(res, 200, { url: urlOf(pathname), downloadUrl: urlOf(pathname), pathname, contentType: "application/octet-stream", contentDisposition: "attachment" });
        }
      }
      refuse(res, 400, "not understood");
    } catch (e) {
      log("blob error", e);
      if (!res.headersSent) refuse(res, 500, "stand-in error");
    }
  })
  .listen(BLOB_PORT, "127.0.0.1", () => log(`stand-in Blob on :${BLOB_PORT}`));
