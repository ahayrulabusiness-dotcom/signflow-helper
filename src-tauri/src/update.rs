//! Updates: whether a newer SignFlow Helper is out, and installing it when
//! she says so — never while a file is open for editing.
//!
//! Tauri's updater plugin (tauri-plugin-updater 2.14.1) does the work. What
//! it does, read from its source:
//!  - Check: one GET of latest.json (config::UPDATE_MANIFEST_URL). It takes
//!    this computer's entry — windows-x86_64-nsis, else windows-x86_64; on a
//!    Mac darwin-<arch>-app, else darwin-<arch> — and it is an update only
//!    when its version is higher (`newer` below decides).
//!  - Download: the whole file into memory. Then its minisign signature is
//!    checked against the public key built into the helper, and so is the
//!    version the file was signed for (requireSignedVersion). Nothing is
//!    written anywhere before that.
//!  - Install, Windows: the installer is written to %TEMP% and started with
//!    "/P /UPDATE /R" (P: a progress bar and no questions; UPDATE: no new
//!    shortcuts; R: start the helper again when done), then the helper exits.
//!    The installer closes the helper if it's still running, installs for
//!    this user only (no administrator) and starts the new helper.
//!  - Install, macOS: the .app.tar.gz is unpacked beside the app (same disk)
//!    and swapped with the installed app in one step, so there's never no
//!    app; if that fails the installed app stays. Then the helper starts
//!    itself again (`open -n`).
//!
//! Neither touches the helper's own folder (her files, its log, its notes).
//! This module holds the rules (versions, what to call a failure, snoozing,
//! when installing must wait, the words); app.rs does the window and menu.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::words;

/// The first check, a little after start (so starting isn't slowed).
pub const FIRST_CHECK_AFTER: Duration = Duration::from_secs(10);
/// Then every six hours.
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// "Not now": no window for a day. The dot and the menu still say it.
pub const SNOOZE_SECS: u64 = 24 * 3600;
const NOTES_MAX_CHARS: usize = 400;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether `remote` is an update for `current`: a higher version, and not a
/// pre-release (GitHub's "latest" never is one; this makes sure).
pub fn newer(remote: &semver::Version, current: &semver::Version) -> bool {
    remote.pre.is_empty() && remote > current
}

/// The same, for version text ("v" allowed). Anything that isn't a version
/// is never newer.
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some(r), Some(c)) => newer(&r, &c),
        _ => false,
    }
}

fn parse_version(v: &str) -> Option<semver::Version> {
    semver::Version::parse(v.trim().trim_start_matches('v')).ok()
}

fn same_version(a: &str, b: &str) -> bool {
    matches!((parse_version(a), parse_version(b)), (Some(x), Some(y)) if x == y)
}

