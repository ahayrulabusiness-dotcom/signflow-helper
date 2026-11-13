// Reads the update window like a person would, and clicks one of its buttons:
// connects to the helper's WebView2 through its debugging port (only the
// Windows e2e build opens one). Prints what it saw as one JSON line.
//   node e2e/click-update.mjs <port> <read | update | later | ok | retry | close | hide> [wait for title text]
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
let page;
while (!page && Date.now() < deadline) {
  page = browser.contexts().flatMap((c) => c.pages()).find((p) => /update\.html/.test(p.url()));
  if (!page) await new Promise((r) => setTimeout(r, 500));
}
if (!page) throw new Error("the update window never opened");
const read = () =>
  page.evaluate(() => ({
    title: document.getElementById("title").textContent,
    message: document.getElementById("message").textContent,
    notes: document.getElementById("notes-box").hidden ? "" : document.getElementById("notes").textContent,
    percent: document.getElementById("bar").hidden ? null : Number(document.getElementById("bar").getAttribute("aria-valuenow")),
    buttons: [...document.querySelectorAll("button")].filter((b) => !b.hidden).map((b) => `${b.textContent}=${b.dataset.choice}`),
  }));
let seen = await read();
while (waitFor && !seen.title.includes(waitFor) && Date.now() < deadline) {
  await new Promise((r) => setTimeout(r, 300));
  seen = await read();
}
let clickError = null;
if (action !== "read") {
  // The window may close itself once answered; the run checks the result in the helper's log.
  await page.click(`button[data-choice="${action}"]:not([hidden])`, { timeout: 15_000 }).catch((e) => { clickError = String(e.message || e).split("\n")[0]; });
}
console.log(JSON.stringify({ ...seen, clicked: action === "read" ? null : action, clickError }));
await browser.close().catch(() => {});
process.exit(0);
