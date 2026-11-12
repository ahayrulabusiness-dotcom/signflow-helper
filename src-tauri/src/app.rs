//! The tray app around a session: answers `signflow-edit://open?pass=…`
//! links (link.rs), runs one edit at a time (session.rs), shows its status in
//! the tray and asks the stale question in a small window. It holds no secret
//! and no storage key; SignFlow's address is built in (config.rs).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tokio::sync::{mpsc, oneshot};

use crate::api::{self, Choice};
use crate::session::{self, Control, Question, Ui};
use crate::{blob, config, link, logfile, watcher};

const TRAY_ID: &str = "signflow-helper";

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
}

struct Items {
    status: MenuItem<Wry>,
    open: MenuItem<Wry>,
    folder: MenuItem<Wry>,
    done: MenuItem<Wry>,
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
            let built = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("stale.html".into()))
                .title("SignFlow Helper")
                .inner_size(460.0, 400.0)
                .resizable(false)
                .minimizable(false)
                .maximizable(false)
                .always_on_top(true)
                .center()
                .focused(true)
                .initialization_script(format!("window.__STALE__ = {data};"))
                .build();
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
    if current.is_some() {
        drop(current);
        logfile::warn("LINK while an edit is open: asked to finish it first");
        ui_of(app).tell("You're already editing a file. Click Done in the SignFlow Helper menu (by the clock), then open the other file again from SignFlow.");
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
        let shared = app.state::<Shared>();
        *shared.current.lock().unwrap() = None;
        if *shared.quit_after_done.lock().unwrap() {
            app.exit(0);
        }
    });
}

fn send_control(app: &AppHandle, c: Control) -> bool {
    let shared = app.state::<Shared>();
    let current = shared.current.lock().unwrap();
    current.as_ref().is_some_and(|tx| tx.try_send(c).is_ok())
}

fn build_tray(app: &tauri::App) -> tauri::Result<Items> {
    let status = MenuItem::with_id(app, "status", "Not editing", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open the file", false, None::<&str>)?;
    let folder = MenuItem::with_id(app, "folder", "Show in folder", false, None::<&str>)?;
    let done = MenuItem::with_id(app, "done", "Done (save and close the edit)", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit SignFlow Helper", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status, &sep1, &open, &folder, &done, &sep2, &quit])?;
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
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(Items { status, open, folder, done })
}

pub fn run() {
    std::panic::set_hook(Box::new(|info| logfile::warn(&format!("panic: {info}"))));
    logfile::info(&format!("start: SignFlow Helper {}{}", env!("CARGO_PKG_VERSION"), if cfg!(feature = "e2e") { " (e2e build)" } else { "" }));
    if !config::endpoints_are_safe() {
        logfile::warn("refusing to run: built with an address that isn't HTTPS");
        return;
    }
    #[cfg(feature = "e2e")]
    {
        // Lets the e2e run click the stale window through WebView2's
        // debugging port. Only the e2e build does this.
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--remote-debugging-port=9223");
    }
    tauri::Builder::default()
        // Must be first: a second launch (another link clicked) hands its
        // link to the running helper (on_open_url below) and exits.
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            logfile::info("second launch forwarded to the running helper");
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![answer_stale])
        .setup(|app| {
            let handle = app.handle().clone();
            let items = match build_tray(app) {
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
            });

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
                            if text.trim() == "done" {
                                logfile::info("e2e: Done asked");
                                send_control(&h, Control::Done);
                            }
                        }
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
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
