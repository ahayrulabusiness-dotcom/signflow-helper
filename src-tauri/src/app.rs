//! The tray app around a session: answers `signflow-edit://open?pass=…`
//! links (link.rs), runs one edit at a time (session.rs), shows its status in
//! the tray and asks the stale question in a small window. It holds no secret
//! and no storage key; SignFlow's address is built in (config.rs).
//!
//! It also keeps itself up to date (update.rs has the rules): it checks on
//! start and every six hours, says so with a dot on the tray picture, an
//! "Update to …" line in the menu and — once, unless she said "Not now" — a
//! small window; it installs only when she says so, and never while a file is
//! open for editing.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::{mpsc, oneshot};

use crate::api::{self, Choice};
use crate::session::{self, Control, Question, Ui};
use crate::update::{self, Phase};
use crate::{blob, config, link, logfile, watcher};

const TRAY_ID: &str = "signflow-helper";
const UPDATE_WINDOW: &str = "update";

/// The Windows e2e build opens WebView2's debugging port so the run can click
/// the helper's windows. Every window must ask for the same arguments: they
/// share one WebView2 browser (wry's own defaults kept alongside).
#[cfg(all(feature = "e2e", windows))]
const E2E_WEBVIEW_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port=9223";

struct Shared {
    /// The open edit's control line, if an edit is open.
    current: Mutex<Option<mpsc::Sender<Control>>>,
    /// The stale window that's waiting for an answer: its label and where the
    /// answer goes. Only that window's button (or closing it) answers.
    answer: Mutex<Option<(String, oneshot::Sender<Option<Choice>>)>>,
    asks: AtomicU64,
    /// Passes acted on in the last few seconds: the same link can arrive
    /// twice from one click. Clicked again later, a used pass goes to
    /// SignFlow, which says it was already used.
    seen: Mutex<HashMap<String, Instant>>,
    quit_after_done: Mutex<bool>,
    ctx: Arc<session::Ctx>,
    items: Option<Items>,
    /// This helper's version.
    version: String,
    /// Where updates stand (update::Phase).
    update_phase: Mutex<Phase>,
    /// The update the last check found.
    update_found: Mutex<Option<tauri_plugin_updater::Update>>,
    /// Its file, downloaded and checked, waiting for the edit to end.
    update_ready: Mutex<Option<Vec<u8>>>,
    /// Set — with `current` held and no edit open — just before installing:
    /// a link arriving now is told to try again in a minute.
    installing: AtomicBool,
    /// The window's last words in the log (so the log isn't one line per %).
    update_said: Mutex<String>,
}

struct Items {
    status: MenuItem<Wry>,
    open: MenuItem<Wry>,
    folder: MenuItem<Wry>,
    done: MenuItem<Wry>,
    update: MenuItem<Wry>,
    check: MenuItem<Wry>,
}

/// Menu text treats `&` as a shortcut marker.
fn menu_text(s: &str) -> String {
    s.replace('&', "&&")
}

/// The tray app's side of the session's Ui.
struct TrayUi {
    app: AppHandle,
}

impl Ui for TrayUi {
    fn status(&self, text: &str) {
        logfile::info(&format!("STATUS {text}"));
        if let Some(shared) = self.app.try_state::<Shared>() {
            if let Some(items) = &shared.items {
                let _ = items.status.set_text(menu_text(text));
            }
        }
        if let Some(tray) = self.app.tray_by_id(TRAY_ID) {
            // Windows cuts tooltips at 128 characters.
            let tip: String = format!("SignFlow Helper — {text}").chars().take(120).collect();
            let _ = tray.set_tooltip(Some(tip));
        }
    }

    fn tell(&self, text: &str) {
        logfile::info(&format!("TELL {text}"));
        if cfg!(feature = "e2e") {
            return;
        }
        let app = self.app.clone();
        let text = text.to_string();
        // Never from inside an event handler on the main thread.
        std::thread::spawn(move || {
            app.dialog().message(text).title("SignFlow Helper").kind(MessageDialogKind::Info).show(|_| {});
        });
    }

