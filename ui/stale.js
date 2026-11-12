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
document.getElementById("title").textContent = who + " saved a newer version";
document.getElementById("lead").textContent = who + " saved a newer version of " + file + " in SignFlow" + when + ", after you opened it. What should happen to your changes?";
for (const b of document.querySelectorAll("[data-choice]")) {
  b.addEventListener("click", () => window.__TAURI__.core.invoke("answer_stale", { choice: b.dataset.choice }));
}
