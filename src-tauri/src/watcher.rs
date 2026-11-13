//! Noticing that Flexi saved. How Flexi saves isn't documented: it may write
//! straight into the file in chunks, or write a temporary file and rename it
//! over the file, leave a .bak beside it, or keep the file open for a moment.
//! So the watcher:
//!   - watches the file's whole folder, and reacts only to the file's own name
//!     (a temporary file renamed onto it counts; the .bak and the temp don't);
//!   - waits until the file's size and modified time stop changing for a
//!     settle period, and until no program has it open for writing;
//!   - reads it and reports new content only when it differs from the last
//!     content it reported (a save with no changes is skipped);
//!   - also looks every few seconds on its own, in case an event is missed.
//! It never writes, moves or deletes the file.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use notify::{RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch};

/// New content, settled: its fingerprint and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How long the file must stay unchanged.
    pub settle: Duration,
    /// How often it is looked at with no event.
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { settle: Duration::from_millis(1500), poll: Duration::from_secs(5) }
    }
}

/// The file's SHA-256, read in 1MB pieces (never all at once).
pub fn fingerprint(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((hex::encode(hasher.finalize()), size))
}

/// The file's size and modified time, or None while it isn't there (a
/// temporary file not renamed onto it yet).
fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// Whether no program has the file open for writing: it can be opened for
/// reading while allowing others only to read.
#[cfg(windows)]
pub fn released(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path).is_ok()
}

/// macOS has no share modes, so ask lsof whether any program has the file
/// open for writing ("w") or reading and writing ("u"). If lsof can't run,
/// the settle period alone decides.
#[cfg(target_os = "macos")]
pub fn released(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    match std::process::Command::new("/usr/sbin/lsof").args(["-F", "a", "--"]).arg(path).output() {
        Ok(out) => !String::from_utf8_lossy(&out.stdout).lines().any(|l| l == "aw" || l == "au"),
        Err(_) => true,
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn released(path: &Path) -> bool {
    path.exists()
}

/// Why a file still counts as open in the program editing it, when she
/// clicks Done (editor_holds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// Another program has the file itself open.
    OpenInProgram,
    /// (macOS) The app this Mac opens the file with is still running. Flexi
    /// for Mac reads the file, lets go of it, and writes it again only when
    /// it closes, so the file alone can't say whether it's still being edited.
    AppRunning(String),
}

/// Whether the program editing the file still has it — so Done waits for it
/// to be saved and closed instead of ending the edit. None when nothing has
/// it (or it can't be told).
///   Windows: any program has the file open (it can't be opened unshared).
///   macOS: another program has the file open (lsof), or the app the Mac
///   uses for this file is running (Launch Services, asked through
///   osascript's AppKit bridge: no Apple events, so no permission prompt).
#[cfg(windows)]
pub fn editor_holds(path: &Path) -> Option<Hold> {
    use std::os::windows::fs::OpenOptionsExt;
    if !path.exists() {
        return None;
    }
    // Opened sharing nothing: refused while any program has it open.
    match std::fs::OpenOptions::new().read(true).share_mode(0).open(path) {
        Ok(_) => None,
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
        Err(e) if matches!(e.raw_os_error(), Some(32) | Some(33)) => Some(Hold::OpenInProgram),
        Err(_) => None,
    }
}

#[cfg(target_os = "macos")]
pub fn editor_holds(path: &Path) -> Option<Hold> {
    if !path.exists() {
        return None;
    }
    if open_by_another_program(path) {
        return Some(Hold::OpenInProgram);
    }
    mac_app_running_for(path).map(Hold::AppRunning)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn editor_holds(_path: &Path) -> Option<Hold> {
    None
}

/// Whether a program other than this helper has the file open, in any way.
#[cfg(target_os = "macos")]
fn open_by_another_program(path: &Path) -> bool {
    let me = std::process::id().to_string();
    match std::process::Command::new("/usr/sbin/lsof").args(["-F", "p", "--"]).arg(path).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.strip_prefix('p')).any(|pid| pid != me),
        Err(_) => false,
    }
}

