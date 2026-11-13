//! One "Open in Flexi": from the link's pass to Done.
//!
//!  1. Swap the pass for an edit key (kept only in memory).
//!  2. Get her copy ready (store::decide): download it through the 5-minute
//!     link, use her copy as it is, or — when her copy has changes that aren't
//!     in SignFlow — upload them, or ask her first if somebody saved since.
//!  3. Open it with Windows' program for .fs files (Flexi).
//!  4. Check in with SignFlow every minute, so the lock holds.
//!  5. Each settled save: snapshot it, upload the snapshot with SignFlow's
//!     one-file token, ask SignFlow to make it the new version. If somebody
//!     saved a newer one meanwhile, she chooses: keep theirs, upload mine as
//!     newest, or save mine as a copy.
//!  6. Done: a last save if there is one, then SignFlow ends the edit.
//!
//! Safe failure: with no answer it keeps the file and tries again; it never
//! deletes her file and never uploads anything but this one file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use crate::api::{Api, ApiError, Choice, Exchange, Saved, StaleInfo};
use crate::blob::{BlobClient, BlobError, Progress};
use crate::config::words;
use crate::logfile as log;
use crate::store::{self, Local, Notes, Start};
use crate::watcher::{self, Settled, Timing};

/// What the session shows and asks; the tray app or a test supplies it.
pub trait Ui: Send + Sync {
    /// The one-line status (tray menu and tooltip).
    fn status(&self, text: &str);
    /// A message she should read (a small window).
    fn tell(&self, text: &str);
    /// The stale question; None when she closes it without choosing.
    fn ask(&self, question: Question) -> oneshot::Receiver<Option<Choice>>;
    /// The file being edited, or None when the edit ends.
    fn editing(&self, file: Option<(&Path, &str)>);
}

/// What the stale window says.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub file_name: String,
    pub changed_by: Option<String>,
    pub changed_at: Option<String>,
}

impl Question {
    fn from(file_name: &str, info: &StaleInfo) -> Question {
        Question { file_name: file_name.into(), changed_by: info.changed_by.clone(), changed_at: info.changed_at.clone() }
    }
    fn who(&self) -> String {
        self.changed_by.clone().unwrap_or_else(|| "Someone".into())
    }
}

/// From the tray, while an edit is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Done,
    OpenFile,
    ShowFolder,
}

pub struct Ctx {
    pub api: Api,
    pub blob: BlobClient,
    pub ui: Arc<dyn Ui>,
    pub home: PathBuf,
    /// Opens a file with its Windows program, or shows a folder.
    pub open: Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>,
    pub reveal: Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>,
    pub timing: Timing,
    /// How long between tries when SignFlow doesn't answer.
    pub retry_after: Duration,
}

/// How it ended, for the caller and the tests.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    /// The pass didn't work (used, expired, invalid…): SignFlow's sentence.
    PassRefused(String),
    /// Done in the tray.
    Done,
    /// SignFlow ended the edit (a release, the job, access…).
    EndedBySignFlow(String),
    /// "Keep theirs": her copy stays here; the edit ends.
    KeptTheirs,
    /// Something here went wrong before editing began.
    CouldNotStart(String),
}

fn file_label(name: &str) -> String {
    name.to_string()
}

fn now_hhmm() -> String {
    let now = chrono::Local::now();
    now.format("%-I:%M %p").to_string()
}

fn stamp_for_name() -> String {
    chrono::Local::now().format("%Y-%m-%d %H%M").to_string()
}

