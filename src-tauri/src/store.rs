//! Where an opened file lives on her computer, and the helper's notes about
//! it: %LOCALAPPDATA%\SignFlow Helper\files\<file id>\<file name>, beside a
//! notes file. The notes say which SignFlow version she has and what was last
//! saved to SignFlow — never a pass, a key, a token or a link.
//!
//! The helper never deletes her file. It replaces it only with a fresh
//! download, and only when everything in it is already in SignFlow; when it
//! has to make way for someone else's version it moves hers aside under a new
//! name first.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Notes {
    pub file_id: String,
    pub file_name: String,
    /// The SignFlow version her copy is based on (SignFlow's version tag).
    pub base_version: String,
    /// The fingerprint of the content last in SignFlow from this computer
    /// (or downloaded from it).
    pub saved_hash: String,
}

#[derive(Debug, Clone)]
pub struct Local {
    pub dir: PathBuf,
    pub path: PathBuf,
    notes_path: PathBuf,
}

/// A file id from SignFlow: letters and digits only (SignFlow's cuid ids).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A file name that is safe as one Windows file name: no folders, no
/// characters Windows refuses, no reserved device names, not too long.
pub fn safe_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    out = out.trim().trim_end_matches(['.', ' ']).to_string();
    if out.is_empty() {
        out = "file.fs".into();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    if reserved.contains(&stem.as_str()) {
        out = format!("_{out}");
    }
    if out.chars().count() > 150 {
        let ext: String = out.rsplit_once('.').map(|(_, e)| format!(".{e}")).filter(|e| e.len() <= 10).unwrap_or_default();
        out = out.chars().take(150 - ext.chars().count()).collect::<String>() + &ext;
    }
    out
}

impl Local {
    pub fn for_file(home: &Path, file_id: &str, file_name: &str) -> Option<Local> {
        if !valid_id(file_id) {
            return None;
        }
        let dir = home.join("files").join(file_id);
        Some(Local { path: dir.join(safe_name(file_name)), notes_path: dir.join(".signflow-notes.json"), dir })
    }

    pub fn read_notes(&self) -> Option<Notes> {
        let text = std::fs::read_to_string(&self.notes_path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write_notes(&self, notes: &Notes) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.notes_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(notes).unwrap())?;
        std::fs::rename(&tmp, &self.notes_path)
    }

    /// Where a download lands before it is moved into place.
    pub fn download_temp(&self) -> PathBuf {
        self.dir.join(format!(".download-{}.part", std::process::id()))
    }

    /// The helper's own copy of the file for one upload, so what is sent is
    /// exactly one saved state even if Flexi saves again meanwhile.
    pub fn upload_snapshot(&self) -> PathBuf {
        self.dir.join(format!(".upload-{}.part", std::process::id()))
    }

    /// A free name beside the file for her version when it must make way:
    /// "Concept (not in SignFlow, 2026-11-12 1405).fs".
    pub fn aside_name(&self, when: &str) -> PathBuf {
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
            _ => (name.clone(), String::new()),
        };
        for n in 1..1000 {
            let suffix = if n == 1 { String::new() } else { format!(" {n}") };
            let candidate = self.dir.join(format!("{stem} (not in SignFlow, {when}{suffix}){ext}"));
            if !candidate.exists() {
                return candidate;
            }
        }
        self.dir.join(format!("{stem} (not in SignFlow, {when} {}){ext}", std::process::id()))
    }
}

/// What to do with her copy when an edit starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// No copy here: download it.
    Download,
    /// Her copy is SignFlow's version exactly: use it as it is.
    UseAsIs,
    /// Her copy is all in SignFlow but an older version: download the newer
    /// one over it (nothing of hers is lost).
    Refresh,
    /// Her copy has changes not in SignFlow, on the version SignFlow has now:
    /// upload them.
    UploadPending,
    /// Her copy has changes not in SignFlow, and somebody saved a newer
    /// version since (or the notes are gone): she chooses first.
    Conflict,
}