    fn ask(&self, q: Question) -> oneshot::Receiver<Option<Choice>> {
        logfile::info(&format!("ASK {} by {}", q.file_name, q.changed_by.clone().unwrap_or_else(|| "someone".into())));
        let (tx, rx) = oneshot::channel();
        #[cfg(feature = "e2e")]
        {
            // The e2e run answers through a file; one part of it leaves the
            // file out and clicks the real window instead.
            let f = config::home_dir().join("e2e-choice.txt");
            match std::fs::read(&f) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let text = text.trim_start_matches('\u{feff}').trim();
                    logfile::info(&format!("ASK answered by the test: {text}"));
                    let _ = tx.send(Choice::parse(text));
                    return rx;
                }
                Err(e) => logfile::info(&format!("ASK: no test answer at {} ({e}); showing the window", f.display())),
            }
        }
        let shared = self.app.state::<Shared>();
        let label = format!("stale-{}", shared.asks.fetch_add(1, Ordering::SeqCst));
        if let Some((old_label, old_tx)) = shared.answer.lock().unwrap().replace((label.clone(), tx)) {
            let _ = old_tx.send(None);
            if let Some(w) = self.app.get_webview_window(&old_label) {
                let _ = w.close();
            }
        }
        let data = serde_json::json!({ "fileName": q.file_name, "changedBy": q.changed_by, "changedAt": q.changed_at });
        let app = self.app.clone();
        // Built off the main thread (building a window inside an event
        // handler can deadlock on Windows).
        std::thread::spawn(move || {
            let builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("stale.html".into()))
                .title("SignFlow Helper")
                .inner_size(460.0, 400.0)
                .resizable(false)
                .minimizable(false)
                .maximizable(false)
                .always_on_top(true)
                .center()
                .focused(true)
                .initialization_script(format!("window.__STALE__ = {data};"));
            #[cfg(all(feature = "e2e", windows))]
            let builder = builder.additional_browser_args(E2E_WEBVIEW_ARGS);
            let built = builder.build();
            match built {
                Ok(window) => {
                    logfile::info(&format!("stale window shown ({label})"));
                    #[cfg(feature = "e2e")]
                    {
                        // The e2e run may still answer through the file (if
                        // its click didn't land), so a test can't hang here.
                        let app3 = app.clone();
                        let mine = label.clone();
                        std::thread::spawn(move || {
                            let f = config::home_dir().join("e2e-choice.txt");
                            for _ in 0..1200 {
                                std::thread::sleep(Duration::from_millis(500));
                                let waiting = app3.state::<Shared>().answer.lock().unwrap().as_ref().map(|(l, _)| l == &mine).unwrap_or(false);
                                if !waiting {
                                    return;
                                }
                                if let Ok(bytes) = std::fs::read(&f) {
                                    let text = String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').trim().to_string();
                                    logfile::info(&format!("ASK answered by the test (late): {text}"));
                                    answer(&app3, &mine, Choice::parse(&text));
                                    if let Some(w) = app3.get_webview_window(&mine) {
                                        let _ = w.close();
                                    }
                                    return;
                                }
                            }
                        });
                    }
                    let app2 = app.clone();
                    let mine = label.clone();
                    window.on_window_event(move |event| {
                        if let tauri::WindowEvent::Destroyed = event {
                            // Closed without choosing: "not now".
                            answer(&app2, &mine, None);
                        }
                    });
                }
                Err(e) => {
                    logfile::warn(&format!("couldn't show the stale window: {e}"));
                    answer(&app, &label, None);
                }
            }
        });
        rx
    }

    fn editing(&self, file: Option<(&Path, &str)>) {
        let Some(shared) = self.app.try_state::<Shared>() else { return };
        let Some(items) = &shared.items else { return };
        let on = file.is_some();
        let _ = items.open.set_enabled(on);
        let _ = items.folder.set_enabled(on);
        let _ = items.done.set_enabled(on);
        let _ = items.open.set_text(match file {
            Some((_, name)) => menu_text(&format!("Open {name}")),
            None => "Open the file".into(),
        });
    }
}

/// Hand the waiting session its answer, if `label` is the window it waits on.
fn answer(app: &AppHandle, label: &str, choice: Option<Choice>) {
    let shared = app.state::<Shared>();
    let mut slot = shared.answer.lock().unwrap();
    if slot.as_ref().map(|(l, _)| l.as_str()) == Some(label) {
        if let Some((_, tx)) = slot.take() {
            let _ = tx.send(choice);
        }
    }
}

/// The stale window's buttons ("" is "Not now").
#[tauri::command]
fn answer_stale(app: AppHandle, window: tauri::WebviewWindow, choice: String) {
    let picked = Choice::parse(&choice);
    logfile::info(&format!("ASK answered in the window: {}", if choice.is_empty() { "not now" } else { &choice }));
    answer(&app, window.label(), picked);
    let _ = window.close();
}