/// The bundle id of the app this Mac opens `path` with, when it is running.
#[cfg(target_os = "macos")]
fn mac_app_running_for(path: &Path) -> Option<String> {
    const SCRIPT: &str = r#"ObjC.import("AppKit");
function run(argv) {
  const app = $.NSWorkspace.sharedWorkspace.URLForApplicationToOpenURL($.NSURL.fileURLWithPath(argv[0]));
  if (!app || app.isNil()) return "none";
  const bundle = $.NSBundle.bundleWithURL(app);
  if (!bundle || bundle.isNil() || bundle.bundleIdentifier.isNil()) return "none";
  const id = ObjC.unwrap(bundle.bundleIdentifier);
  return ($.NSRunningApplication.runningApplicationsWithBundleIdentifier(id).count > 0 ? "running " : "stopped ") + id;
}"#;
    let out = run_for_at_most(
        std::process::Command::new("/usr/bin/osascript").args(["-l", "JavaScript", "-e", SCRIPT]).arg(path),
        Duration::from_secs(5),
    )?;
    let id = out.trim().strip_prefix("running ")?.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// A command's output, or None if it fails or takes longer than `limit`.
#[cfg(target_os = "macos")]
fn run_for_at_most(cmd: &mut std::process::Command, limit: Duration) -> Option<String> {
    use std::io::Read as _;
    let mut child = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut s = String::new();
                child.stdout.take()?.read_to_string(&mut s).ok()?;
                return Some(s);
            }
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn same_name(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) => x.to_string_lossy().eq_ignore_ascii_case(&y.to_string_lossy()),
        _ => false,
    }
}