/// The rule for the above, from her copy's fingerprint (None: no copy), the
/// notes, and SignFlow's version now.
pub fn decide(current_hash: Option<&str>, notes: Option<&Notes>, server_version: &str) -> Start {
    let Some(hash) = current_hash else { return Start::Download };
    let Some(notes) = notes else { return Start::Conflict };
    let clean = notes.saved_hash == hash;
    let same_version = notes.base_version == server_version;
    match (clean, same_version) {
        (true, true) => Start::UseAsIs,
        (true, false) => Start::Refresh,
        (false, true) => Start::UploadPending,
        (false, false) => Start::Conflict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("Lobby sign.fs"), "Lobby sign.fs");
        assert_eq!(safe_name("..\\..\\Windows\\evil.fs"), ".._.._Windows_evil.fs");
        assert_eq!(safe_name("a/b:c*?.fs"), "a_b_c__.fs");
        assert_eq!(safe_name("CON.fs"), "_CON.fs");
        assert_eq!(safe_name("trailing. "), "trailing");
        assert_eq!(safe_name(""), "file.fs");
        let long = format!("{}.fs", "x".repeat(300));
        let s = safe_name(&long);
        assert_eq!(s.chars().count(), 150);
        assert!(s.ends_with(".fs"));
    }

    #[test]
    fn only_signflow_ids_make_folders() {
        assert!(valid_id("cmwd6zuys00011mv8oinmamub"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
        assert!(Local::for_file(Path::new("C:/h"), "..", "x.fs").is_none());
        let l = Local::for_file(Path::new("C:/h"), "abc123", "Lobby sign.fs").unwrap();
        assert!(l.path.ends_with("files/abc123/Lobby sign.fs") || l.path.ends_with("files\\abc123\\Lobby sign.fs"));
    }

    #[test]
    fn notes_round_trip_and_hold_no_secret() {
        let dir = tempfile::tempdir().unwrap();
        let l = Local::for_file(dir.path(), "abc", "Lobby.fs").unwrap();
        assert_eq!(l.read_notes(), None);
        let n = Notes { file_id: "abc".into(), file_name: "Lobby.fs".into(), base_version: "v-1".into(), saved_hash: "h".into() };
        l.write_notes(&n).unwrap();
        assert_eq!(l.read_notes(), Some(n));
        let text = std::fs::read_to_string(l.dir.join(".signflow-notes.json")).unwrap();
        for word in ["key", "pass", "token", "http"] {
            assert!(!text.to_lowercase().contains(word), "{word}");
        }
    }

    #[test]
    fn the_start_rule() {
        let n = Notes { file_id: "a".into(), file_name: "x.fs".into(), base_version: "v-1".into(), saved_hash: "h1".into() };
        assert_eq!(decide(None, Some(&n), "v-1"), Start::Download);
        assert_eq!(decide(Some("h1"), Some(&n), "v-1"), Start::UseAsIs);
        assert_eq!(decide(Some("h1"), Some(&n), "v-2"), Start::Refresh);
        assert_eq!(decide(Some("h2"), Some(&n), "v-1"), Start::UploadPending);
        assert_eq!(decide(Some("h2"), Some(&n), "v-2"), Start::Conflict);
        assert_eq!(decide(Some("h2"), None, "v-1"), Start::Conflict);
    }

    #[test]
    fn a_name_aside_never_takes_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let l = Local::for_file(dir.path(), "abc", "Lobby.fs").unwrap();
        std::fs::create_dir_all(&l.dir).unwrap();
        let first = l.aside_name("2026-11-12 1405");
        assert!(first.ends_with("Lobby (not in SignFlow, 2026-11-12 1405).fs"));
        std::fs::write(&first, b"x").unwrap();
        let second = l.aside_name("2026-11-12 1405");
        assert!(second.ends_with("Lobby (not in SignFlow, 2026-11-12 1405 2).fs"));
    }
}