fn ui_of(app: &AppHandle) -> TrayUi {
    TrayUi { app: app.clone() }
}

/// A link arrived: the one this launch started with, or one forwarded from a
/// second launch. May run on the main thread, so it only starts work.
fn handle_link(app: &AppHandle, raw: &str) {
    match link::parse(raw) {
        Err(link::LinkError::NotOurs) => {}
        Err(link::LinkError::Malformed) => refuse_link(app),
        Ok(link::OpenLink { pass }) => start_session(app, pass),
    }
}

fn refuse_link(app: &AppHandle) {
    logfile::warn("LINK refused (not a valid signflow-edit link)");
    ui_of(app).tell("This link doesn't work. Open the file again from SignFlow.");
}

fn start_session(app: &AppHandle, pass: String) {
    let shared = app.state::<Shared>();
    {
        let mut seen = shared.seen.lock().unwrap();
        seen.retain(|_, at| at.elapsed() < Duration::from_secs(10));
        if seen.insert(pass.clone(), Instant::now()).is_some() {
            logfile::info("LINK repeated within seconds, ignored");
            return;
        }
    }
    let mut current = shared.current.lock().unwrap();
    if shared.installing.load(Ordering::SeqCst) {
        drop(current);
        logfile::warn("LINK while an update is installing: asked to try again in a minute");
        ui_of(app).tell("SignFlow Edit is updating itself. Open the file again from SignFlow in a minute.");
        return;
    }
    if current.is_some() {
        drop(current);
        logfile::warn("LINK while an edit is open: asked to finish it first");
        ui_of(app).tell(&format!("You're already editing a file. Click Done in the SignFlow Helper menu ({}), then open the other file again from SignFlow.", config::words::MENU_PLACE));
        return;
    }
    let (tx, rx) = mpsc::channel(8);
    *current = Some(tx);
    drop(current);
    let ctx = shared.ctx.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let ended = session::run(ctx, pass, rx).await;
        logfile::info(&format!("SESSION {ended:?}"));
        edit_ended(&app);
    });
}

/// The edit is over: an update waiting for it installs now; one found
/// meanwhile is offered now (unless she said "Not now"). Quit wins over both.
fn edit_ended(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let quit = *shared.quit_after_done.lock().unwrap();
    let mut current = shared.current.lock().unwrap();
    *current = None;
    // Decided with `current` held, so no link can start an edit in between.
    let install_now = if quit {
        None
    } else {
        let mut phase = shared.update_phase.lock().unwrap();
        update::after_edit(&phase).map(|next| {
            *phase = next.clone();
            shared.installing.store(true, Ordering::SeqCst);
            next
        })
    };
    drop(current);
    if quit {
        app.exit(0);
        return;
    }
    if let Some(next) = install_now {
        show_phase(app, &next, true);
        let found = shared.update_found.lock().unwrap().clone();
        let bytes = shared.update_ready.lock().unwrap().take();
        match (found, bytes) {
            (Some(found), Some(bytes)) => {
                logfile::info("UPDATE the edit ended; installing now");
                install(app, found, bytes);
            }
            _ => {
                shared.installing.store(false, Ordering::SeqCst);
                fail(app, update::Failure::InstallFailed);
            }
        }
        return;
    }
    let phase = shared.update_phase.lock().unwrap().clone();
    if matches!(phase, Phase::Available { .. }) {
        let snoozed = update::Saved::read(&shared.ctx.home).snoozed(update::now_secs());
        if update::pop_up(false, false, snoozed) {
            logfile::info("UPDATE the edit ended; offering the update found meanwhile");
            show_update_window(app);
        }
    }
}

fn send_control(app: &AppHandle, c: Control) -> bool {
    let shared = app.state::<Shared>();
    let current = shared.current.lock().unwrap();
    current.as_ref().is_some_and(|tx| tx.try_send(c).is_ok())
}

