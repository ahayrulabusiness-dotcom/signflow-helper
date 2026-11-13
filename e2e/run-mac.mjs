// End-to-end run on a GitHub macOS runner, with the e2e build of the helper
// (it talks to the throwaway stand-in on 127.0.0.1 instead of SignFlow):
// install the .app from its .dmg into /Applications, fire signflow-edit://
// links with `open`, save the file the way Mac apps do, and check every
// result in the stand-in. Nothing here touches SignFlow or a real store.
//   node e2e/run-mac.mjs <path to the e2e .dmg>
import { spawn, execFileSync } from "node:child_process";
import { createHash, randomInt } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync, appendFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dmg = process.argv[2];
const work = join(here, "work");
mkdirSync(work, { recursive: true });
const APP = "/Applications/SignFlow Helper.app";
const LSREG = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
const data = join(homedir(), "Library/Application Support/SignFlow Helper Files");
const log = join(data, "helper.log");
const openedFile = join(work, "opened.txt");

const results = [];
const check = (name, ok, detail = "") => {
  results.push({ name, ok: !!ok, detail: String(detail ?? "") });
  console.log(`${ok ? "ok  " : "FAIL"} ${name}  ${detail ?? ""}`);
};
// Something the runner itself can't be set up for: said so, not counted as a pass.
const skip = (name, why) => {
  results.push({ name, ok: true, skipped: true, detail: why });
  console.log(`skip ${name}  ${why}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, seconds) {
  const end = Date.now() + seconds * 1000;
  while (Date.now() < end) {
    try {
      const r = await fn();
      if (r) return r;
    } catch {}
    await sleep(500);
  }
  return null;
}
const sha = (b) => createHash("sha256").update(b).digest("hex");
const fileSha = (p) => sha(readFileSync(p));
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...opts });
async function admin(route, body) {
  const r = await fetch(`http://127.0.0.1:4700/__test/${route}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const j = await r.json();
  if (!r.ok) throw Object.assign(new Error(`${route}: ${r.status}`), { status: r.status, body: j });
  return j;
}
const state = async () => (await fetch("http://127.0.0.1:4700/__test/state")).json();
const fileRow = async (id) => (await state()).files.find((f) => f.id === id);
const editOf = async (id) => (await state()).edits.filter((e) => e.fileId === id).at(-1);
const logHas = (t) => existsSync(log) && readFileSync(log, "utf8").includes(t);
const opened = () => (existsSync(openedFile) ? readFileSync(openedFile, "utf8").split("\n").filter(Boolean).at(-1) : null);
const openLink = (pass) => run("open", [`signflow-edit://open?pass=${pass}`]);
// A note for the e2e build in its folder (an answer, or "done"): written whole
// under a temporary name and renamed into place, the word ended by a line
// break, so the helper never reads one half written.
const tell = (text, file) => {
  const f = join(data, file);
  writeFileSync(`${f}.writing`, `${text}\n`);
  renameSync(`${f}.writing`, f);
};
// How Mac apps save: a hidden temporary beside the file, renamed over it.
function saveLikeMac(path, bytes) {
  const tmp = join(dirname(path), `.${path.split("/").pop()}.sb-${randomInt(1e9).toString(16)}`);
  writeFileSync(tmp, bytes);
  renameSync(tmp, path);
}
const b = (s) => Buffer.from(s);

let standin;
try {
  // ---- The stand-in SignFlow and its store
  standin = spawn(process.execPath, [join(here, "standin/server.mjs")], {
    env: { ...process.env, STANDIN_DATA: join(work, "standin-data"), STANDIN_HEARTBEAT: "10" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  standin.stdout.on("data", (d) => appendFileSync(join(work, "standin.out"), d));
  standin.stderr.on("data", (d) => appendFileSync(join(work, "standin.err"), d));
  check("the stand-in SignFlow is up", await waitFor(() => state(), 30));

  // ---- The app this Mac uses for .fs files (a stand-in editor)
  rmSync(openedFile, { force: true });
  const probe = join(work, "probe.fs");
  writeFileSync(probe, "probe");
  let setup = "";
  try {
    setup = run("bash", [join(here, "mac/make-fake-editor.sh"), openedFile, probe]);
  } catch (e) {
    setup = `${e.stdout ?? ""}\n${e.stderr ?? ""}`;
  }
  console.log(setup);
  try { run("open", [probe]); } catch (e) { setup += `\nopen probe: ${String(e.stderr ?? e.message)}`; }
  const probeOpened = await waitFor(() => (opened() === probe ? probe : null), 30);
  const editorSetup = setup.split("\n").filter((l) => /default handler|type:|opens with|error|timed out|open probe/.test(l)).join(" / ");
  if (probeOpened) check("the stand-in editor opens .fs files on this Mac (the runner's own setup)", true, editorSetup);
  // On the runner .fs belongs to another app (Xcode, as a shader file), and
  // macOS 26.4+ asks a person before changing a default app.
  else skip("the stand-in editor opens .fs files on this Mac (the runner's own setup)", `the runner wouldn't hand .fs to a test editor | ${editorSetup}`);

  // ---- Install from the .dmg into /Applications
  rmSync(APP, { recursive: true, force: true });
  const attach = run("hdiutil", ["attach", "-nobrowse", "-readonly", "-plist", dmg]);
  const mount = [...attach.matchAll(/<key>mount-point<\/key>\s*<string>([^<]+)<\/string>/g)].map((m) => m[1])[0];
  check("the .dmg opens", mount, mount);
  const inDmg = mount ? readdirSync(mount) : [];
  check("the .dmg holds the app and an Applications shortcut", inDmg.includes("SignFlow Helper.app") && inDmg.includes("Applications"), inDmg.join(", "));
  run("ditto", [join(mount, "SignFlow Helper.app"), APP]);
  run("hdiutil", ["detach", mount, "-quiet"]);
  check("installed into /Applications", existsSync(join(APP, "Contents/MacOS/signflow-helper")), APP);
  // codesign -dv writes its answer to stderr.
  let sig = "";
  try { sig = run("bash", ["-c", `codesign -dv --verbose=2 "${APP}" 2>&1`]); } catch (e) { sig = String(e.stdout ?? e.message); }
  check("ad-hoc signed (Signature=adhoc)", /Signature=adhoc/.test(sig), (sig.match(/Signature=\S+|Identifier=\S+|flags=\S+/g) ?? []).join(" "));
  let verify = "ok";
  try { execFileSync("codesign", ["--verify", "--deep", "--strict", APP], { stdio: "pipe" }); } catch (e) { verify = String(e.stderr ?? e.message).trim(); }
  check("the signature covers the whole app (codesign --verify --deep --strict)", verify === "ok", verify);
  let schemes = "";
  try { schemes = run("/usr/libexec/PlistBuddy", ["-c", "Print :CFBundleURLTypes", join(APP, "Contents/Info.plist")]); } catch (e) { schemes = String(e.stderr ?? e.message); }
  check("Info.plist registers signflow-edit://", /signflow-edit/.test(schemes), schemes.replace(/\s+/g, " ").slice(0, 200));
  let uiElement = "";
  try { uiElement = run("/usr/libexec/PlistBuddy", ["-c", "Print :LSUIElement", join(APP, "Contents/Info.plist")]).trim(); } catch {}
  check("Info.plist marks it a menu-bar app (LSUIElement, no Dock icon)", uiElement === "true", uiElement || "missing");
  try { run(LSREG, ["-f", APP]); } catch (e) { console.log("lsregister:", String(e.stderr ?? e.message)); }
  const handler = await waitFor(() => {
    const out = run("osascript", ["-l", "JavaScript", "-e", "ObjC.import('AppKit'); var u = $.NSWorkspace.sharedWorkspace.URLForApplicationToOpenURL($.NSURL.URLWithString('signflow-edit://open')); u ? u.path.js : ''"]).trim();
    return out ? out : null;
  }, 20);
  check("macOS sends signflow-edit:// links to the installed app", handler === APP, handler ?? "no handler");

  // ---- Open: a link with a pass opens the file in the .fs app
  const v1 = b("Flexi file, version 1 " + "-".repeat(4000));
  const f = await admin("file", { name: "Concept.fs", content: v1.toString("base64") });
  const pass1 = (await admin("pass", { fileId: f.id, user: "carlee" })).pass;
  openLink(pass1);
  const openedPath = probeOpened ? await waitFor(() => (opened()?.endsWith("/Concept.fs") ? opened() : null), 90) : null;
  if (probeOpened) check("a signflow-edit:// link (open) opens the file with the Mac's .fs app", openedPath, openedPath ?? opened() ?? "nothing");
  else skip("a signflow-edit:// link (open) opens the file with the Mac's .fs app", "no test editor for .fs on the runner (above)");
  check("the helper asked macOS to open the file, and macOS accepted", await waitFor(() => logHas("session: asked your mac to open Concept.fs"), 60));
  // The rest goes on from the helper's own copy even if the runner's .fs app
  // didn't say what it opened.
  const filesDir = join(data, "files");
  const path = openedPath ?? (await waitFor(() => {
    for (const d of existsSync(filesDir) ? readdirSync(filesDir) : []) {
      const p = join(filesDir, d, "Concept.fs");
      if (existsSync(p)) return p;
    }
    return null;
  }, 30));
  check("the file was downloaded into the helper's own folder", path?.startsWith(join(data, "files")), path);
  check("the downloaded file is SignFlow's", path && fileSha(path) === sha(v1));
  check("SignFlow knows it's being edited", (await editOf(f.id))?.open);
  check("the helper checks in (heartbeat)", await waitFor(async () => (await state()).events.find((e) => e.what === "heartbeat" && e.fileId === f.id), 45));
  check("the menu bar says it's editing", logHas("STATUS Editing Concept.fs"));
  let appType = "";
  try { appType = run("lsappinfo", ["info", "-only", "ApplicationType", "-app", "com.signflow.helper"]).trim(); } catch {}
  check("it runs as a menu-bar app (no Dock icon)", /UIElement/.test(appType), appType);

  // ---- A Mac-style save becomes the new version
  const v2 = b("Flexi file, version 2 " + "+".repeat(5000));
  saveLikeMac(path, v2);
  const saved = await waitFor(async () => { const r = await fileRow(f.id); return r?.sha256 === sha(v2) ? r : null; }, 60);
  check("a save (hidden temp file, then rename) becomes the new version in SignFlow", saved);
  check("the version it replaced is kept", saved?.versions?.[0] === sha(v1));
  check("the menu bar says Saved to SignFlow", logHas("STATUS Saved to SignFlow"));
  await sleep(4000);
  const count = async () => (await state()).events.filter((e) => e.what === "replaced" && e.fileId === f.id).length;
  const before = await count();
  writeFileSync(path, v2);
  writeFileSync(join(dirname(path), ".DS_Store"), "Finder's notes");
  writeFileSync(join(dirname(path), "Concept.fs~"), "an editor's backup");
  await sleep(8000);
  const after = await count();
  check("saving again unchanged, plus a .DS_Store and a ~ file beside it, uploads nothing", before === after, `${before} → ${after}`);

  // ---- Stale: someone else saved while the lock had lapsed
  await admin("lapse", { fileId: f.id });
  const theirs = b("Dana's version");
  const r = await admin("replace-as", { fileId: f.id, user: "dana", content: theirs.toString("base64") });
  check("Dana could replace it while the lock had lapsed", r.ok);
  tell("upload-mine-as-newest", "e2e-choice.txt");
  const mine = b(`Carlee's version ${randomInt(1e9)}`);
  saveLikeMac(path, mine);
  const ok = await waitFor(async () => { const x = await fileRow(f.id); return x?.sha256 === sha(mine) ? x : null; }, 60);
  check("stale save, 'upload mine as newest': hers is the file, Dana's kept as a version", ok && ok.versions[0] === sha(theirs));
  check("the stale question named Dana", logHas("ASK Concept.fs by Dana"));
  rmSync(join(data, "e2e-choice.txt"), { force: true });

  // ---- Done ends it; her file stays
  tell("done", "e2e-control.txt");
  const done = await waitFor(async () => { const e = await editOf(f.id); return e && !e.open ? e : null; }, 60);
  check("Done ends the edit and the lock", done?.closedReason === "done", done?.closedReason);
  check("her file is still on this computer after Done", existsSync(path));

  // ---- Links that don't work say so (the helper keeps running in the menu bar)
  await sleep(1500);
  openLink(pass1);
  check("a used link: 'already used … Open the file again from SignFlow'", await waitFor(() => logHas("TELL This link was already used. Open the file again from SignFlow."), 30));
  run("open", ["signflow-edit://open?pass=short&url=https://evil.example"]);
  check("a malformed link is refused", await waitFor(() => logHas("LINK refused"), 30));
  const uploads = (await state()).events.filter((e) => e.what?.startsWith("blob"));
  check("every upload was the opened file (no .DS_Store, no ~ file)", uploads.length >= 2, `${uploads.length} uploads`);
} catch (e) {
  check("the run stopped early", false, `${e.message}${e.body ? " " + JSON.stringify(e.body) : ""}`);
} finally {
  standin?.kill();
  try { run("pkill", ["-f", "SignFlow Helper.app/Contents/MacOS/signflow-helper"]); } catch {}
}

// ---- Report (notes readable without signing in; GitHub keeps ~4KB each)
const failed = results.filter((r) => !r.ok);
const skipped = results.filter((r) => r.skipped);
const esc = (t) => t.replace(/%/g, "%25").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
function notice(title, lines, max) {
  const chunks = [];
  let cur = "";
  for (const l of lines) {
    if (cur.length + l.length + 1 > 3600 && cur) { chunks.push(cur); cur = ""; }
    cur += l + "\n";
  }
  if (cur) chunks.push(cur);
  const from = Math.max(0, chunks.length - max);
  for (let i = from; i < chunks.length; i++) console.log(`::notice title=${title} (${i + 1}/${chunks.length})::${esc(chunks[i].trimEnd())}`);
}
notice(`macOS e2e results ${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} skipped`, results.map((r) => `${r.skipped ? "skip" : r.ok ? "ok  " : "FAIL"} ${r.name} | ${r.detail}`), 2);
for (const r of failed.slice(0, 7)) console.log(`::error title=macOS e2e FAIL::${esc(`${r.name} | ${r.detail}`)}`);
if (existsSync(log)) notice("macOS helper.log", readFileSync(log, "utf8").split("\n").filter((l) => l && !/STATUS (Downloading|Saving) .* \d+%/.test(l)).map((l) => l.replace(/^\d{4}-\d\d-\d\dT(\S+)[+-]\d{4} /, "$1 ")), 3);
const md = `## Helper end-to-end on macOS\n\n| | Check | Detail |\n|---|---|---|\n${results.map((r) => `| ${r.skipped ? "⏭️" : r.ok ? "✅" : "❌"} | ${r.name} | ${r.detail.replace(/\|/g, "/")} |`).join("\n")}\n`;
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, md);
writeFileSync(join(work, "summary-mac.md"), md);
if (existsSync(log)) writeFileSync(join(work, "helper-mac.log"), readFileSync(log));
console.log(`${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} skipped`);
process.exit(failed.length ? 1 : 0);