pub async fn run(ctx: Arc<Ctx>, pass: String, mut control: mpsc::Receiver<Control>) -> Ended {
    let ui = ctx.ui.clone();
    ui.status("Opening the file…");
    log::info("session: swapping the pass for an edit key");

    // 1. The pass, once. A short retry only for no answer: it lasts 2 minutes.
    let mut tries = 0;
    let ex: Exchange = loop {
        match ctx.api.exchange(&pass).await {
            Ok(ex) => break ex,
            Err(e) if e.is_retryable() && tries < 4 => {
                tries += 1;
                log::warn(&format!("session: exchange not answered ({e:?}); trying again"));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                let msg = e.message();
                log::warn(&format!("session: pass refused: {msg}"));
                ui.status("Not editing");
                ui.tell(&msg);
                return Ended::PassRefused(msg);
            }
        }
    };
    let key = ex.key.clone();
    let name = ex.file.name.clone();
    log::info(&format!("session: editing {name} (file {})", ex.file.id));

    let Some(local) = Local::for_file(&ctx.home, &ex.file.id, &name) else {
        let _ = ctx.api.done(&key).await;
        ui.status("Not editing");
        ui.tell("SignFlow sent a file this helper can't open. Nothing was changed.");
        return Ended::CouldNotStart("bad file id".into());
    };
    if let Err(e) = std::fs::create_dir_all(&local.dir) {
        let _ = ctx.api.done(&key).await;
        ui.status("Not editing");
        ui.tell("The helper couldn't make its folder on this computer. Nothing was changed.");
        return Ended::CouldNotStart(e.to_string());
    }

    // 2. Her copy.
    let current = if local.path.exists() {
        let p = local.path.clone();
        tokio::task::spawn_blocking(move || watcher::fingerprint(&p).ok()).await.ok().flatten().map(|(h, _)| h)
    } else {
        None
    };
    let notes_now = local.read_notes();
    let start = store::decide(current.as_deref(), notes_now.as_ref(), &ex.file.version);
    log::info(&format!("session: her copy: {start:?}"));
    let mut notes = notes_now.clone().unwrap_or(Notes { file_id: ex.file.id.clone(), file_name: name.clone(), base_version: ex.file.version.clone(), saved_hash: String::new() });
    let mut pending: Option<String> = None;
    let mut ask_first = false;

    match start {
        Start::UseAsIs => {}
        Start::Download | Start::Refresh => {
            if start == Start::Refresh && !watcher::released(&local.path) {
                let _ = ctx.api.done(&key).await;
                ui.status("Not editing");
                ui.tell(&format!("A newer version of {name} is in SignFlow. Close {name}{}, then open it again from SignFlow.", words::IN_EDITOR));
                return Ended::CouldNotStart("file in use".into());
            }
            match download(&ctx, &key, &local, &name).await {
                Ok((hash, version)) => {
                    notes = Notes { file_id: ex.file.id.clone(), file_name: name.clone(), base_version: version, saved_hash: hash };
                    let _ = local.write_notes(&notes);
                }
                Err(msg) => {
                    let _ = ctx.api.done(&key).await;
                    ui.status("Not editing");
                    ui.tell(&msg);
                    return Ended::CouldNotStart(msg);
                }
            }
        }
        Start::UploadPending => pending = current.clone(),
        Start::Conflict => {
            pending = current.clone();
            ask_first = true;
        }
    }

    // 3. Open it in Flexi.
    if let Err(e) = (ctx.open)(&local.path) {
        log::warn(&format!("session: couldn't open the file: {e}"));
        ui.tell(&format!(
            "{} couldn't open {name}. {} Your file is here: {}",
            words::COMPUTER,
            words::SET_UP_EDITOR,
            local.path.display()
        ));
    }
    ui.editing(Some((&local.path, &name)));
    ui.status(&format!("Editing {}", file_label(&name)));

    // 4. Check in every minute.
    let (end_tx, mut end_rx) = mpsc::channel::<String>(1);
    let (hb_stop_tx, hb_stop) = watch::channel(false);
    {
        let ctx = ctx.clone();
        let key = key.clone();
        let name = name.clone();
        let every = Duration::from_secs(ex.heartbeat_seconds.clamp(10, 300));
        tokio::spawn(heartbeat(ctx, key, name, every, end_tx, hb_stop));
    }

    // 5. Watch for saves.
    let (save_tx, mut save_rx) = mpsc::channel::<Settled>(4);
    let (watch_stop_tx, watch_stop) = watch::channel(false);
    tokio::spawn(watcher::watch_file(local.path.clone(), notes.saved_hash.clone(), ctx.timing, save_tx, watch_stop));

    let mut retry_at: Option<tokio::time::Instant> = if pending.is_some() { Some(tokio::time::Instant::now()) } else { None };
    let ended = loop {
        let wake = retry_at.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            Some(settled) = save_rx.recv() => {
                if settled.hash != notes.saved_hash {
                    pending = Some(settled.hash);
                    retry_at = Some(tokio::time::Instant::now());
                }
            }
            _ = tokio::time::sleep_until(wake), if retry_at.is_some() => {
                retry_at = None;
                if pending.is_some() {
                    match save(&ctx, &key, &local, &name, &mut notes, &mut pending, ask_first).await {
                        SaveOutcome::Saved | SaveOutcome::Waiting => { ask_first = false; }
                        SaveOutcome::Retry => { retry_at = Some(tokio::time::Instant::now() + ctx.retry_after); }
                        SaveOutcome::Ended(e) => break e,
                    }
                }
            }
            Some(msg) = end_rx.recv() => {
                break Ended::EndedBySignFlow(msg);
            }
            c = control.recv() => match c {
                Some(Control::OpenFile) => { if let Err(e) = (ctx.open)(&local.path) { ui.tell(&format!("{} couldn't open {name}: {e}", words::COMPUTER)); } }
                Some(Control::ShowFolder) => { let _ = (ctx.reveal)(&local.path); }
                Some(Control::Done) | None => {
                    // A last save, if there is one.
                    let p = local.path.clone();
                    let last = tokio::task::spawn_blocking(move || watcher::fingerprint(&p).ok()).await.ok().flatten().map(|(h, _)| h);
                    if let Some(h) = last { if h != notes.saved_hash { pending = Some(h); } }
                    if pending.is_some() {
                        if let SaveOutcome::Ended(e) = save(&ctx, &key, &local, &name, &mut notes, &mut pending, ask_first).await { break e; }
                    }
                    break Ended::Done;
                }
            },
        }
    };

    let _ = watch_stop_tx.send(true);
    let _ = hb_stop_tx.send(true);
    match &ended {
        Ended::Done | Ended::KeptTheirs => {
            for _ in 0..3 {
                match ctx.api.done(&key).await {
                    Err(e) if e.is_retryable() => tokio::time::sleep(Duration::from_secs(3)).await,
                    _ => break,
                }
            }
            if ended == Ended::Done {
                let unsaved = pending.is_some();
                ui.status(if unsaved { "Done — some changes didn't reach SignFlow" } else { "Done" });
                if unsaved {
                    ui.tell(&format!("Your latest changes to {name} didn't reach SignFlow. They're safe on this computer: {}", local.path.display()));
                }
            }
        }
        Ended::EndedBySignFlow(msg) => {
            ui.status("Not editing");
            ui.tell(&format!("{msg} Your file is still on this computer: {}", local.path.display()));
        }
        _ => {}
    }
    ui.editing(None);
    log::info(&format!("session: ended: {ended:?}"));
    ended
}