fn build_tray(app: &tauri::App, version: &str) -> tauri::Result<Items> {
    let status = MenuItem::with_id(app, "status", "Not editing", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open the file", false, None::<&str>)?;
    let folder = MenuItem::with_id(app, "folder", "Show in folder", false, None::<&str>)?;
    let done = MenuItem::with_id(app, "done", "Done (save and close the edit)", false, None::<&str>)?;
    let (line, enabled) = update::menu_line(&Phase::Idle, version);
    let update = MenuItem::with_id(app, "update", menu_text(&line), enabled, None::<&str>)?;
    let check = MenuItem::with_id(app, "check-updates", "Check for updates now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit SignFlow Helper", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status, &sep1, &open, &folder, &done, &sep2, &update, &check, &sep3, &quit])?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("SignFlow Helper — Not editing")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                send_control(app, Control::OpenFile);
            }
            "folder" => {
                send_control(app, Control::ShowFolder);
            }
            "done" => {
                send_control(app, Control::Done);
            }
            "update" => {
                logfile::info("UPDATE menu: update line clicked");
                show_update_window(app);
            }
            "check-updates" => {
                logfile::info("UPDATE menu: Check for updates now");
                let app = app.clone();
                tauri::async_runtime::spawn(async move { check_for_updates(app, true).await });
            }
            "quit" => {
                // With an edit open, Quit finishes it first (last save, tell
                // SignFlow), then exits.
                if send_control(app, Control::Done) {
                    *app.state::<Shared>().quit_after_done.lock().unwrap() = true;
                } else {
                    app.exit(0);
                }
            }
            _ => {}
        });
    // macOS: a black-and-clear "template" picture that the menu bar tints for
    // light and dark. Windows: the app's own icon by the clock.
    #[cfg(target_os = "macos")]
    {
        tray = tray.icon(tauri::include_image!("./icons/menubar-template.png")).icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(Items { status, open, folder, done, update, check })
}