/// Watches `path` until `stop` turns true, sending each settled new content
/// to `out`. `known` is the content already in SignFlow (what she opened).
pub async fn watch_file(path: PathBuf, known: String, timing: Timing, out: mpsc::Sender<Settled>, mut stop: watch::Receiver<bool>) -> notify::Result<()> {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let (poke_tx, mut poke_rx) = mpsc::unbounded_channel::<()>();
    let target = path.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.paths.iter().any(|p| same_name(p, &target)) {
                let _ = poke_tx.send(());
            }
        }
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;

    let mut reported = known;
    let mut last_stamp = stamp(&path);
    loop {
        // Wait for a sign of change: an event, or the periodic look.
        let mut dirty = false;
        tokio::select! {
            r = stop.changed() => { if r.is_err() || *stop.borrow() { break; } }
            got = poke_rx.recv() => { if got.is_none() { break; } dirty = true; }
            _ = tokio::time::sleep(timing.poll) => { dirty = stamp(&path) != last_stamp; }
        }
        if !dirty {
            continue;
        }
        // Settle: the same size and time across a whole settle period, the
        // file there, and nobody writing to it.
        let mut before = stamp(&path);
        loop {
            tokio::select! {
                r = stop.changed() => { if r.is_err() || *stop.borrow() { return Ok(()); } }
                _ = tokio::time::sleep(timing.settle) => {}
            }
            while poke_rx.try_recv().is_ok() {}
            let now = stamp(&path);
            if now.is_some() && now == before && released(&path) {
                break;
            }
            before = now;
        }
        last_stamp = before;
        let p = path.clone();
        let read = tokio::task::spawn_blocking(move || fingerprint(&p)).await;
        if let Ok(Ok((hash, size))) = read {
            if hash != reported {
                reported = hash.clone();
                if out.send(Settled { hash, size }).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(watcher);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Instant;

    const FAST: Timing = Timing { settle: Duration::from_millis(400), poll: Duration::from_secs(2) };

    struct Rig {
        _dir: tempfile::TempDir,
        path: PathBuf,
        rx: mpsc::Receiver<Settled>,
        stop: watch::Sender<bool>,
    }

    async fn rig(initial: &[u8]) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Concept.fs");
        std::fs::write(&path, initial).unwrap();
        let (known, _) = fingerprint(&path).unwrap();
        let (tx, rx) = mpsc::channel(8);
        let (stop, stop_rx) = watch::channel(false);
        tokio::spawn(watch_file(path.clone(), known, FAST, tx, stop_rx));
        tokio::time::sleep(Duration::from_millis(300)).await;
        Rig { _dir: dir, path, rx, stop }
    }

    async fn next(rx: &mut mpsc::Receiver<Settled>, within: Duration) -> Option<Settled> {
        tokio::time::timeout(within, rx.recv()).await.ok().flatten()
    }

    fn hash_of(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[tokio::test]
    async fn flexi_writes_a_temp_file_then_renames_it_over_the_file() {
        let mut r = rig(b"version 1").await;
        let started = Instant::now();
        let tmp = r.path.with_file_name("Concept_fs.tmp");
        std::fs::write(&tmp, b"version 2, written to a temp file first").unwrap();
        std::fs::rename(&tmp, &r.path).unwrap();
        let got = next(&mut r.rx, Duration::from_secs(10)).await.expect("noticed");
        println!("[watcher] temp file then rename: noticed {} ms after the rename", started.elapsed().as_millis());
        assert_eq!(got.hash, hash_of(b"version 2, written to a temp file first"));
        assert!(next(&mut r.rx, Duration::from_secs(2)).await.is_none(), "once, not once per event");
        r.stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn flexi_writes_the_file_in_chunks() {
        let mut r = rig(b"version 1").await;
        let started = Instant::now();
        let mut f = std::fs::OpenOptions::new().write(true).truncate(true).open(&r.path).unwrap();
        let mut all = Vec::new();
        for i in 0..6u8 {
            let chunk = vec![b'a' + i; 64 * 1024];
            f.write_all(&chunk).unwrap();
            f.flush().unwrap();
            all.extend_from_slice(&chunk);
            std::thread::sleep(Duration::from_millis(250));
        }
        drop(f);
        let finished = started.elapsed();
        let got = next(&mut r.rx, Duration::from_secs(10)).await.expect("noticed");
        println!("[watcher] chunks for {} ms: noticed {} ms after the last chunk, one upload of the whole file", finished.as_millis(), (started.elapsed() - finished).as_millis());
        assert_eq!(got, Settled { hash: hash_of(&all), size: all.len() as u64 }, "the whole file, not a half-written one");
        assert!(next(&mut r.rx, Duration::from_secs(2)).await.is_none());
        r.stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn a_bak_beside_the_file_is_ignored() {
        let mut r = rig(b"version 1").await;
        std::fs::copy(&r.path, r.path.with_file_name("Concept.fs.bak")).unwrap();
        std::fs::write(r.path.with_file_name("Concept.bak"), b"old").unwrap();
        assert!(next(&mut r.rx, Duration::from_secs(3)).await.is_none(), "a .bak alone is not a save");
        let started = Instant::now();
        std::fs::write(&r.path, b"version 2 after the backup").unwrap();
        let got = next(&mut r.rx, Duration::from_secs(10)).await.expect("noticed");
        println!("[watcher] .bak beside it: the .bak was ignored; the save was noticed {} ms later", started.elapsed().as_millis());
        assert_eq!(got.hash, hash_of(b"version 2 after the backup"));
        r.stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn waits_while_flexi_holds_the_file_open() {
        let mut r = rig(b"version 1").await;
        let mut f = std::fs::OpenOptions::new().write(true).truncate(true).open(&r.path).unwrap();
        f.write_all(b"version 2, still open").unwrap();
        f.flush().unwrap();
        let held = Instant::now();
        assert!(next(&mut r.rx, Duration::from_secs(4)).await.is_none(), "not while it is still open for writing");
        drop(f);
        let released_at = held.elapsed();
        let got = next(&mut r.rx, Duration::from_secs(10)).await.expect("noticed");
        println!("[watcher] held open for {} ms: nothing sent while open; noticed {} ms after it was let go", released_at.as_millis(), (held.elapsed() - released_at).as_millis());
        assert_eq!(got.hash, hash_of(b"version 2, still open"));
        r.stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn a_mac_style_save_into_a_hidden_temp_file_then_rename() {
        // How macOS apps usually save: a hidden temporary beside the file (or
        // in a temporary folder), then a rename over the original.
        let mut r = rig(b"version 1").await;
        let started = Instant::now();
        let tmp = r.path.with_file_name(".Concept.fs.sb-1a2b3c4d-XyZ123");
        std::fs::write(&tmp, b"version 2, saved the Mac way").unwrap();
        std::fs::rename(&tmp, &r.path).unwrap();
        let got = next(&mut r.rx, Duration::from_secs(10)).await.expect("noticed");
        println!("[watcher] hidden temp file then rename (Mac): noticed {} ms after the rename", started.elapsed().as_millis());
        assert_eq!(got.hash, hash_of(b"version 2, saved the Mac way"));
        assert!(next(&mut r.rx, Duration::from_secs(2)).await.is_none(), "once");
        r.stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn ds_store_and_tilde_files_are_ignored() {
        let mut r = rig(b"version 1").await;
        let dir = r.path.parent().unwrap().to_path_buf();
        std::fs::write(dir.join(".DS_Store"), b"Finder's notes").unwrap();
        std::fs::write(dir.join("Concept.fs~"), b"an editor's backup").unwrap();
        std::fs::write(dir.join("~$Concept.fs"), b"an owner file").unwrap();
        std::fs::write(dir.join("~Concept.fs.tmp"), b"a scratch file").unwrap();
        assert!(next(&mut r.rx, Duration::from_secs(4)).await.is_none(), ".DS_Store and ~ files are not saves");
        std::fs::write(&r.path, b"version 2").unwrap();
        let got = next(&mut r.rx, Duration::from_secs(10)).await.expect("the real save is still noticed");
        assert_eq!(got.hash, hash_of(b"version 2"));
        println!("[watcher] .DS_Store and ~ files beside it: ignored; the real save was noticed");
        r.stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn a_save_with_no_changes_is_skipped() {
        let mut r = rig(b"version 1").await;
        std::fs::write(&r.path, b"version 1").unwrap();
        assert!(next(&mut r.rx, Duration::from_secs(4)).await.is_none(), "same content: nothing to upload");
        std::fs::write(&r.path, b"version 2").unwrap();
        assert!(next(&mut r.rx, Duration::from_secs(10)).await.is_some());
        std::fs::write(&r.path, b"version 2").unwrap();
        assert!(next(&mut r.rx, Duration::from_secs(4)).await.is_none(), "saved again unchanged: skipped");
        println!("[watcher] unchanged saves: skipped");
        r.stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn the_watcher_never_touches_the_file() {
        let mut r = rig(b"version 1").await;
        std::fs::write(&r.path, b"version 2").unwrap();
        next(&mut r.rx, Duration::from_secs(10)).await.expect("noticed");
        r.stop.send(true).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(std::fs::read(&r.path).unwrap(), b"version 2");
        let names: Vec<_> = std::fs::read_dir(r.path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("Concept.fs")]);
    }

    /// Done waits while the program editing the file still has it.
    #[test]
    fn a_file_another_program_holds_open_counts_as_still_being_edited() {
        let dir = tempfile::tempdir().unwrap();
        // An extension no app claims, so on a Mac only the file itself decides.
        let p = dir.path().join("Held.sfhtest");
        std::fs::write(&p, b"version 1").unwrap();
        assert_eq!(editor_holds(&p), None, "nobody has it");
        assert_eq!(editor_holds(&dir.path().join("missing.sfhtest")), None, "a file that isn't there");

        #[cfg(windows)]
        {
            // Any open handle counts (Flexi, or here the test itself).
            let f = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
            assert_eq!(editor_holds(&p), Some(Hold::OpenInProgram));
            drop(f);
        }
        #[cfg(target_os = "macos")]
        {
            // Another program holding it open (this helper's own reads don't count).
            let mut holder = std::process::Command::new("/bin/sh").args(["-c", "exec 3<>\"$0\"; exec sleep 30"]).arg(&p).spawn().unwrap();
            let start = Instant::now();
            while editor_holds(&p).is_none() && start.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(editor_holds(&p), Some(Hold::OpenInProgram), "held by another program");
            let _own = std::fs::File::open(&p).unwrap();
            assert_eq!(editor_holds(&p), Some(Hold::OpenInProgram));
            holder.kill().unwrap();
            holder.wait().unwrap();
            let _ = _own;
            assert_eq!(editor_holds(&p), None, "the helper's own open file alone doesn't count");
        }
        assert_eq!(editor_holds(&p), None, "let go");
        println!("[watcher] a file held open by another program counts as still being edited; let go, it doesn't");
    }
}
