// The update window. The helper puts its words here before the page loads
// (window.__UPDATE__) and changes them with window.__render(view): a title, a
// message, release notes, a percentage while downloading, and up to two
// buttons as [label, what it does]. Shown as text only.
const $ = (id) => document.getElementById(id);
let lastPrimary = null;

function render(v) {
  v = v || {};
  $("title").textContent = v.title || "";
  document.title = v.title || "SignFlow Edit update";
  $("message").textContent = v.message || "";
  const downloading = typeof v.percent === "number";
  $("bar").hidden = !downloading;
  if (downloading) {
    $("fill").style.width = Math.max(0, Math.min(100, v.percent)) + "%";
    $("bar").setAttribute("aria-valuenow", String(v.percent));
  }
  $("notes-box").hidden = !v.notes;
  $("notes").textContent = v.notes || "";
  for (const [id, b] of [["primary", v.primary], ["secondary", v.secondary]]) {
    const el = $(id);
    el.hidden = !b;
    if (b) {
      el.textContent = b[0];
      el.dataset.choice = b[1];
    } else {
      delete el.dataset.choice;
    }
  }
  // Focus the main button when it changes (not on every percent).
  const primary = v.primary ? v.primary[1] : null;
  if (primary && primary !== lastPrimary) $("primary").focus();
  lastPrimary = primary;
}

window.__render = render;
render(window.__UPDATE__);

for (const id of ["primary", "secondary"]) {
  $(id).addEventListener("click", (e) => {
    const choice = e.currentTarget.dataset.choice;
    if (choice) window.__TAURI__.core.invoke("update_choice", { choice });
  });
}
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("secondary").hidden) $("secondary").click();
});