pub fn run() {
    std::panic::set_hook(Box::new(|info| logfile::warn(&format!("panic: {info}"))));
    let context = tauri::generate_context!();
    // The version as built (tauri.conf.json), which is also what updates compare.
    let version = context.package_info().version.to_string();
    logfile::info(&format!("start: SignFlow Helper {version}{}", if cfg!(feature = "e2e") { " (e2e build)" } else { "" }));
    if !config::endpoints_are_safe() {
        logfile::warn("refusing to run: built with an address that isn't HTTPS");
        return;
    }
    tauri::Builder::default()
        // Must be first: a second launch (another link clicked) hands its
        // link to the running helper (on_open_url below) and exits.
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            logfile::info("second launch forwarded to the running helper");
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![answer_stale, update_choice])
        .setup(move |app| {
            // A menu-bar app on macOS: no Dock icon, no app menu (Info.plist's
            // LSUIElement says the same from launch).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let handle = app.handle().clone();
            let items = match build_tray(app, &version) {
                Ok(items) => Some(items),
                Err(e) => {
                    logfile::warn(&format!("no tray icon: {e}"));
                    None
                }
            };
            let ctx = session::Ctx {
                api: api::Api::new(config::SIGNFLOW_URL),
                blob: blob::BlobClient::new(config::BLOB_API_URL),
                ui: Arc::new(TrayUi { app: handle.clone() }),
                home: config::home_dir(),
                // ShellExecute: the file opens in Windows' default program for .fs.
                open: Arc::new(|p: &Path| tauri_plugin_opener::open_path(p, None::<&str>).map_err(|e| e.to_string())),
                reveal: Arc::new(|p: &Path| tauri_plugin_opener::reveal_item_in_dir(p).map_err(|e| e.to_string())),
                timing: watcher::Timing::default(),
                retry_after: Duration::from_secs(30),
            };
            app.manage(Shared {
                current: Mutex::new(None),
                answer: Mutex::new(None),
                asks: AtomicU64::new(0),
                seen: Mutex::new(HashMap::new()),
                quit_after_done: Mutex::new(false),
                ctx: Arc::new(ctx),
                items,
                version: version.clone(),
                update_phase: Mutex::new(Phase::Idle),
                update_found: Mutex::new(None),
                update_ready: Mutex::new(None),
                installing: AtomicBool::new(false),
                update_said: Mutex::new(String::new()),
            });

            // Did the last update take?
            let home = config::home_dir();
            let mut saved = update::Saved::read(&home);
            match update::after_restart(&saved, &version, update::now_secs()) {
                update::AfterRestart::Updated { from, to } => {
                    logfile::info(&format!("UPDATE updated from {from} to {to}"));
                    set_phase(&handle, Phase::Updated(to));
                }
                update::AfterRestart::DidNotInstall { to } => {
                    logfile::warn(&format!("UPDATE the update to {to} didn't install; still {version}"));
                    ui_of(&handle).tell(&format!(
                        "The update to SignFlow Edit {to} didn't finish installing. SignFlow Edit {version} still works, and the update will be offered again."
                    ));
                }
                update::AfterRestart::Nothing => {}
            }
            if saved.installing.take().is_some() {
                let _ = saved.write(&home);
            }

            // Updates: a little after start, then every six hours of real
            // time (counted by the clock, so a sleeping computer catches up).
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(update::FIRST_CHECK_AFTER).await;
                loop {
                    check_for_updates(h.clone(), false).await;
                    let since = std::time::SystemTime::now();
                    loop {
                        tokio::time::sleep(Duration::from_secs(300)).await;
                        if since.elapsed().map(|d| d >= update::CHECK_EVERY).unwrap_or(true) {
                            break;
                        }
                    }
                }
            });

            // macOS runs an app opened straight from the .dmg or Downloads from
            // a temporary read-only copy: links and updates can't reach it there.
            #[cfg(target_os = "macos")]
            if let Ok(exe) = std::env::current_exe() {
                let p = exe.to_string_lossy();
                if p.contains("/AppTranslocation/") || p.starts_with("/Volumes/") {
                    logfile::warn("running from outside Applications (translocated or on the .dmg)");
                    ui_of(&handle).tell("Drag SignFlow Helper into your Applications folder, then open it from there. Links from SignFlow only reach it in Applications.");
                }
            }

            // Later links (forwarded from a second launch).
            let h = handle.clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    handle_link(&h, url.as_str());
                }
            });
            // The link this launch started with.
            match app.deep_link().get_current() {
                Ok(Some(urls)) => {
                    for url in urls {
                        handle_link(&handle, url.as_str());
                    }
                }
                _ => match link::from_args(std::env::args().skip(1)) {
                    Some(Ok(link::OpenLink { pass })) => start_session(&handle, pass),
                    Some(Err(_)) => refuse_link(&handle),
                    None => {}
                },
            }

            #[cfg(feature = "e2e")]
            {
                // The e2e run asks for Done through a file (it can't click the
                // tray on the runner).
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let f = config::home_dir().join("e2e-control.txt");
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if let Ok(text) = std::fs::read_to_string(&f) {
                            let _ = std::fs::remove_file(&f);
                            let asked = text.trim_start_matches('\u{feff}').trim().to_string();
                            logfile::info(&format!("e2e: {asked} asked"));
                            match asked.as_str() {
                                "done" => {
                                    send_control(&h, Control::Done);
                                }
                                // The menu's "Check for updates now", and the
                                // automatic check (which may not pop up).
                                "check" => check_for_updates(h.clone(), true).await,
                                "check-auto" => check_for_updates(h.clone(), false).await,
                                // The window's buttons (the Mac run can't click them).
                                "update" => start_update(&h),
                                "not-now" => {
                                    snooze(&h);
                                    if let Some(w) = h.get_webview_window(UPDATE_WINDOW) {
                                        let _ = w.close();
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                });
            }
            Ok(())
        })
        .build(context)
        .expect("SignFlow Helper couldn't start")
        .run(|_app, event| {
            // A tray app: closing the stale window must not quit it. app.exit
            // (Quit) carries a code and goes through.
            if let RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

// ---- Updates (the rules are in update.rs)

/// The updater, set up the helper's way.
fn build_updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, tauri_plugin_updater::Error> {
    let url = url::Url::parse(config::UPDATE_MANIFEST_URL)?;
    let builder = app
        .updater_builder()
        .endpoints(vec![url])?
        .timeout(Duration::from_secs(30))
        .version_comparator(|current, release| update::newer(&release.version, &current))
        // A dead line shows up in seconds, not when a whole-download timeout ends.
        .configure_client(|c| c.connect_timeout(Duration::from_secs(15)).read_timeout(Duration::from_secs(30)));
    // Windows: the installer starts the new helper itself, with no arguments.
    // (The plugin's own restart would repeat the arguments this helper was
    // started with, and those can be an old, used link.)
    #[cfg(windows)]
    let builder = builder.restart_after_install(false).installer_arg("/R");
    builder.build()
}

/// Sets where updates stand and shows it (menu line, dot, window).
fn set_phase(app: &AppHandle, next: Phase) {
    change_phase(app, |_| Some(next));
}

/// Changes where updates stand in one step (`f` gives the next phase, or
/// None to leave it), then shows it. Never holds a lock while showing.
fn change_phase(app: &AppHandle, f: impl FnOnce(&Phase) -> Option<Phase>) -> Option<Phase> {
    let shared = app.state::<Shared>();
    let (had_dot, next) = {
        let mut phase = shared.update_phase.lock().unwrap();
        let next = f(&phase)?;
        let had = phase.dot();
        *phase = next.clone();
        (had, next)
    };
    show_phase(app, &next, had_dot);
    Some(next)
}

fn show_phase(app: &AppHandle, phase: &Phase, had_dot: bool) {
    let shared = app.state::<Shared>();
    let (line, enabled) = update::menu_line(phase, &shared.version);
    if let Some(items) = &shared.items {
        let _ = items.update.set_text(menu_text(&line));
        let _ = items.update.set_enabled(enabled);
        let _ = items.check.set_enabled(!phase.busy() && *phase != Phase::Checking);
    }
    if !matches!(phase, Phase::Downloading { percent, .. } if percent % 25 != 0) {
        logfile::info(&format!("UPDATE menu: {line}"));
    }
    if phase.dot() != had_dot {
        set_dot(app, phase.dot());
    }
    render_update_window(app);
}

/// The dot on the tray picture: a newer version is waiting.
fn set_dot(app: &AppHandle, on: bool) {
    logfile::info(if on { "UPDATE dot on" } else { "UPDATE dot off" });
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    #[cfg(target_os = "macos")]
    {
        let picture = if on { tauri::include_image!("./icons/menubar-template-update.png") } else { tauri::include_image!("./icons/menubar-template.png") };
        let _ = tray.set_icon(Some(picture));
        let _ = tray.set_icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let picture = if on { Some(tauri::include_image!("./icons/tray-update.png")) } else { app.default_window_icon().cloned() };
        let _ = tray.set_icon(picture);
    }
}

/// One check. Quiet when it fails: only the menu says "Couldn't check for
/// updates". `asked`: "Check for updates now" (always shows what it finds).
async fn check_for_updates(app: AppHandle, asked: bool) {
    let before = app.state::<Shared>().update_phase.lock().unwrap().clone();
    if before == Phase::Checking {
        // One is under way: when she asked, show what it finds.
        logfile::info("UPDATE check already under way");
        if asked {
            for _ in 0..80 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let now = app.state::<Shared>().update_phase.lock().unwrap().clone();
                if now != Phase::Checking {
                    if now.dot() {
                        show_update_window(&app);
                    }
                    break;
                }
            }
        }
        return;
    }
    if before.busy() {
        logfile::info("UPDATE check skipped: an update is under way");
        if asked {
            show_update_window(&app);
        }
        return;
    }
    if !before.dot() {
        set_phase(&app, Phase::Checking);
    }
    logfile::info(&format!("UPDATE check ({})", if asked { "asked" } else { "automatic" }));
    let found = match build_updater(&app) {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };
    let found = match found {
        Ok(Some(found)) if !config::update_download_allowed(&found.download_url) => {
            logfile::warn("UPDATE refused: latest.json points somewhere this helper won't download from");
            update::Found::Failed
        }
        Ok(Some(found)) => {
            let version = found.version.clone();
            let notes = update::short_notes(found.body.as_deref());
            logfile::info(&format!("UPDATE found {version}"));
            *app.state::<Shared>().update_found.lock().unwrap() = Some(found);
            update::Found::Newer { version, notes }
        }
        Ok(None) => {
            logfile::info("UPDATE up to date");
            update::Found::NotNewer
        }
        Err(e) => {
            logfile::warn(&format!("UPDATE check failed: {e}"));
            update::Found::Failed
        }
    };
    let Some(next) = change_phase(&app, |current| Some(update::after_check(current, found))) else { return };
    if let Phase::Available { version, .. } = &next {
        let shared = app.state::<Shared>();
        let editing = shared.current.lock().unwrap().is_some();
        let snoozed = update::Saved::read(&shared.ctx.home).snoozed(update::now_secs());
        if app.get_webview_window(UPDATE_WINDOW).is_some() {
            logfile::info("UPDATE the window is open: its words are updated");
            show_update_window(&app);
        } else if update::pop_up(asked, editing, snoozed) {
            show_update_window(&app);
        } else {
            logfile::info(&format!(
                "UPDATE {version}: no window ({}); the dot and the menu say it",
                if editing { "she's editing; offered when the edit ends" } else { "she said Not now" }
            ));
        }
    }
}

/// "Update" or "Try again": download, check the signature, then install —
/// or wait, if she is editing.
fn start_update(app: &AppHandle) {
    let shared = app.state::<Shared>();
    #[cfg(target_os = "macos")]
    if running_outside_applications() {
        logfile::warn("UPDATE refused: not running from Applications");
        fail(app, update::Failure::NotInApplications);
        return;
    }
    let started = change_phase(app, |current| {
        if !update::may_start_update(current) {
            return None;
        }
        let (version, notes) = current.update()?;
        Some(Phase::Downloading { version: version.to_string(), notes: notes.to_string(), percent: 0 })
    });
    let Some(Phase::Downloading { version, .. }) = started else {
        logfile::info("UPDATE update asked, but there's nothing to start");
        return;
    };
    let found = shared.update_found.lock().unwrap().clone();
    let Some(found) = found else {
        fail(app, update::Failure::Unavailable);
        return;
    };
    logfile::info(&format!("UPDATE downloading {version}"));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let received = Arc::new(AtomicU64::new(0));
        let counter = received.clone();
        let app2 = app.clone();
        let mut shown = 0u8;
        let result = found
            .download(
                move |chunk, total| {
                    let got = counter.fetch_add(chunk as u64, Ordering::SeqCst) + chunk as u64;
                    if let Some(total) = total.filter(|t| *t > 0) {
                        let percent = (got.saturating_mul(100) / total).min(100) as u8;
                        if percent != shown {
                            shown = percent;
                            change_phase(&app2, |p| match p {
                                Phase::Downloading { version, notes, .. } => Some(Phase::Downloading { version: version.clone(), notes: notes.clone(), percent }),
                                _ => None,
                            });
                        }
                    }
                },
                || {},
            )
            .await;
        match result {
            Err(e) => {
                let failure = update::classify(&e, update::Stage::Download, received.load(Ordering::SeqCst));
                logfile::warn(&format!("UPDATE failed ({}): {e}", failure.name()));
                fail(&app, failure);
            }
            Ok(bytes) => {
                logfile::info(&format!("UPDATE downloaded {} bytes; the signature checks out", bytes.len()));
                downloaded(&app, found, bytes);
            }
        }
    });
}