/// The release notes as the window shows them: plain text, at most a few
/// lines.
pub fn short_notes(notes: Option<&str>) -> String {
    let text: String = notes
        .unwrap_or("")
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect();
    let mut lines: Vec<&str> = text.lines().map(str::trim).collect();
    lines.dedup_by(|a, b| a.is_empty() && b.is_empty());
    let text = lines.join("\n").trim().to_string();
    if text.chars().count() <= NOTES_MAX_CHARS {
        return text;
    }
    let cut: String = text.chars().take(NOTES_MAX_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// What went wrong with an update, as she hears it. Every one of them leaves
/// the installed helper as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Couldn't connect at all.
    NoSignal,
    /// The connection dropped part-way through the download.
    Interrupted,
    /// The download address answered with an error.
    Unavailable,
    /// The file's signature (or the version it was signed for) didn't check out.
    BadSignature,
    DiskFull,
    InstallFailed,
    /// macOS: the swap failed and the old app couldn't be put back; it's here.
    MovedAside(PathBuf),
    /// macOS: running from the .dmg or Downloads, where updates can't reach.
    NotInApplications,
}

impl Failure {
    pub fn message(&self, current: &str) -> String {
        let still = format!("Nothing was changed: SignFlow Edit {current} still works.");
        match self {
            Failure::NoSignal => format!("Couldn't reach the internet to download the update. {still} Try again when you're online."),
            Failure::Interrupted => format!("The download stopped before it finished. {still} Try again."),
            Failure::Unavailable => format!("The update couldn't be downloaded right now. {still} Try again later."),
            Failure::BadSignature => format!("The update didn't pass its safety check, so it wasn't installed. {still}"),
            Failure::DiskFull => format!("There isn't enough free space on this computer for the update. {still} Free up some space, then try again."),
            Failure::InstallFailed => format!("The update couldn't be installed. {still}"),
            Failure::MovedAside(p) => format!(
                "The update couldn't be installed, and SignFlow Helper was moved to {}. Drag it back into your Applications folder, then open it.",
                p.display()
            ),
            Failure::NotInApplications => {
                "Drag SignFlow Helper into your Applications folder and open it from there, then update. Updates can't reach it where it is now.".into()
            }
        }
    }

    /// For the log.
    pub fn name(&self) -> &'static str {
        match self {
            Failure::NoSignal => "no signal",
            Failure::Interrupted => "interrupted",
            Failure::Unavailable => "unavailable",
            Failure::BadSignature => "bad signature",
            Failure::DiskFull => "disk full",
            Failure::InstallFailed => "install failed",
            Failure::MovedAside(_) => "old app moved aside",
            Failure::NotInApplications => "not in Applications",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Download,
    Install,
}

/// Names what went wrong. `received` is how much of the file arrived.
pub fn classify(e: &tauri_plugin_updater::Error, stage: Stage, received: u64) -> Failure {
    use tauri_plugin_updater::Error as E;
    match e {
        E::Minisign(_) | E::Base64(_) | E::SignatureUtf8(_) | E::SignedVersionMismatch { .. } | E::MissingSignedVersion => Failure::BadSignature,
        E::PreviousAppNotRestored(p) => Failure::MovedAside(p.clone()),
        E::Io(io) if is_disk_full(io) => Failure::DiskFull,
        // "Download request failed with status: …"
        E::Network(_) => Failure::Unavailable,
        E::Reqwest(r) => {
            if received > 0 || r.is_body() || r.is_decode() {
                Failure::Interrupted
            } else {
                Failure::NoSignal
            }
        }
        _ if stage == Stage::Download => Failure::Interrupted,
        _ => Failure::InstallFailed,
    }
}

/// Whether an error means the disk is full, wherever it is in the chain (an
/// unpacking error wraps the one from the disk).
pub fn is_disk_full(e: &std::io::Error) -> bool {
    fn code_is_full(code: i32) -> bool {
        // ERROR_HANDLE_DISK_FULL, ERROR_DISK_FULL
        #[cfg(windows)]
        return code == 39 || code == 112;
        // ENOSPC, EDQUOT
        #[cfg(target_os = "macos")]
        return code == 28 || code == 69;
        #[cfg(not(any(windows, target_os = "macos")))]
        return code == 28 || code == 122;
    }
    fn io_is_full(e: &std::io::Error) -> bool {
        format!("{:?}", e.kind()) == "StorageFull" || e.raw_os_error().is_some_and(code_is_full)
    }
    if io_is_full(e) {
        return true;
    }
    let mut next: Option<&(dyn std::error::Error + 'static)> = e.get_ref().map(|inner| inner as &(dyn std::error::Error + 'static));
    while let Some(err) = next {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            if io_is_full(io) {
                return true;
            }
        }
        next = err.source();
    }
    false
}

/// What the helper remembers about updates, in its own folder (update.json
/// beside helper.log; never her files).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Saved {
    /// "Not now": no window before this (seconds since 1970).
    pub snoozed_until: u64,
    /// Written just before installing, so the next start can tell whether
    /// the update took.
    pub installing: Option<Installing>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installing {
    pub from: String,
    pub to: String,
    pub at: u64,
}

impl Saved {
    pub fn path(home: &Path) -> PathBuf {
        home.join("update.json")
    }

