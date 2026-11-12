// What the helper put here before the page loaded (window.__STALE__): the
// file's name, who saved the newer one and when. Shown as text only.
const q = window.__STALE__ || {};
const who = q.changedBy || "Someone";
const file = q.fileName || "this file";
let when = "";
if (q.changedAt) {
  const d = new Date(q.changedAt);
  if (!isNaN(d)) when = " at " + d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}
if (q.changedBy) {
  document.getElementById("title").textContent = who + " saved a newer version";
  document.getElementById("lead").textContent = who + " saved a newer version of " + file + " in SignFlow" + when + ", after you opened it. What should happen to your changes?";
} else {
  // Opening a file whose copy here has changes that never reached SignFlow,
  // while SignFlow has a newer version.
  document.getElementById("title").textContent = "There's a newer version in SignFlow";
  document.getElementById("lead").textContent = "SignFlow has a newer version of " + file + ", and your copy on this computer has changes that aren't in SignFlow. What should happen to your changes?";
}
for (const b of document.querySelectorAll("[data-choice]")) {
  b.addEventListener("click", () => window.__TAURI__.core.invoke("answer_stale", { choice: b.dataset.choice }));
}