/// The checked file is in hand: install now, or wait for the edit to end.
fn downloaded(app: &AppHandle, found: tauri_plugin_updater::Update, bytes: Vec<u8>) {
    let shared = app.state::<Shared>();
    // Decided with `current` held, so an edit can't start or end in between.
    let current = shared.current.lock().unwrap();
    let editing = current.is_some();
    let (had_dot, next) = {
        let mut phase = shared.update_phase.lock().unwrap();
        let Some(next) = update::after_download(&phase, editing) else { return };
        let had = phase.dot();
        *phase = next.clone();
        (had, next)
    };
    let install_now = if editing {
        *shared.update_ready.lock().unwrap() = Some(bytes);
        None
    } else {
        shared.installing.store(true, Ordering::SeqCst);
        Some(bytes)
    };
    drop(current);
    show_phase(app, &next, had_dot);
    match install_now {
        None => logfile::info("UPDATE waiting for the edit to end before installing"),
        Some(bytes) => install(app, found, bytes),
    }
}

/// Installs (the phase already says Installing and `installing` is set).
/// Windows: the installer takes over and this process ends. macOS: the app
/// is swapped, then the helper starts again. On failure nothing is changed.
fn install(app: &AppHandle, found: tauri_plugin_updater::Update, bytes: Vec<u8>) {
    let shared = app.state::<Shared>();
    let home = shared.ctx.home.clone();
    let mut saved = update::Saved::read(&home);
    saved.installing = Some(update::Installing { from: shared.version.clone(), to: found.version.clone(), at: update::now_secs() });
    let _ = saved.write(&home);
    logfile::info(&format!("UPDATE installing {} (from {})", found.version, shared.version));
    let app = app.clone();
    // Not on the main thread: on a Mac the plugin may need it for a password prompt.
    std::thread::spawn(move || {
        // A moment for the window to say "Installing…".
        std::thread::sleep(Duration::from_millis(800));
        match found.install(&bytes) {
            Ok(()) => {
                // Windows never gets here: the plugin ends this process once
                // the installer has started.
                logfile::info(&format!("UPDATE installed {}; starting again", found.version));
                app.restart();
            }
            Err(e) => {
                let failure = update::classify(&e, update::Stage::Install, bytes.len() as u64);
                logfile::warn(&format!("UPDATE failed ({}): {e}", failure.name()));
                let mut saved = update::Saved::read(&home);
                saved.installing = None;
                let _ = saved.write(&home);
                app.state::<Shared>().installing.store(false, Ordering::SeqCst);
                fail(&app, failure);
            }
        }
    });
}

