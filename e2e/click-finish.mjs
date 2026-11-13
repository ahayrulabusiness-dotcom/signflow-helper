// Reads the window after Done (finish.html) like a person would, and clicks
// one of its buttons: connects to the helper's WebView2 through its debugging
// port (only the Windows e2e build opens one). Prints what it saw as one JSON
// line.
//   node e2e/click-finish.mjs <port> <read | keep-waiting | finish-without-saving | ok> [wait for title text]
import { chromium } from "playwright-core";
process.on("uncaughtException", (e) => { console.log(JSON.stringify({ error: String(e?.message || e).split("\n")[0] })); process.exit(0); });
process.on("unhandledRejection", (e) => { console.log(JSON.stringify({ error: String(e?.message || e).split("\n")[0] })); process.exit(0); });
const [port = "9223", action = "read", waitFor = ""] = process.argv.slice(2);
const deadline = Date.now() + 90_000;
let browser;
while (!browser && Date.now() < deadline) {
  try {
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  } catch {
    await new Promise((r) => setTimeout(r, 1000));
  }
}
if (!browser) throw new Error("no WebView2 debugging port");
const read = (page) =>
  page.evaluate(() => ({
    title: document.getElementById("title").textContent,
    message: document.getElementById("message").textContent,
    buttons: [...document.querySelectorAll("button")].filter((b) => !b.hidden).map((b) => `${b.textContent}=${b.dataset.choice}`),
  }));
// The window that says `waitFor` (a closed window's page can linger a moment).
let page, seen;
while (Date.now() < deadline) {
  for (const p of browser.contexts().flatMap((c) => c.pages()).filter((p) => /finish\.html/.test(p.url()))) {
    try {
      const s = await read(p);
      if (!waitFor || s.title.includes(waitFor)) { page = p; seen = s; break; }
    } catch {}
  }
  if (page) break;
  await new Promise((r) => setTimeout(r, 300));
}
if (!page) throw new Error(`the window after Done never said "${waitFor}"`);
let clickError = null;
if (action !== "read") {
  // The window closes itself once answered; the run checks the result in the helper's log.
  await page.click(`button[data-choice="${action}"]:not([hidden])`, { timeout: 15_000 }).catch((e) => { clickError = String(e.message || e).split("\n")[0]; });
}
console.log(JSON.stringify({ ...seen, clicked: action === "read" ? null : action, clickError }));
await browser.close().catch(() => {});
process.exit(0);
