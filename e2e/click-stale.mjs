// Clicks through the stale window like a person would: connects to the
// helper's WebView2 through its debugging port (only the e2e run turns that
// on), checks the words, and clicks one choice. Prints what it saw as JSON.
//   node e2e/click-stale.mjs <port> <choice>
import { chromium } from "playwright-core";
// Whatever happens, say it as one JSON line the run can read.
process.on("uncaughtException", (e) => { console.log(JSON.stringify({ error: String(e?.message || e).split("\n")[0] })); process.exit(0); });
process.on("unhandledRejection", (e) => { console.log(JSON.stringify({ error: String(e?.message || e).split("\n")[0] })); process.exit(0); });
const [port = "9223", choice = "upload-mine-as-newest"] = process.argv.slice(2);
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
  page = browser.contexts().flatMap((c) => c.pages()).find((p) => /stale\.html/.test(p.url()));
  if (!page) await new Promise((r) => setTimeout(r, 500));
}
if (!page) throw new Error("the stale window never opened");
await page.waitForSelector("[data-choice]");
const title = await page.locator("#title").innerText();
const lead = await page.locator("#lead").innerText();
const buttons = await page.locator("button.choice b").allInnerTexts();
// The window closes itself once answered, which can cut the click's own
// bookkeeping short; that's fine — the run checks SignFlow for the result.
let clickError = null;
await page.click(`[data-choice="${choice}"]`, { timeout: 15_000 }).catch((e) => { clickError = String(e.message || e).split("\n")[0]; });
console.log(JSON.stringify({ title, lead, buttons, clicked: choice, clickError }));
await browser.close().catch(() => {});
process.exit(0);