/// Something went wrong: the window says what (it opens again if she had
/// closed it — she asked for the update and should hear back).
fn fail(app: &AppHandle, failure: update::Failure) {
    let shared = app.state::<Shared>();
    *shared.update_ready.lock().unwrap() = None;
    change_phase(app, |p| {
        let (version, notes) = p.update().map(|(v, n)| (v.to_string(), n.to_string())).unwrap_or_else(|| (shared.version.clone(), String::new()));
        Some(Phase::Failed { version, notes, failure })
    });
    show_update_window(app);
}

/// "Not now": no window for a day; the dot and the menu stay.
fn snooze(app: &AppHandle) {
    let home = app.state::<Shared>().ctx.home.clone();
    let mut saved = update::Saved::read(&home);
    saved.snooze(update::now_secs());
    let _ = saved.write(&home);
    logfile::info(&format!("UPDATE not now: no window until {} (the dot and the menu stay)", saved.snoozed_until));
}

#[cfg(target_os = "macos")]
fn running_outside_applications() -> bool {
    std::env::current_exe()
        .map(|exe| {
            let p = exe.to_string_lossy();
            p.contains("/AppTranslocation/") || p.starts_with("/Volumes/")
        })
        .unwrap_or(false)
}

fn update_view(app: &AppHandle) -> update::View {
    let shared = app.state::<Shared>();
    let phase = shared.update_phase.lock().unwrap().clone();
    update::view(&phase, &shared.version)
}

