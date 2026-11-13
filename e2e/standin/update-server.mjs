// A throwaway stand-in for the helper's update address on GitHub (:4702), for
// the end-to-end runs: latest.json and the files it names. Its mode makes it
// misbehave on purpose. Node built-ins only; nothing here is a secret (the
// files are signed with a key made for this run and thrown away).
//
//   UPDATE_FILES   folder with the next version's update file and its .sig
//                  (Windows: *_x64-setup.exe; macOS: *.app.tar.gz), plus
//                  not-an-installer.bin and its .sig
//   UPDATE_VERSION the next version (e.g. 0.1.4); UPDATE_CURRENT the installed one
//
// Modes (POST /__mode {"mode": …}):
//   none      latest.json names the installed version: nothing to update
//   ok        the real signed update (sent at about 2MB/s, so progress shows)
//   tampered  the same, with one byte changed (the signature no longer fits)
//   cut       half the file, then the connection drops
//   dead      latest.json points at a port nothing listens on
//   garbage   a file signed properly for the version, but not an installer
import http from "node:http";
import fs from "node:fs";
import path from "node:path";

const PORT = Number(process.env.UPDATE_PORT || 4702);
const DIR = path.resolve(process.env.UPDATE_FILES || "update-files");
const NEXT = process.env.UPDATE_VERSION;
const CURRENT = process.env.UPDATE_CURRENT;
const BASE = `http://127.0.0.1:${PORT}`;
let mode = process.env.UPDATE_MODE || "none";
const requests = [];
const log = (...a) => console.log(new Date().toISOString(), ...a);

const update = fs.readdirSync(DIR).find((f) => /_x64-setup\.exe$/.test(f) || /\.app\.tar\.gz$/.test(f));
if (!update || !NEXT || !CURRENT) throw new Error(`need UPDATE_FILES with an update file, UPDATE_VERSION and UPDATE_CURRENT (found ${update})`);
const mac = update.endsWith(".app.tar.gz");
const sig = (f) => fs.readFileSync(path.join(DIR, `${f}.sig`), "utf8").trim();

function manifest() {
  const file = mode === "garbage" ? "not-an-installer.bin" : update;
  const url = mode === "dead" ? `http://127.0.0.1:4709/files/${encodeURIComponent(file)}` : `${BASE}/files/${encodeURIComponent(file)}`;
  const entry = { signature: sig(file), url };
  const platforms = mac
    ? { "darwin-aarch64-app": entry, "darwin-aarch64": entry, "darwin-x86_64-app": entry, "darwin-x86_64": entry }
    : { "windows-x86_64-nsis": entry, "windows-x86_64": entry };
  return {
    version: mode === "none" ? CURRENT : NEXT,
    notes: `Test notes for ${NEXT}: saving to SignFlow is quicker.\nNothing else changes.`,
    pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    platforms,
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function sendFile(req, res, name) {
  const disk = path.join(DIR, path.basename(name));
  if (!fs.existsSync(disk)) {
    res.writeHead(404, { "content-type": "text/plain" });
    return res.end("Not Found");
  }
  let bytes = fs.readFileSync(disk);
  if (mode === "tampered") {
    bytes = Buffer.from(bytes);
    bytes[Math.floor(bytes.length / 2)] ^= 0xff;
  }
  res.writeHead(200, { "content-type": "application/octet-stream", "content-length": bytes.length });
  const stop = mode === "cut" ? Math.floor(bytes.length / 2) : bytes.length;
  for (let at = 0; at < stop; at += 64 * 1024) {
    const end = Math.min(stop, at + 64 * 1024);
    if (!res.write(bytes.subarray(at, end))) await new Promise((r) => res.once("drain", r));
    await sleep(30);
  }
  if (mode === "cut") {
    log("cut the download at", stop, "of", bytes.length);
    req.socket.destroy();
    return;
  }
  res.end();
}

http
  .createServer(async (req, res) => {
    const u = new URL(req.url, BASE);
    requests.push({ at: Date.now(), method: req.method, path: u.pathname, mode });
    try {
      if (req.method === "POST" && u.pathname === "/__mode") {
        let body = "";
        for await (const c of req) body += c;
        mode = JSON.parse(body || "{}").mode || mode;
        log("mode", mode);
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ mode }));
      }
      if (u.pathname === "/__requests") {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify(requests));
      }
      if (u.pathname === "/latest.json") {
        res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" });
        return res.end(JSON.stringify(manifest()));
      }
      if (u.pathname.startsWith("/files/")) return await sendFile(req, res, decodeURIComponent(u.pathname.slice("/files/".length)));
      res.writeHead(404);
      res.end();
    } catch (e) {
      log("error", e.message);
      try { res.writeHead(500); res.end(); } catch {}
    }
  })
  .listen(PORT, "127.0.0.1", () => log(`update stand-in on ${BASE}, ${mac ? "macOS" : "Windows"} file ${update}, ${CURRENT} → ${NEXT}, mode ${mode}`));