    pub fn read(home: &Path) -> Saved {
        std::fs::read(Self::path(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn write(&self, home: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(home)?;
        let tmp = home.join("update.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, Self::path(home))
    }

    pub fn snooze(&mut self, now: u64) {
        self.snoozed_until = now + SNOOZE_SECS;
    }

    pub fn snoozed(&self, now: u64) -> bool {
        now < self.snoozed_until
    }
}

/// On start: did the last update take?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterRestart {
    Nothing,
    Updated { from: String, to: String },
    DidNotInstall { to: String },
}

pub fn after_restart(saved: &Saved, current: &str, now: u64) -> AfterRestart {
    match &saved.installing {
        Some(i) if now.saturating_sub(i.at) <= 24 * 3600 => {
            if same_version(current, &i.to) {
                AfterRestart::Updated { from: i.from.clone(), to: i.to.clone() }
            } else if same_version(current, &i.from) {
                AfterRestart::DidNotInstall { to: i.to.clone() }
            } else {
                AfterRestart::Nothing
            }
        }
        _ => AfterRestart::Nothing,
    }
}

/// Where updates stand.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    /// Just restarted into this version.
    Updated(String),
    CheckFailed,
    Available { version: String, notes: String },
    Downloading { version: String, notes: String, percent: u8 },
    /// Downloaded and checked; installs as soon as the edit ends.
    WaitingForEdit { version: String, notes: String },
    Installing { version: String, notes: String },
    Failed { version: String, notes: String, failure: Failure },
}

impl Phase {
    /// The update this is about, if any.
    pub fn update(&self) -> Option<(&str, &str)> {
        match self {
            Phase::Available { version, notes }
            | Phase::Downloading { version, notes, .. }
            | Phase::WaitingForEdit { version, notes }
            | Phase::Installing { version, notes }
            | Phase::Failed { version, notes, .. } => Some((version, notes)),
            _ => None,
        }
    }

    /// Whether the tray picture carries the dot: a newer version is known
    /// and not yet installed.
    pub fn dot(&self) -> bool {
        matches!(self, Phase::Available { .. } | Phase::Downloading { .. } | Phase::WaitingForEdit { .. } | Phase::Failed { .. })
    }

    /// Downloading, waiting or installing: a check doesn't interrupt it.
    pub fn busy(&self) -> bool {
        matches!(self, Phase::Downloading { .. } | Phase::WaitingForEdit { .. } | Phase::Installing { .. })
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Newer { version: String, notes: String },
    NotNewer,
    Failed,
}

/// A check's answer, applied. Never over a download, a wait or an install;
/// a failed check doesn't forget an update already known.
pub fn after_check(current: &Phase, found: Found) -> Phase {
    if current.busy() {
        return current.clone();
    }
    match found {
        Found::Newer { version, notes } => Phase::Available { version, notes },
        Found::NotNewer => match current {
            Phase::Updated(v) => Phase::Updated(v.clone()),
            _ => Phase::UpToDate,
        },
        Found::Failed if current.dot() => current.clone(),
        Found::Failed => Phase::CheckFailed,
    }
}

/// "Update" (or "Try again") may start a download now.
pub fn may_start_update(current: &Phase) -> bool {
    matches!(current, Phase::Available { .. } | Phase::Failed { .. })
}

/// The download is in hand and checked: install now, or — while she is
/// editing — wait for the edit to end.
pub fn after_download(current: &Phase, editing: bool) -> Option<Phase> {
    let Phase::Downloading { version, notes, .. } = current else { return None };
    let (version, notes) = (version.clone(), notes.clone());
    Some(if editing { Phase::WaitingForEdit { version, notes } } else { Phase::Installing { version, notes } })
}

/// The edit ended: a waiting update installs now.
pub fn after_edit(current: &Phase) -> Option<Phase> {
    match current {
        Phase::WaitingForEdit { version, notes } => Some(Phase::Installing { version: version.clone(), notes: notes.clone() }),
        _ => None,
    }
}

/// Whether a link may start an edit now (not while an update installs).
pub fn may_start_edit(current: &Phase) -> bool {
    !matches!(current, Phase::Installing { .. })
}

/// Whether finding an update opens the window by itself. "Check for
/// updates now" always does; otherwise not while she's editing (the window
/// waits for Done) and not for a day after "Not now".
pub fn pop_up(asked: bool, editing: bool, snoozed: bool) -> bool {
    asked || (!editing && !snoozed)
}

/// The update line in the tray menu, and whether it can be clicked.
pub fn menu_line(phase: &Phase, current: &str) -> (String, bool) {
    match phase {
        Phase::Idle => (format!("SignFlow Edit {current}"), false),
        Phase::Checking => ("Checking for updates…".into(), false),
        Phase::UpToDate => (format!("Up to date (version {current})"), false),
        Phase::Updated(v) => (format!("Updated to version {v}"), false),
        Phase::CheckFailed => ("Couldn't check for updates".into(), false),
        Phase::Available { version, .. } | Phase::Failed { version, .. } => (format!("Update to {version}"), true),
        Phase::Downloading { version, percent, .. } => (format!("Downloading {version}… {percent}%"), false),
        Phase::WaitingForEdit { version, .. } => (format!("Update to {version} after this edit"), true),
        Phase::Installing { version, .. } => (format!("Installing {version}…"), false),
    }
}

/// What the update window says, and its buttons as [label, what it does].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub title: String,
    pub message: String,
    pub notes: String,
    pub percent: Option<u8>,
    pub primary: Option<(String, String)>,
    pub secondary: Option<(String, String)>,
}

fn button(label: &str, choice: &str) -> Option<(String, String)> {
    Some((label.into(), choice.into()))
}

pub fn view(phase: &Phase, current: &str) -> View {
    let mut v = View { title: String::new(), message: String::new(), notes: String::new(), percent: None, primary: None, secondary: None };
    match phase {
        Phase::Available { version, notes } => {
            v.title = format!("SignFlow Edit {version} is available");
            v.message = format!("You have version {current}. Updating takes about a minute, and SignFlow Edit starts again by itself.");
            v.notes = notes.clone();
            v.primary = button("Update", "update");
            v.secondary = button("Not now", "later");
        }
        Phase::Downloading { version, percent, .. } => {
            v.title = format!("Updating to SignFlow Edit {version}");
            v.message = format!("Downloading… {percent}%");
            v.percent = Some(*percent);
            v.secondary = button("Hide", "hide");
        }
        Phase::WaitingForEdit { version, .. } => {
            v.title = "Finish your edit first".into();
            v.message = format!(
                "SignFlow Edit {version} is downloaded and checked. It installs and starts again as soon as you click Done in the SignFlow Helper menu ({}). Your file isn't touched.",
                words::MENU_PLACE
            );
            v.primary = button("OK", "ok");
        }
        Phase::Installing { version, .. } => {
            v.title = format!("Installing SignFlow Edit {version}");
            v.message = "SignFlow Edit starts again by itself in a moment.".into();
        }
        Phase::Failed { version, failure, .. } => {
            v.title = format!("SignFlow Edit {version} wasn't installed");
            v.message = failure.message(current);
            v.primary = button("Try again", "retry");
            v.secondary = button("Close", "close");
        }
        other => {
            v.title = format!("SignFlow Edit {current}");
            v.message = menu_line(other, current).0;
            v.primary = button("Close", "close");
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use tauri_plugin_updater::UpdaterExt;

    // ---- versions

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.1.4", "0.1.3"));
        assert!(is_newer("v0.1.4", "0.1.3"));
        assert!(is_newer("0.2.0", "0.1.30"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.3", "0.1.3"), "the same version is not an update");
        assert!(!is_newer("0.1.2", "0.1.3"), "never a downgrade");
        assert!(!is_newer("0.1.4-beta.1", "0.1.3"), "never a pre-release");
        assert!(!is_newer("latest", "0.1.3"));
        assert!(!is_newer("", "0.1.3"));
        assert!(!is_newer("0.1.4", "nonsense"));
    }

    // ---- latest.json, both platforms (the real v0.1.2 file's shape)

    const REL: &str = "https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/v0.1.2";

    fn real_manifest() -> serde_json::Value {
        let exe = format!("{REL}/SignFlow-Helper_0.1.2_x64-setup.exe");
        let mac = format!("{REL}/SignFlow-Helper_0.1.2_universal.app.tar.gz");
        serde_json::json!({
            "version": "0.1.2",
            "notes": "SignFlow Helper 0.1.2",
            "pub_date": "2026-11-13T01:15:25Z",
            "platforms": {
                "windows-x86_64-nsis": { "signature": "c2lnLWV4ZQ==", "url": exe },
                "windows-x86_64": { "signature": "c2lnLWV4ZQ==", "url": exe },
                "darwin-aarch64-app": { "signature": "c2lnLW1hYw==", "url": mac },
                "darwin-aarch64": { "signature": "c2lnLW1hYw==", "url": mac },
                "darwin-x86_64-app": { "signature": "c2lnLW1hYw==", "url": mac },
                "darwin-x86_64": { "signature": "c2lnLW1hYw==", "url": mac }
            }
        })
    }

    #[test]
    fn latest_json_for_both_platforms() {
        let release: tauri_plugin_updater::RemoteRelease = serde_json::from_value(real_manifest()).unwrap();
        assert_eq!(release.version.to_string(), "0.1.2");
        assert_eq!(release.notes.as_deref(), Some("SignFlow Helper 0.1.2"));
        for t in ["windows-x86_64-nsis", "windows-x86_64"] {
            let url = release.download_url(t).unwrap();
            assert!(url.as_str().ends_with("/SignFlow-Helper_0.1.2_x64-setup.exe"), "{t}: {url}");
            assert_eq!(release.signature(t).unwrap(), "c2lnLWV4ZQ==");
            if !cfg!(feature = "e2e") {
                assert!(crate::config::update_download_allowed(url), "{t}");
            }
        }
        // Apple Silicon and Intel Macs both get the universal app.
        for t in ["darwin-aarch64-app", "darwin-aarch64", "darwin-x86_64-app", "darwin-x86_64"] {
            let url = release.download_url(t).unwrap();
            assert!(url.as_str().ends_with("/SignFlow-Helper_0.1.2_universal.app.tar.gz"), "{t}: {url}");
            assert_eq!(release.signature(t).unwrap(), "c2lnLW1hYw==");
        }
        assert!(release.download_url("linux-x86_64").is_err(), "no entry, no download");
    }

    #[test]
    fn latest_json_that_isnt_one() {
        for bad in [
            serde_json::json!({ "notes": "no version", "platforms": {} }),
            serde_json::json!({ "version": "soon", "platforms": {} }),
            serde_json::json!({ "version": "0.1.4" }),
            serde_json::json!({ "version": "0.1.4", "pub_date": "yesterday", "platforms": {} }),
            serde_json::json!({ "version": "0.1.4", "platforms": { "windows-x86_64": { "signature": "x", "url": "not a url" } } }),
        ] {
            assert!(serde_json::from_value::<tauri_plugin_updater::RemoteRelease>(bad.clone()).is_err(), "{bad}");
        }
    }

    // ---- the plugin itself, on a mock app, with the test key pair and
    //      signatures published in tauri-plugin-updater 2.14.1's own tests
    //      (payload "test", signed for version 2.0.0).

    const TEST_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEE0OUVGNDc0NkZBNkU2RDcKUldUWDVxWnZkUFNlcERHV2cyYno4bjZBclFuQk13Z1FLMWdlVm9zb3JjYkx6Y3VuRCtLSitjWWU=";
    const TEST_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUWDVxWnZkUFNlcFBGMnpzanZQL0wvVFF0QkxSbldSdmFxKzRvbUsxVUV1Z3RRcC9VU3JHaUkxdjc2MnNEeTNpZDNhb3I3YlJDOUVUeWVvdDZsTUhIcldvWjVoalVNMVFjPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkxNjQ5MDUxCWZpbGU6cGF5bG9hZAl2ZXJzaW9uOjIuMC4wCkREcTJDTE9FbDVjUW1DMmVKUkhsblpQVlZpT1pzdTFDanBQS1FvZWk0QS9YQmZrRHFHTzBCK1FNcmNySGdFUTVxVjNtbmU3NXRkRldoSnBxVVhoOUFBPT0=";
    const UNVERSIONED_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXkgRTc2MjBGMTg0MkI0RTgxRgpSV1FmNkxSQ0dBOWk1M21sWWVjTzRJelQ1MVRHUHB2V3VjTlNDaDFDQk0wUVRhTG43M1k3R0ZPMw==";
    const UNVERSIONED_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIG1pbmlzaWduIHNlY3JldCBrZXkKUlVRZjZMUkNHQTlpNTU5cjNnN1YxcU55SkRBcEdpcDhNZnFjYWRJZ1Q5Q3VoVjNFTWhIb04xbUdUa1VpZEYvejdTcmxRZ1hkeThvZmpiN2JOSkp5bERPb2NyQ284S0x6WndvPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNjMzNzAwODM1CWZpbGU6dGVzdAlwcmVoYXNoZWQKd0xNRGp5OUZMQXV4WjNxNE5sRXZrZ3R5aHJyMGd0VHU2S0M0S0JKZElUYmJPZUFpMXpCSVlvMHY0aVRndDhqSnBJaWRSSm5wOTRBQlFrSkFnQW9vQlE9PQ==";

    /// The helper's updater settings (requireSignedVersion on) on a mock app;
    /// the app is returned too so it outlives the updater.
    fn updater(endpoint: &str, target: &str, pubkey: &str) -> (tauri::App<tauri::test::MockRuntime>, tauri_plugin_updater::Updater) {
        let mut ctx = tauri::test::mock_context(tauri::test::noop_assets());
        ctx.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({ "pubkey": TEST_PUBKEY, "requireSignedVersion": true, "dangerousInsecureTransportProtocol": true }),
        );
        let app = tauri::test::mock_builder().plugin(tauri_plugin_updater::Builder::new().build()).build(ctx).unwrap();
        let updater = app
            .handle()
            .updater_builder()
            .pubkey(pubkey)
            .target(target)
            .endpoints(vec![endpoint.parse().unwrap()])
            .unwrap()
            .version_comparator(|current, release| newer(&release.version, &current))
            .build()
            .unwrap();
        (app, updater)
    }

    fn manifest(version: &str, url: &str, signature: &str) -> serde_json::Value {
        serde_json::json!({ "version": version, "notes": "Faster saves.", "platforms": { "test-target": { "url": url, "signature": signature } } })
    }

    #[test]
    fn a_tampered_file_is_refused_before_anything_is_installed() {
        let (_app, u) = updater("https://example.invalid/latest.json", "test-target", TEST_PUBKEY);
        let update = u.restore_update(manifest("2.0.0", "https://example.invalid/f", TEST_SIG)).unwrap().unwrap();
        update.verify(b"test").expect("the signed file checks out");
        let e = update.verify(b"tampered").unwrap_err();
        assert_eq!(classify(&e, Stage::Download, 8), Failure::BadSignature);
        // install() checks again first, so a tampered file is never handed on.
        let e = update.install(b"tampered").unwrap_err();
        assert_eq!(classify(&e, Stage::Install, 8), Failure::BadSignature);
    }

    #[test]
    fn a_file_signed_for_another_version_is_refused() {
        let (_app, u) = updater("https://example.invalid/latest.json", "test-target", TEST_PUBKEY);
        // latest.json says 3.0.0, but the file was signed as 2.0.0.
        let update = u.restore_update(manifest("3.0.0", "https://example.invalid/f", TEST_SIG)).unwrap().unwrap();
        let e = update.verify(b"test").unwrap_err();
        assert!(matches!(e, tauri_plugin_updater::Error::SignedVersionMismatch { .. }), "{e}");
        assert_eq!(classify(&e, Stage::Download, 4), Failure::BadSignature);
    }

    #[test]
    fn a_signature_without_its_version_is_refused() {
        let (_app, u) = updater("https://example.invalid/latest.json", "test-target", UNVERSIONED_PUBKEY);
        let update = u.restore_update(manifest("2.0.0", "https://example.invalid/f", UNVERSIONED_SIG)).unwrap().unwrap();
        let e = update.verify(b"test").unwrap_err();
        assert_eq!(classify(&e, Stage::Download, 4), Failure::BadSignature);
    }

    #[test]
    fn a_signature_from_another_key_is_refused() {
        let (_app, u) = updater("https://example.invalid/latest.json", "test-target", UNVERSIONED_PUBKEY);
        let update = u.restore_update(manifest("2.0.0", "https://example.invalid/f", TEST_SIG)).unwrap().unwrap();
        let e = update.verify(b"test").unwrap_err();
        assert_eq!(classify(&e, Stage::Download, 4), Failure::BadSignature);
    }

    #[test]
    fn the_same_or_an_older_version_is_not_an_update() {
        let (_app, u) = updater("https://example.invalid/latest.json", "test-target", TEST_PUBKEY);
        // The mock app is version 0.1.0.
        assert!(u.restore_update(manifest("0.1.0", "https://example.invalid/f", TEST_SIG)).unwrap().is_none());
        assert!(u.restore_update(manifest("0.0.9", "https://example.invalid/f", TEST_SIG)).unwrap().is_none());
        assert!(u.restore_update(manifest("0.2.0-beta.1", "https://example.invalid/f", TEST_SIG)).unwrap().is_none());
        assert!(u.restore_update(manifest("0.2.0", "https://example.invalid/f", TEST_SIG)).unwrap().is_some());
    }

    /// A tiny HTTP server: each request gets what `reply` says for its path.
    #[derive(Clone)]
    enum Reply {
        Body(Vec<u8>),
        /// Promises `promised` bytes, sends `sent`, then drops the connection.
        Cut { promised: usize, sent: usize },
        Status(u16),
    }

    fn serve(reply: impl Fn(&str) -> Reply + Send + Sync + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&req).lines().next().unwrap_or("").to_string();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                match reply(&path) {
                    Reply::Body(b) => {
                        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len());
                        let _ = s.write_all(&b);
                    }
                    Reply::Cut { promised, sent } => {
                        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {promised}\r\nConnection: close\r\n\r\n");
                        let _ = s.write_all(&vec![7u8; sent]);
                        let _ = s.flush();
                    }
                    Reply::Status(code) => {
                        let _ = write!(s, "HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            }
        });
        base
    }

    /// A port nothing listens on.
    fn closed_port() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        format!("http://{addr}")
    }

    async fn download_from(file: Reply, url_of: impl Fn(&str) -> String) -> (Result<Vec<u8>, tauri_plugin_updater::Error>, u64) {
        let base = serve(move |path| {
            if path == "/latest.json" {
                Reply::Body(Vec::new())
            } else {
                file.clone()
            }
        });
        let (_app, u) = updater(&format!("{base}/latest.json"), "test-target", TEST_PUBKEY);
        let update = u.restore_update(manifest("2.0.0", &url_of(&base), TEST_SIG)).unwrap().unwrap();
        let mut got = 0u64;
        let r = update.download(|n, _| got += n as u64, || {}).await;
        (r, got)
    }

    #[tokio::test]
    async fn download_the_signed_file() {
        let (r, got) = download_from(Reply::Body(b"test".to_vec()), |b| format!("{b}/f")).await;
        assert_eq!(r.unwrap(), b"test");
        assert_eq!(got, 4);
    }

    #[tokio::test]
    async fn download_a_tampered_file() {
        let (r, _) = download_from(Reply::Body(b"tampered".to_vec()), |b| format!("{b}/f")).await;
        assert_eq!(classify(&r.unwrap_err(), Stage::Download, 8), Failure::BadSignature);
    }

    #[tokio::test]
    async fn download_interrupted() {
        let (r, got) = download_from(Reply::Cut { promised: 200_000, sent: 50_000 }, |b| format!("{b}/f")).await;
        let e = r.unwrap_err();
        assert!(got > 0, "part of it arrived ({got} bytes)");
        assert_eq!(classify(&e, Stage::Download, got), Failure::Interrupted, "{e}");
    }

    #[tokio::test]
    async fn download_with_no_signal() {
        let dead = closed_port();
        let (r, got) = download_from(Reply::Status(500), move |_| format!("{dead}/f")).await;
        let e = r.unwrap_err();
        assert_eq!(got, 0);
        assert_eq!(classify(&e, Stage::Download, got), Failure::NoSignal, "{e}");
    }

    #[tokio::test]
    async fn download_missing() {
        let (r, got) = download_from(Reply::Status(404), |b| format!("{b}/f")).await;
        assert_eq!(classify(&r.unwrap_err(), Stage::Download, got), Failure::Unavailable);
    }

    #[tokio::test]
    async fn check_reads_latest_json_and_fails_quietly() {
        let body = serde_json::to_vec(&manifest("2.0.0", "http://127.0.0.1:9/f", TEST_SIG)).unwrap();
        let base = serve(move |_| Reply::Body(body.clone()));
        let (_app, u) = updater(&format!("{base}/latest.json"), "test-target", TEST_PUBKEY);
        let found = u.check().await.unwrap().expect("2.0.0 is newer than 0.1.0");
        assert_eq!(found.version, "2.0.0");
        assert_eq!(short_notes(found.body.as_deref()), "Faster saves.");
        // A release build won't download from anywhere but this repository.
        assert_eq!(crate::config::update_download_allowed(&found.download_url), cfg!(feature = "e2e"));

        let (_app2, u2) = updater(&format!("{}/latest.json", closed_port()), "test-target", TEST_PUBKEY);
        assert!(u2.check().await.is_err());
        assert_eq!(after_check(&Phase::Idle, Found::Failed), Phase::CheckFailed);
        assert_eq!(menu_line(&Phase::CheckFailed, "0.1.3").0, "Couldn't check for updates");
    }

    // ---- disk full and install failures

    #[derive(Debug)]
    struct Wrapped(std::io::Error);
    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "failed to unpack the app")
        }
    }
    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn disk_full_is_named_wherever_it_hides() {
        #[cfg(windows)]
        let codes = [112, 39];
        #[cfg(not(windows))]
        let codes = [28, 28];
        for code in codes {
            let e = tauri_plugin_updater::Error::Io(std::io::Error::from_raw_os_error(code));
            assert_eq!(classify(&e, Stage::Install, 100), Failure::DiskFull);
            let wrapped = std::io::Error::other(Wrapped(std::io::Error::from_raw_os_error(code)));
            assert!(is_disk_full(&wrapped));
            let e = tauri_plugin_updater::Error::Io(wrapped);
            assert_eq!(classify(&e, Stage::Install, 100), Failure::DiskFull);
        }
        assert!(!is_disk_full(&std::io::Error::from_raw_os_error(2)));
        let e = tauri_plugin_updater::Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(classify(&e, Stage::Install, 100), Failure::InstallFailed);
        assert_eq!(classify(&tauri_plugin_updater::Error::InvalidUpdaterFormat, Stage::Install, 100), Failure::InstallFailed);
        let kept = PathBuf::from("/Applications/SignFlow Helper (previous version).app");
        assert_eq!(classify(&tauri_plugin_updater::Error::PreviousAppNotRestored(kept.clone()), Stage::Install, 100), Failure::MovedAside(kept));
    }

    #[test]
    fn every_failure_says_the_installed_version_still_works() {
        for f in [Failure::NoSignal, Failure::Interrupted, Failure::Unavailable, Failure::BadSignature, Failure::DiskFull, Failure::InstallFailed] {
            let m = f.message("0.1.3");
            assert!(m.contains("Nothing was changed: SignFlow Edit 0.1.3 still works."), "{m}");
        }
        assert!(Failure::DiskFull.message("0.1.3").starts_with("There isn't enough free space"));
        assert!(Failure::BadSignature.message("0.1.3").contains("didn't pass its safety check"));
        assert!(Failure::MovedAside(PathBuf::from("/Applications/X (previous version).app")).message("0.1.3").contains("/Applications/X (previous version).app"));
    }

    // ---- snooze

    #[test]
    fn not_now_snoozes_for_a_day_and_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Saved::read(dir.path());
        assert_eq!(s, Saved::default());
        let now = 1_800_000_000;
        assert!(!s.snoozed(now));
        s.snooze(now);
        s.write(dir.path()).unwrap();
        let s = Saved::read(dir.path());
        assert!(s.snoozed(now + 1));
        assert!(s.snoozed(now + SNOOZE_SECS - 1));
        assert!(!s.snoozed(now + SNOOZE_SECS));
        // Automatic checks don't pop up while snoozed; the dot and menu still show it.
        assert!(!pop_up(false, false, s.snoozed(now + 60)));
        assert!(pop_up(true, false, s.snoozed(now + 60)), "Check for updates now always shows it");
        assert!(pop_up(false, false, s.snoozed(now + SNOOZE_SECS)), "after a day it may show once more");
        let found = after_check(&Phase::Idle, Found::Newer { version: "0.1.4".into(), notes: String::new() });
        assert!(found.dot());
        assert_eq!(menu_line(&found, "0.1.3"), ("Update to 0.1.4".into(), true));
        // A broken file doesn't lose the snooze, or anything else.
        std::fs::write(Saved::path(dir.path()), b"{ not json").unwrap();
        assert_eq!(Saved::read(dir.path()), Saved::default());
    }

    // ---- the "editing, so wait" rule

    #[test]
    fn never_installs_while_she_is_editing() {
        let available = Phase::Available { version: "0.1.4".into(), notes: "n".into() };
        assert!(may_start_update(&available));
        // No pop-up of its own while she's editing.
        assert!(!pop_up(false, true, false));
        let downloading = Phase::Downloading { version: "0.1.4".into(), notes: "n".into(), percent: 100 };
        assert!(!may_start_update(&downloading), "one download at a time");
        // Downloaded while she edits: it waits.
        let waiting = after_download(&downloading, true).unwrap();
        assert_eq!(waiting, Phase::WaitingForEdit { version: "0.1.4".into(), notes: "n".into() });
        assert!(may_start_edit(&waiting), "editing goes on as normal while it waits");
        assert_eq!(view(&waiting, "0.1.3").title, "Finish your edit first");
        assert_eq!(menu_line(&waiting, "0.1.3").0, "Update to 0.1.4 after this edit");
        // A check meanwhile changes nothing.
        assert_eq!(after_check(&waiting, Found::NotNewer), waiting);
        assert_eq!(after_check(&waiting, Found::Failed), waiting);
        // The edit ends: now it installs, and no new edit starts until it has.
        let installing = after_edit(&waiting).unwrap();
        assert_eq!(installing, Phase::Installing { version: "0.1.4".into(), notes: "n".into() });
        assert!(!may_start_edit(&installing));
        // Not editing: it installs straight away.
        assert_eq!(after_download(&downloading, false), Some(installing));
        // Nothing waits, nothing to do when an edit ends.
        assert_eq!(after_edit(&available), None);
    }

    #[test]
    fn after_a_restart() {
        let at = 1_800_000_000;
        let saved = Saved { snoozed_until: 0, installing: Some(Installing { from: "0.1.3".into(), to: "0.1.4".into(), at }) };
        assert_eq!(after_restart(&saved, "0.1.4", at + 60), AfterRestart::Updated { from: "0.1.3".into(), to: "0.1.4".into() });
        assert_eq!(after_restart(&saved, "0.1.3", at + 60), AfterRestart::DidNotInstall { to: "0.1.4".into() });
        assert_eq!(after_restart(&saved, "0.1.4", at + 3 * 24 * 3600), AfterRestart::Nothing, "long ago: say nothing");
        assert_eq!(after_restart(&Saved::default(), "0.1.4", at), AfterRestart::Nothing);
    }

    // ---- the words

    #[test]
    fn the_window_and_menu_words() {
        let available = Phase::Available { version: "0.1.4".into(), notes: "Faster saves.".into() };
        let v = view(&available, "0.1.3");
        assert_eq!(v.title, "SignFlow Edit 0.1.4 is available");
        assert_eq!(v.notes, "Faster saves.");
        assert_eq!(v.primary, Some(("Update".into(), "update".into())));
        assert_eq!(v.secondary, Some(("Not now".into(), "later".into())));
        let d = view(&Phase::Downloading { version: "0.1.4".into(), notes: String::new(), percent: 42 }, "0.1.3");
        assert_eq!((d.percent, d.message.as_str()), (Some(42), "Downloading… 42%"));
        let f = view(&Phase::Failed { version: "0.1.4".into(), notes: String::new(), failure: Failure::Interrupted }, "0.1.3");
        assert!(f.message.starts_with("The download stopped before it finished."));
        assert_eq!(f.primary, Some(("Try again".into(), "retry".into())));
        assert_eq!(menu_line(&Phase::Updated("0.1.4".into()), "0.1.4").0, "Updated to version 0.1.4");
        assert_eq!(menu_line(&Phase::UpToDate, "0.1.3").0, "Up to date (version 0.1.3)");
        assert!(!Phase::UpToDate.dot() && !Phase::CheckFailed.dot() && !Phase::Installing { version: "x".into(), notes: String::new() }.dot());
    }

    #[test]
    fn notes_are_short_plain_text() {
        assert_eq!(short_notes(None), "");
        assert_eq!(short_notes(Some("  One.\r\n\r\n\r\nTwo.\u{7}  ")), "One.\n\nTwo.");
        let long = "x".repeat(1000);
        let s = short_notes(Some(&long));
        assert_eq!(s.chars().count(), NOTES_MAX_CHARS);
        assert!(s.ends_with('…'));
    }
}
