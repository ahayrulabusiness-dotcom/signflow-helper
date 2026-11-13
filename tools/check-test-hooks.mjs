// Checks a built helper program for the e2e build's test hooks: the control
// files a test run drives it through (e2e-control.txt, e2e-choice.txt) and
// the WebView2 debugging port the Windows run clicks through. They exist only
// in code behind the `e2e` feature, so a release build must contain none.
// Node built-ins only.
//   node tools/check-test-hooks.mjs <program file> absent   (a release build: none may be found)
//   node tools/check-test-hooks.mjs <program file> present  (an e2e build: the control files must be found,
//                                                            which shows the search can see them)
import { readFileSync } from "node:fs";

const [file, want] = process.argv.slice(2);
if (!file || !["absent", "present"].includes(want)) {
  console.error("usage: node tools/check-test-hooks.mjs <program file> absent|present");
  process.exit(2);
}
const CONTROL_FILES = ["e2e-control.txt", "e2e-choice.txt"];
// Windows e2e builds only (#[cfg(all(feature = "e2e", windows))]).
const DEBUG_PORT = "--remote-debugging-port=9223";
const bytes = readFileSync(file);
const has = (s) => bytes.includes(Buffer.from(s, "utf8"));
const found = [...CONTROL_FILES, DEBUG_PORT].filter(has);
const name = file.split(/[\\/]/).pop();
if (want === "absent") {
  if (found.length) {
    console.log(`FAIL: ${name} (${bytes.length} bytes) carries test hooks: ${found.join(", ")}`);
    process.exit(1);
  }
  console.log(`ok: ${name} (${bytes.length} bytes) has no test hooks (looked for ${[...CONTROL_FILES, DEBUG_PORT].join(", ")})`);
} else {
  const missing = CONTROL_FILES.filter((s) => !has(s));
  if (missing.length) {
    console.log(`FAIL: ${name} (${bytes.length} bytes) is missing ${missing.join(", ")}, so this search can't be trusted`);
    process.exit(1);
  }
  console.log(`ok: ${name} (${bytes.length} bytes) is a test build: found ${found.join(", ")}`);
}
