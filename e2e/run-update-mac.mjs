// End-to-end run of the UPDATE flow on a GitHub macOS runner, after
// run-mac.mjs: the e2e build in /Applications (the version being released)
// checks a local stand-in update address (:4702) that serves the next
// version, built in this run and signed with a key made for this run only.
// Every failure path first, then the real update — asked for while a file is
// open, so it must wait for Done — then the new version opens, saves and
// uploads. The window's buttons are pressed through the e2e control file (no
// driver for Mac web views); what the window says comes from the helper's log.
//   node e2e/run-update-mac.mjs <update files folder> <current version> <next version>
import { spawn, execFileSync } from "node:child_process";
import { createHash, randomInt } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync, appendFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const [filesDir, current, next] = process.argv.slice(2);
const work = join(here, "work");
mkdirSync(work, { recursive: true });
const APP = "/Applications/SignFlow Helper.app";
const LSREG = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
const data = join(homedir(), "Library/Application Support/SignFlow Helper Files");
const log = join(data, "helper.log");

const results = [];
const check = (name, ok, detail = "") => {
  results.push({ name, ok: !!ok, detail: String(detail ?? "") });
  console.log(`${ok ? "ok  " : "FAIL"} ${name}  ${detail ?? ""}`);
};
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
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...opts });
const tryRun = (cmd, args) => { try { return run(cmd, args).trim(); } catch (e) { return `${String(e.stdout ?? "").trim()} ${String(e.stderr ?? e.message).trim()}`.trim(); } };
async function admin(route, body) {
  const r = await fetch(`http://127.0.0.1:4700/__test/${route}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const j = await r.json();
  if (!r.ok) throw Object.assign(new Error(`${route}: ${r.status}`), { status: r.status, body: j });
  return j;
}
const state = async () => (await fetch("http://127.0.0.1:4700/__test/state")).json();
const fileRow = async (id) => (await state()).files.find((f) => f.id === id);
const editOf = async (id) => (await state()).edits.filter((e) => e.fileId === id).at(-1);
const mode = (m) => fetch("http://127.0.0.1:4702/__mode", { method: "POST", body: JSON.stringify({ mode: m }) });
const logText = () => (existsSync(log) ? readFileSync(log, "utf8") : "");
let mark = 0;
const markNow = () => { mark = logText().length; };
const since = () => logText().slice(mark);
const seen = (t) => since().includes(t);
const waitLog = (t, s) => waitFor(() => seen(t), s);
async function control(text) {
  const f = join(data, "e2e-control.txt");
  // Whole under a temporary name, then renamed into place, the word ended by a
  // line break: the helper never reads it half written.
  writeFileSync(`${f}.writing`, `${text}\n`);
  renameSync(`${f}.writing`, f);
  await waitFor(() => !existsSync(f), 15);
}
const versionOf = (app) => tryRun("/usr/libexec/PlistBuddy", ["-c", "Print :CFBundleShortVersionString", join(app, "Contents/Info.plist")]);
const pidOf = (app) => {
  const out = tryRun("pgrep", ["-f", `${app}/Contents/MacOS/signflow-helper`]);
  return /^\d+/.test(out) ? Number(out.split("\n")[0]) : null;
};
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
const stopHelper = async () => { tryRun("pkill", ["-f", "SignFlow Helper.app/Contents/MacOS/signflow-helper"]); await sleep(2000); };
async function startHelper(app = APP) {
  run("open", ["-n", app]);
  return waitLog("start: SignFlow Helper", 30);
}
const openLink = (pass) => run("open", [`signflow-edit://open?pass=${pass}`]);
function saveLikeMac(path, bytes) {
  const tmp = join(dirname(path), `.${path.split("/").pop()}.sb-${randomInt(1e9).toString(16)}`);
  writeFileSync(tmp, bytes);
  renameSync(tmp, path);
}
const b = (s) => Buffer.from(s);
// Closes the app this Mac opens the file with (on the runner, Xcode for .fs),
// as she closes Flexi after saving: Done waits while the file is still open in
// it. Force, and waits until it's gone.
async function quitFsApp(path) {
  const jxa = (body) => tryRun("osascript", ["-l", "JavaScript", "-e", `ObjC.import("AppKit");
var a = $.NSWorkspace.sharedWorkspace.URLForApplicationToOpenURL($.NSURL.fileURLWithPath(${JSON.stringify(path ?? "/nonexistent.fs")}));
var id = a ? ObjC.unwrap($.NSBundle.bundleWithURL(a).bundleIdentifier) : "";
var apps = id ? $.NSRunningApplication.runningApplicationsWithBundleIdentifier(id) : null;
${body}`]);
  const was = jxa('(apps && apps.count > 0 ? "running " : "not running ") + id');
  if (!was.startsWith("running ")) return was;
  jxa("for (var i = 0; i < apps.count; i++) apps.objectAtIndex(i).forceTerminate; apps.count");
  await waitFor(() => !jxa('(apps && apps.count > 0 ? "running " : "not running ") + id').startsWith("running "), 30);
  return `${was} → closed`;
}
function herFiles() {
  const all = {};
  const walk = (d) => {
    for (const n of existsSync(d) ? readdirSync(d) : []) {
      const p = join(d, n);
      if (statSync(p).isDirectory()) walk(p);
      else all[p] = sha(readFileSync(p));
    }
  };
  walk(join(data, "files"));
  return all;
}
const fileIn = (name) => {
  const filesRoot = join(data, "files");
  for (const d of existsSync(filesRoot) ? readdirSync(filesRoot) : []) {
    const p = join(filesRoot, d, name);
    if (existsSync(p)) return p;
  }
  return null;
};
const windowSays = (title, words) => seen(`UPDATE window says: ${title} | ${words}`);

let standin, updates;
const tiny = "/private/tmp/sfh-tiny";
try {
  // ---- The stand-ins: SignFlow (:4700/:4701) and the update address (:4702)
  standin = spawn(process.execPath, [join(here, "standin/server.mjs")], {
    env: { ...process.env, STANDIN_DATA: join(work, "standin-data-update"), STANDIN_HEARTBEAT: "10" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  standin.stdout.on("data", (d) => appendFileSync(join(work, "standin-update.out"), d));
  standin.stderr.on("data", (d) => appendFileSync(join(work, "standin-update.err"), d));
  updates = spawn(process.execPath, [join(here, "standin/update-server.mjs")], {
    env: { ...process.env, UPDATE_FILES: resolve(filesDir), UPDATE_VERSION: next, UPDATE_CURRENT: current, UPDATE_MODE: "none" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  updates.stdout.on("data", (d) => appendFileSync(join(work, "update-server.out"), d));
  updates.stderr.on("data", (d) => appendFileSync(join(work, "update-server.err"), d));
  check("the stand-ins are up", await waitFor(async () => (await state()) && (await fetch("http://127.0.0.1:4702/latest.json")).ok, 30));

  // ---- The installed version
  await stopHelper();
  check(`/Applications has ${current} (from the .dmg, run-mac.mjs)`, versionOf(APP) === current, versionOf(APP));
  const herBefore = herFiles();
  markNow();
  await startHelper();
  check(`it starts as ${current}`, seen(`start: SignFlow Helper ${current} (e2e build)`));
  check("it checks for an update by itself a few seconds after starting", await waitLog("UPDATE check (automatic)", 40));
  check("nothing newer: up to date, quietly (no window)", (await waitLog("UPDATE up to date", 30)) && !seen("UPDATE window shown"));
  check("the menu bar menu says so", seen(`UPDATE menu: Up to date (version ${current})`));

  // ---- A newer version: the window, once; "Not now"; the dot and the menu stay
  await mode("ok");
  markNow();
  await control("check-auto");
  check("a newer version found by the automatic check", await waitLog(`UPDATE found ${next}`, 30));
  check("…opens the update window by itself", (await waitLog("UPDATE window shown", 30)) && (await waitLog("UPDATE window open", 30)));
  check(`window: 'SignFlow Edit ${next} is available'`, await waitFor(() => windowSays(`SignFlow Edit ${next} is available`, `You have version ${current}.`), 10));
  check("menu bar: the dot is on", seen("UPDATE dot on"));
  check(`menu: 'Update to ${next}'`, seen(`UPDATE menu: Update to ${next}`));
  await control("not-now");
  check("…and the window closes", await waitLog("UPDATE window closed", 20));
  const saved = await waitFor(() => JSON.parse(readFileSync(join(data, "update.json"), "utf8")), 10);
  const hours = saved ? (saved.snoozedUntil - Date.now() / 1000) / 3600 : 0;
  check("Not now snoozes for 24 hours (update.json)", hours > 23.9 && hours <= 24, `${hours.toFixed(3)} hours`);
  markNow();
  await control("check-auto");
  await waitLog(`UPDATE found ${next}`, 30);
  check("snoozed: the next automatic check shows no window", (await waitLog("no window (she said Not now)", 15)) && !seen("UPDATE window shown"));
  check("…and the dot stays on", !seen("UPDATE dot off"));

  // ---- Every failure says what happened and leaves the installed app working
  let pid0 = pidOf(APP);
  for (const c of [
    { mode: "tampered", failure: "bad signature", words: "The update didn't pass its safety check" },
    { mode: "cut", failure: "interrupted", words: "The download stopped before it finished." },
    { mode: "dead", failure: "no signal", words: "Couldn't reach the internet" },
    { mode: "garbage", failure: "install failed", words: "The update couldn't be installed." },
  ]) {
    await mode(c.mode);
    markNow();
    await control("check");
    await waitLog(`UPDATE found ${next}`, 30);
    await control("update");
    const failed = await waitLog(`UPDATE failed (${c.failure})`, 120);
    const said = await waitFor(() => windowSays(`SignFlow Edit ${next} wasn't installed`, c.words) && seen(`Nothing was changed: SignFlow Edit ${current} still works.`), 20);
    check(`${c.mode}: '${c.failure}', said in the window`, failed && said, since().split("\n").filter((l) => /UPDATE (failed|window says)/.test(l)).slice(-2).join(" / "));
    check(`${c.mode}: ${current} still installed and running (same process)`, pidOf(APP) === pid0 && alive(pid0) && versionOf(APP) === current, `pid ${pidOf(APP)} vs ${pid0}, ${versionOf(APP)}`);
  }
  const s2 = JSON.parse(readFileSync(join(data, "update.json"), "utf8"));
  check("no failed try left an install half-recorded", !s2.installing);

  // ---- Disk full: the app on a tiny disk that's full (the swap works beside the app)
  let made = false;
  try {
    rmSync(join(work, "tiny.dmg"), { force: true });
    tryRun("hdiutil", ["detach", tiny, "-force"]);
    run("hdiutil", ["create", "-size", "64m", "-fs", "APFS", "-volname", "SFHTiny", join(work, "tiny.dmg")]);
    mkdirSync(tiny, { recursive: true });
    run("hdiutil", ["attach", "-nobrowse", "-mountpoint", tiny, join(work, "tiny.dmg")]);
    run("ditto", [APP, join(tiny, "SignFlow Helper.app")]);
    const availKb = Number(run("df", ["-k", tiny]).trim().split("\n").pop().split(/\s+/)[3]);
    const fillMb = Math.max(0, Math.floor(availKb / 1024) - 2);
    run("dd", ["if=/dev/zero", `of=${join(tiny, "fill.bin")}`, "bs=1m", `count=${fillMb}`]);
    made = true;
  } catch (e) {
    skip("disk full: the update says so and the installed app keeps working", `couldn't make a tiny disk on the runner: ${String(e.stderr ?? e.message).trim().slice(0, 300)}`);
  }
  if (made) {
    const tinyApp = join(tiny, "SignFlow Helper.app");
    await stopHelper();
    markNow();
    await startHelper(tinyApp);
    const pidT = pidOf(tinyApp);
    await mode("ok");
    await control("check");
    await waitLog(`UPDATE found ${next}`, 30);
    await control("update");
    const failed = await waitLog("UPDATE failed (disk full)", 120);
    const said = await waitFor(() => windowSays(`SignFlow Edit ${next} wasn't installed`, "There isn't enough free space on this computer for the update."), 20);
    check("disk full: says there isn't enough free space", failed && said, `${run("df", ["-h", tiny]).trim().split("\n").pop()} / ${since().split("\n").filter((l) => /UPDATE failed/.test(l)).slice(-1)[0] ?? ""}`);
    check("disk full: the app is untouched and still running", pidT && alive(pidT) && versionOf(tinyApp) === current && tryRun("codesign", ["--verify", "--deep", "--strict", tinyApp]) === "");
    await stopHelper();
    tryRun("hdiutil", ["detach", tiny, "-force"]);
    tryRun(LSREG, ["-f", APP]);
  }

  // ---- The update itself, asked for while a file is open: it waits for Done
  await stopHelper();
  await mode("ok");
  markNow();
  await startHelper();
  await waitLog(`UPDATE found ${next}`, 40);
  const uf = await admin("file", { name: "Update.fs", content: b("Update file, version 1").toString("base64") });
  openLink((await admin("pass", { fileId: uf.id, user: "carlee" })).pass);
  const uOpen = await waitFor(async () => (await editOf(uf.id))?.open, 60);
  const uPath = await waitFor(() => fileIn("Update.fs"), 30);
  check("a file is open for editing", uOpen && uPath, uPath);
  await control("check");
  check("while editing, 'Check for updates now' still shows the window", await waitLog("UPDATE window shown", 30));
  pid0 = pidOf(APP);
  await control("update");
  check("it downloads, with progress in the menu", (await waitLog(`UPDATE downloading ${next}`, 20)) && (await waitLog(`UPDATE menu: Downloading ${next}`, 60)));
  check("the download's signature checks out (this run's key)", await waitLog("the signature checks out", 120));
  check("she's editing, so it waits", await waitLog("UPDATE waiting for the edit to end before installing", 20));
  check("window: 'Finish your edit first'", await waitFor(() => windowSays("Finish your edit first", `SignFlow Edit ${next} is downloaded and checked.`), 10));
  await sleep(12000);
  check(`never installs while she's editing (${current}, same process, 12 seconds on)`, pidOf(APP) === pid0 && versionOf(APP) === current && !seen("UPDATE installing"));
  const mineU = b(`Update file, saved while the update waits ${randomInt(1e9)}`);
  if (uPath) saveLikeMac(uPath, mineU);
  check("editing goes on as normal: a save reaches SignFlow while the update waits", await waitFor(async () => (await fileRow(uf.id))?.sha256 === sha(mineU), 60));
  console.log("closed the .fs app:", await quitFsApp(uPath));
  await control("done");
  check("Done ends the edit", await waitFor(async () => (await editOf(uf.id))?.open === false, 60));
  check("…then it installs", (await waitLog("UPDATE the edit ended; installing now", 30)) && (await waitLog(`UPDATE installing ${next} (from ${current})`, 30)));
  const installed = await waitLog(`UPDATE installed ${next}; starting again`, 90);
  if (!installed) {
    const prompts = [tryRun("pgrep", ["-lx", "osascript"]), tryRun("pgrep", ["-lx", "SecurityAgent"])].filter(Boolean).join(" ");
    check(`the app was swapped for ${next}`, false, `no swap within 90s; a password prompt would mean the swap was refused (permission or App Management): ${prompts || "no prompt processes"} / ${since().split("\n").filter((l) => /UPDATE/.test(l)).slice(-3).join(" / ")}`);
  } else {
    check(`the app was swapped for ${next} (no password asked)`, true);
  }
  check("the old helper exits", await waitFor(() => !alive(pid0), 60));
  const back = await waitFor(() => { const p = pidOf(APP); return p && p !== pid0 ? p : null; }, 60);
  check("it starts again by itself", back, `pid ${back}`);
  check(`it is ${next} now`, (await waitLog(`start: SignFlow Helper ${next} (e2e build)`, 60)) && versionOf(APP) === next, versionOf(APP));
  check("it knows it was updated", (await waitLog(`UPDATE updated from ${current} to ${next}`, 30)) && seen(`UPDATE menu: Updated to version ${next}`));
  let sig = "";
  try { sig = run("bash", ["-c", `codesign -dv --verbose=2 "${APP}" 2>&1`]); } catch (e) { sig = String(e.stdout ?? e.message); }
  const verify = tryRun("codesign", ["--verify", "--deep", "--strict", APP]);
  check("the new app is ad-hoc signed and its signature covers the whole app", /Signature=adhoc/.test(sig) && verify === "", `${(sig.match(/Signature=\S+|Identifier=\S+/g) ?? []).join(" ")} ${verify}`);
  const attrs = tryRun("xattr", [APP]);
  check("no quarantine mark on the new app (so no Gatekeeper prompt is expected)", !/com\.apple\.quarantine/.test(attrs), attrs || "(no extended attributes)");
  const apps = readdirSync("/Applications").filter((n) => n.startsWith("SignFlow Helper"));
  check("one app in Applications (no old copy left behind)", apps.length === 1, apps.join(", "));
  const handler = tryRun("osascript", ["-l", "JavaScript", "-e", "ObjC.import('AppKit'); var u = $.NSWorkspace.sharedWorkspace.URLForApplicationToOpenURL($.NSURL.URLWithString('signflow-edit://open')); u ? u.path.js : ''"]);
  check("signflow-edit:// links still go to the app in Applications", handler === APP, handler);
  await sleep(15000);
  check("no 'link already used' message after the restart", !seen("TELL This link was already used"));
  check(`its own first check: nothing newer than ${next}`, await waitLog("UPDATE up to date", 30));
  const s3 = JSON.parse(readFileSync(join(data, "update.json"), "utf8"));
  check("the install record is cleared", !s3.installing);

  // ---- And it still works: open, save, upload on the new version
  const af = await admin("file", { name: "After.fs", content: b("After the update, version 1").toString("base64") });
  openLink((await admin("pass", { fileId: af.id, user: "carlee" })).pass);
  const aOpen = await waitFor(async () => (await editOf(af.id))?.open, 60);
  const aPath = await waitFor(() => fileIn("After.fs"), 30);
  check(`${next}: a link opens the file`, aOpen && aPath, aPath);
  const mineA = b(`After the update, saved ${randomInt(1e9)}`);
  const afterMark = logText().length;
  if (aPath) saveLikeMac(aPath, mineA);
  const uploadedA = await waitFor(async () => (await fileRow(af.id))?.sha256 === sha(mineA), 90);
  // If not, say why: what the helper logged about it, and who holds the file open.
  const why = uploadedA ? "" : `${logText().slice(afterMark).split("\n").filter((l) => /After\.fs|save|STATUS|watch|session/.test(l)).slice(-10).join(" / ")} / lsof: ${aPath ? tryRun("/usr/sbin/lsof", ["--", aPath]).replace(/\s+/g, " ").slice(0, 400) : "-"}`;
  check(`${next}: a save is uploaded as the new version`, uploadedA, why);
  console.log("closed the .fs app:", await quitFsApp(aPath));
  await control("done");
  check(`${next}: Done ends the edit`, await waitFor(async () => (await editOf(af.id))?.open === false, 60));

  // ---- Her files
  const herAfter = herFiles();
  const changed = Object.keys(herBefore).filter((p) => herAfter[p] !== herBefore[p]);
  check(`every file she had is still there, unchanged (${Object.keys(herBefore).length} files)`, Object.keys(herBefore).length > 0 && changed.length === 0, changed.join(", "));
} catch (e) {
  check("the run stopped early", false, `${e.message}${e.body ? " " + JSON.stringify(e.body) : ""}`);
} finally {
  standin?.kill();
  updates?.kill();
  tryRun("pkill", ["-f", "SignFlow Helper.app/Contents/MacOS/signflow-helper"]);
  tryRun("hdiutil", ["detach", tiny, "-force"]);
}

// ---- Report
const failed = results.filter((r) => !r.ok);
const skipped = results.filter((r) => r.skipped);
const passed = results.length - failed.length - skipped.length;
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
notice(`macOS update e2e ${passed} passed, ${failed.length} failed, ${skipped.length} skipped`, results.map((r) => `${r.skipped ? "skip" : r.ok ? "ok  " : "FAIL"} ${r.name} | ${r.detail}`), 3);
for (const r of failed.slice(0, 7)) console.log(`::error title=macOS update e2e FAIL::${esc(`${r.name} | ${r.detail}`)}`);
notice("macOS helper.log (updates)", logText().split("\n").filter((l) => /UPDATE|start:|TELL|LINK|SESSION|e2e:/.test(l) && !/UPDATE menu: Downloading .* [1-9]\d?%/.test(l)).map((l) => l.replace(/^\d{4}-\d\d-\d\dT(\S+)[+-]\d{4} /, "$1 ")).slice(-160), 4);
const md = `## Helper update end-to-end on macOS\n\n| | Check | Detail |\n|---|---|---|\n${results.map((r) => `| ${r.skipped ? "⏭️" : r.ok ? "✅" : "❌"} | ${r.name} | ${r.detail.replace(/\|/g, "/")} |`).join("\n")}\n`;
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, md);
writeFileSync(join(work, "summary-update-mac.md"), md);
if (existsSync(log)) writeFileSync(join(work, "helper-update-mac.log"), readFileSync(log));
console.log(`${passed} passed, ${failed.length} failed, ${skipped.length} skipped`);
process.exit(failed.length ? 1 : 0);