/// Says what the window says in the log (the Mac run can't read the window).
fn log_view(app: &AppHandle, v: &update::View) {
    if v.percent.is_some() {
        return;
    }
    let text = format!("{} | {}", v.title, v.message);
    let shared = app.state::<Shared>();
    let mut said = shared.update_said.lock().unwrap();
    if *said != text {
        logfile::info(&format!("UPDATE window says: {text}"));
        *said = text;
    }
}

fn render_update_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(UPDATE_WINDOW) else { return };
    let v = update_view(app);
    log_view(app, &v);
    if let Ok(json) = serde_json::to_string(&v) {
        let _ = window.eval(format!("window.__render && window.__render({json});"));
    }
}

/// Shows the update window (or brings it forward with its words updated).
fn show_update_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(UPDATE_WINDOW) {
        render_update_window(app);
        let _ = window.set_focus();
        return;
    }
    let v = update_view(app);
    logfile::info("UPDATE window shown");
    app.state::<Shared>().update_said.lock().unwrap().clear();
    log_view(app, &v);
    let data = serde_json::to_string(&v).unwrap_or_else(|_| "{}".into());
    let app = app.clone();
    // Built off the main thread (building a window inside an event handler
    // can deadlock on Windows).
    std::thread::spawn(move || {
        let builder = WebviewWindowBuilder::new(&app, UPDATE_WINDOW, WebviewUrl::App("update.html".into()))
            .title("SignFlow Edit update")
            .inner_size(440.0, 340.0)
            .resizable(false)
            .minimizable(false)
            .maximizable(false)
            .center()
            .focused(true)
            .initialization_script(format!("window.__UPDATE__ = {data};"));
        #[cfg(all(feature = "e2e", windows))]
        let builder = builder.additional_browser_args(E2E_WEBVIEW_ARGS);
        match builder.build() {
            Ok(window) => {
                let app2 = app.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Destroyed = event {
                        logfile::info("UPDATE window closed");
                        // Closed without choosing: the same as "Not now".
                        let unanswered = matches!(*app2.state::<Shared>().update_phase.lock().unwrap(), Phase::Available { .. });
                        if unanswered {
                            snooze(&app2);
                        }
                    }
                });
            }
            Err(e) => logfile::warn(&format!("UPDATE couldn't show the window: {e}")),
        }
    });
}

/// The update window's buttons.
#[tauri::command]
fn update_choice(app: AppHandle, window: tauri::WebviewWindow, choice: String) {
    logfile::info(&format!("UPDATE window: {choice} clicked"));
    match choice.as_str() {
        "update" | "retry" => start_update(&app),
        "later" => {
            snooze(&app);
            let _ = window.close();
        }
        // "ok", "hide", "close": the window goes; whatever is under way goes on.
        _ => {
            let _ = window.close();
        }
    }
}
