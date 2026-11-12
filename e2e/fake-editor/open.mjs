// The end-to-end run's stand-in for Flexi: registered on the runner as the
// Windows program for .fs files. When the helper opens a file, Windows starts
// this with the file's path; it writes that path down for the test and exits.
// The test then saves the file the ways Flexi might (e2e/run.ps1).
import fs from "node:fs";
import path from "node:path";
const file = process.argv[2] ?? "";
const out = process.env.SIGNFLOW_E2E_OPENED ?? path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1")), "..", "work", "opened.txt");
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.appendFileSync(out, `${file}\n`);