/// Downloads the file into place; its fingerprint and SignFlow's version
/// tag for what was downloaded. Never over unsaved work.
async fn download(ctx: &Ctx, key: &str, local: &Local, name: &str) -> Result<(String, String), String> {
    let mut tries = 0;
    loop {
        let attempt = async {
            let link = ctx.api.download_link(key).await.map_err(|e| (e.is_retryable(), e.message()))?;
            let tmp = local.download_temp();
            let _ = std::fs::remove_file(&tmp);
            let ui = ctx.ui.clone();
            let label = name.to_string();
            let progress: Progress = Arc::new(move |got, total| {
                if total > 0 {
                    ui.status(&format!("Downloading {label} {}%", got * 100 / total));
                }
            });
            match ctx.blob.download(&link.url, &tmp, Some(link.size_bytes), progress).await {
                Ok(_) => {}
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err((e.is_retryable(), blob_message(&e)));
                }
            }
            std::fs::rename(&tmp, &local.path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                (false, format!("The helper couldn't put {name} in its folder ({e}). Close it{} and open it again from SignFlow.", words::IN_EDITOR))
            })?;
            let p = local.path.clone();
            let (hash, _) = tokio::task::spawn_blocking(move || watcher::fingerprint(&p)).await.map_err(|e| (false, e.to_string()))?.map_err(|e| (false, e.to_string()))?;
            Ok::<(String, String), (bool, String)>((hash, link.version.clone()))
        };
        match attempt.await {
            Ok(got) => return Ok(got),
            Err((true, _)) if tries < 4 => {
                tries += 1;
                tokio::time::sleep(ctx.retry_after).await;
            }
            Err((_, msg)) => return Err(msg),
        }
    }
}

enum SaveOutcome {
    Saved,
    /// Waiting for her (she closed the question): the next save asks again.
    Waiting,
    Retry,
    Ended(Ended),
}

