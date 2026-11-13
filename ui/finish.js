// The window after Done while the file is still open in Flexi: waiting for it
// to be saved and closed (Keep waiting / Finish without saving), then "Saved
// to SignFlow" or "Nothing to upload". The helper puts its words here before
// the page loads (window.__FINISH__) and changes them with window.__render(view):
// a title, a message, and up to two buttons as [label, what it does]. Shown as
// text only; the look is the update window's (update.css).
const $ = (id) => document.getElementById(id);

function render(v) {
  v = v || {};
  $("title").textContent = v.title || "";
  document.title = v.title || "SignFlow Edit";
  $("message").textContent = v.message || "";
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
  if (v.primary) $("primary").focus();
}

window.__render = render;
render(window.__FINISH__);

for (const id of ["primary", "secondary"]) {
  $(id).addEventListener("click", (e) => {
    const choice = e.currentTarget.dataset.choice;
    if (choice) window.__TAURI__.core.invoke("finish_choice", { choice });
  });
}
// Escape is the main button (Keep waiting, or OK): never "Finish without saving".
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("primary").hidden) $("primary").click();
});