/// One save of whatever is in the file now.
async fn save(ctx: &Ctx, key: &str, local: &Local, name: &str, notes: &mut Notes, pending: &mut Option<String>, ask_first: bool) -> SaveOutcome {
    let ui = ctx.ui.clone();
    // Her copy has changes and somebody saved since: she chooses before anything is sent.
    let mut choice: Option<Choice> = None;
    if ask_first {
        let q = Question { file_name: name.into(), changed_by: None, changed_at: None };
        match ui.ask(q).await.ok().flatten() {
            None => {
                ui.status(&format!("Editing {name} — not saved to SignFlow yet"));
                return SaveOutcome::Waiting;
            }
            Some(Choice::KeepTheirs) => return keep_theirs_first(ctx, key, local, name, notes, pending).await,
            Some(c) => choice = Some(c),
        }
    }

    // A snapshot: exactly one saved state is sent, even if Flexi saves again.
    let snap = local.upload_snapshot();
    let from = local.path.clone();
    let to = snap.clone();
    let copied = tokio::task::spawn_blocking(move || -> std::io::Result<(String, u64)> {
        std::fs::copy(&from, &to)?;
        watcher::fingerprint(&to)
    })
    .await;
    let (hash, _size) = match copied {
        Ok(Ok(v)) => v,
        _ => {
            let _ = std::fs::remove_file(&snap);
            return SaveOutcome::Retry;
        }
    };
    if hash == notes.saved_hash {
        let _ = std::fs::remove_file(&snap);
        *pending = None;
        return SaveOutcome::Saved;
    }

    let outcome = upload_and_replace(ctx, key, &snap, name, choice, &hash, notes, pending).await;
    let _ = std::fs::remove_file(&snap);
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn upload_and_replace(ctx: &Ctx, key: &str, snap: &Path, name: &str, mut choice: Option<Choice>, hash: &str, notes: &mut Notes, pending: &mut Option<String>) -> SaveOutcome {
    let ui = ctx.ui.clone();
    let token = match ctx.api.upload_token(key).await {
        Ok(t) => t,
        Err(e) => return api_trouble(ctx, e),
    };
    let label = name.to_string();
    let ui2 = ui.clone();
    let progress: Progress = Arc::new(move |sent, total| {
        if total > 0 {
            ui2.status(&format!("Saving {label} to SignFlow… {}%", sent * 100 / total));
        }
    });
    let url = match ctx.blob.upload(&token, snap, progress).await {
        Ok(u) => u,
        Err(BlobError::TooBig { .. }) => {
            ui.status(&format!("Editing {name} — too big for SignFlow"));
            ui.tell(&format!("{name} is now bigger than 500MB, so SignFlow can't take it. It's safe on this computer."));
            *pending = None;
            return SaveOutcome::Waiting;
        }
        Err(e) if e.is_retryable() => {
            ui.status(&format!("Editing {name} — can't reach SignFlow, will try again"));
            return SaveOutcome::Retry;
        }
        Err(e) => {
            log::warn(&format!("save: upload refused: {e:?}"));
            ui.status(&format!("Editing {name} — the upload was refused, will try again"));
            return SaveOutcome::Retry;
        }
    };
    loop {
        match ctx.api.replace(key, &url, choice).await {
            Ok(Saved::Replaced { version }) => {
                notes.base_version = version;
                notes.saved_hash = hash.to_string();
                let _ = local_notes(ctx, notes);
                *pending = None;
                ui.status(&format!("Saved to SignFlow at {}", now_hhmm()));
                log::info("save: saved to SignFlow");
                return SaveOutcome::Saved;
            }
            Ok(Saved::Copy { copy_name, .. }) => {
                notes.saved_hash = hash.to_string();
                let _ = local_notes(ctx, notes);
                *pending = None;
                ui.status(&format!("Saved your version as {copy_name}"));
                log::info("save: saved as a copy");
                return SaveOutcome::Saved;
            }
            Ok(Saved::KeptTheirs { .. }) => {
                *pending = None;
                ui.tell(&format!(
                    "Kept their version of {name} in SignFlow. Your changes are still in {name} on this computer. To work on their version, close {name}{}, then open it again from SignFlow.",
                    words::IN_EDITOR
                ));
                ui.status("Not editing");
                log::info("save: kept theirs");
                return SaveOutcome::Ended(Ended::KeptTheirs);
            }
            Err(ApiError::Stale(info)) if choice.is_none() => {
                let q = Question::from(name, &info);
                log::info("save: stale; asking her");
                match ui.ask(q.clone()).await.ok().flatten() {
                    Some(c) => {
                        choice = Some(c);
                        continue;
                    }
                    None => {
                        log::info("save: stale question closed without a choice");
                        ui.status(&format!("Editing {name} — {} saved a newer version; not saved yet", q.who()));
                        return SaveOutcome::Waiting;
                    }
                }
            }
            Err(e) => return api_trouble(ctx, e),
        }
    }
}

/// "Keep theirs" before anything was sent: her copy moves aside under a new
/// name (never deleted), and theirs is downloaded into place.
async fn keep_theirs_first(ctx: &Ctx, key: &str, local: &Local, name: &str, notes: &mut Notes, pending: &mut Option<String>) -> SaveOutcome {
    let ui = ctx.ui.clone();
    if !watcher::released(&local.path) {
        ui.tell(&format!("Close {name}{} first, then open it again from SignFlow to get their version. Your file hasn't been changed.", words::IN_EDITOR));
        return SaveOutcome::Ended(Ended::KeptTheirs);
    }
    let aside = local.aside_name(&stamp_for_name());
    if std::fs::rename(&local.path, &aside).is_err() {
        ui.tell(&format!("Close {name}{} first, then open it again from SignFlow to get their version. Your file hasn't been changed.", words::IN_EDITOR));
        return SaveOutcome::Ended(Ended::KeptTheirs);
    }
    match download(ctx, key, local, name).await {
        Ok((hash, version)) => {
            notes.saved_hash = hash;
            notes.base_version = version;
            let _ = local_notes(ctx, notes);
            *pending = None;
            ui.tell(&format!("Opened their version of {name}. Your version is kept beside it as {}.", aside.file_name().unwrap_or_default().to_string_lossy()));
            let _ = (ctx.open)(&local.path);
            SaveOutcome::Saved
        }
        Err(msg) => {
            ui.tell(&msg);
            SaveOutcome::Ended(Ended::KeptTheirs)
        }
    }
}

fn local_notes(ctx: &Ctx, notes: &Notes) -> std::io::Result<()> {
    match Local::for_file(&ctx.home, &notes.file_id, &notes.file_name) {
        Some(l) => l.write_notes(notes),
        None => Ok(()),
    }
}

fn api_trouble(ctx: &Ctx, e: ApiError) -> SaveOutcome {
    log::warn(&format!("save: SignFlow answered {e:?}"));
    if e.is_retryable() {
        ctx.ui.status("Can't reach SignFlow — your file is safe here; trying again");
        return SaveOutcome::Retry;
    }
    match e {
        ApiError::Refused { status: 401 | 403, message } => SaveOutcome::Ended(Ended::EndedBySignFlow(message)),
        other => {
            ctx.ui.tell(&other.message());
            SaveOutcome::Waiting
        }
    }
}

fn blob_message(e: &BlobError) -> String {
    match e {
        BlobError::NotAllowed => "SignFlow sent a download link this helper won't use. Nothing was changed.".into(),
        BlobError::TooBig { .. } => "This file is bigger than SignFlow takes.".into(),
        BlobError::Disk(d) => format!("The helper couldn't write the file on this computer ({d})."),
        BlobError::Refused(m) => m.clone(),
        BlobError::Unreachable(_) => "Can't reach the file store right now. Open the file again from SignFlow in a moment.".into(),
    }
}

async fn heartbeat(ctx: Arc<Ctx>, key: String, name: String, every: Duration, end: mpsc::Sender<String>, mut stop: watch::Receiver<bool>) {
    let mut failing = false;
    loop {
        tokio::select! {
            r = stop.changed() => { if r.is_err() || *stop.borrow() { return; } }
            _ = tokio::time::sleep(every) => {}
        }
        match ctx.api.heartbeat(&key).await {
            Ok(()) => {
                if failing {
                    failing = false;
                    ctx.ui.status(&format!("Editing {name}"));
                }
            }
            Err(e) if e.is_retryable() => {
                failing = true;
                ctx.ui.status("Can't reach SignFlow — your file is safe here; trying again");
            }
            Err(e) => {
                let _ = end.send(e.message()).await;
                return;
            }
        }
    }
}
