//! What is built into the helper. SignFlow's address and the file store's
//! upload address are fixed when the helper is built; nothing in a link, a
//! file or a server answer can change where the helper talks to or uploads.
//!
//! The release build uses the defaults below. The end-to-end test build sets
//! the `SIGNFLOW_HELPER_*` variables at build time to a throwaway local
//! stand-in, and turns on the `e2e` feature; it is never released.

use url::Url;

/// SignFlow, the only server the helper's API calls go to.
pub const SIGNFLOW_URL: &str = match option_env!("SIGNFLOW_HELPER_BASE_URL") {
    Some(v) => v,
    None => "https://signflow-neon.vercel.app",
};

/// Vercel Blob's upload API: where the one new file goes, with SignFlow's
/// single-use token.
pub const BLOB_API_URL: &str = match option_env!("SIGNFLOW_HELPER_BLOB_API_URL") {
    Some(v) => v,
    None => "https://vercel.com/api/blob",
};

/// Where the helper asks whether a newer version is out: latest.json of the
/// newest PUBLISHED release of the helper's public GitHub repository (GitHub
/// never answers this address with a draft or a pre-release). Read every six
/// hours and when she asks; it is only a pointer, and nothing it names is
/// installed unless its signature checks out against the public key built
/// into the helper (tauri.conf.json, plugins.updater.pubkey).
pub const UPDATE_MANIFEST_URL: &str = match option_env!("SIGNFLOW_HELPER_UPDATE_URL") {
    Some(v) => v,
    None => "https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/latest/download/latest.json",
};

/// The only place a release build downloads an update from: a file of one of
/// this repository's own releases.
const UPDATE_DOWNLOAD_PREFIX: &str = "https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/";

/// The Blob API version @vercel/blob 2.7 sends (`x-api-version`).
pub const BLOB_API_VERSION: &str = "12";

/// Uploads above this go in parts (the browser's own threshold in SignFlow).
pub const MULTIPART_THRESHOLD: u64 = 10 * 1024 * 1024;
/// One part, as @vercel/blob sends them.
pub const PART_SIZE: usize = 8 * 1024 * 1024;
/// The largest file SignFlow takes.
pub const MAX_FILE_BYTES: u64 = 500 * 1024 * 1024;

/// The links the helper answers to.
pub const LINK_SCHEME: &str = "signflow-edit";

/// Words that differ by computer. Windows keeps its wording exactly; a Mac
/// has no Flexi, so the file opens in whatever app the Mac uses for it.
pub mod words {
    #[cfg(target_os = "macos")]
    pub const IN_EDITOR: &str = "";
    #[cfg(not(target_os = "macos"))]
    pub const IN_EDITOR: &str = " in Flexi";
    #[cfg(target_os = "macos")]
    pub const COMPUTER: &str = "Your Mac";
    #[cfg(not(target_os = "macos"))]
    pub const COMPUTER: &str = "Windows";
    #[cfg(target_os = "macos")]
    pub const SET_UP_EDITOR: &str = "Make sure an app on this Mac is set to open .fs files.";
    #[cfg(not(target_os = "macos"))]
    pub const SET_UP_EDITOR: &str = "Make sure Flexi is installed and set as the program for .fs files.";
    #[cfg(target_os = "macos")]
    pub const MENU_PLACE: &str = "in the menu bar";
    #[cfg(not(target_os = "macos"))]
    pub const MENU_PLACE: &str = "by the clock";
}

/// The helper's own folder for the files it opens, its log and its notes
/// about each file (never a key or a token). Deliberately NOT where the app
/// is installed, so uninstalling can never take her files with it:
///   Windows: %LOCALAPPDATA%\SignFlow Helper Files (the app is in
///            %LOCALAPPDATA%\SignFlow Helper)
///   macOS:   ~/Library/Application Support/SignFlow Helper Files (the app is
///            in /Applications)
pub fn home_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home).join("Library/Application Support/SignFlow Helper Files");
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("SignFlow Helper Files")
}

/// Whether a download link SignFlow handed back points somewhere the helper
/// will fetch from: Vercel Blob over HTTPS (the release), or the local
/// stand-in store (the e2e build only).
pub fn download_url_allowed(url: &Url) -> bool {
    if cfg!(feature = "e2e") {
        return url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    }
    url.scheme() == "https"
        && url
            .host_str()
            .map(|h| h.ends_with(".blob.vercel-storage.com"))
            .unwrap_or(false)
}

/// Whether an update latest.json points to is one the helper will download:
/// a file of this repository's own GitHub releases over HTTPS (the release),
/// or the local stand-in (the e2e build only). Checked before anything is
/// downloaded; the signature is checked after.
pub fn update_download_allowed(url: &Url) -> bool {
    if cfg!(feature = "e2e") {
        return url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    }
    // Url has already resolved any "." and ".." in the path.
    url.as_str().starts_with(UPDATE_DOWNLOAD_PREFIX) && url.query().is_none() && url.fragment().is_none()
}

/// The release talks to SignFlow, the store and GitHub over HTTPS only.
pub fn endpoints_are_safe() -> bool {
    if cfg!(feature = "e2e") {
        return true;
    }
    SIGNFLOW_URL.starts_with("https://") && BLOB_API_URL.starts_with("https://") && UPDATE_MANIFEST_URL.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_defaults_are_https() {
        if !cfg!(feature = "e2e") {
            assert!(endpoints_are_safe());
            assert_eq!(SIGNFLOW_URL, "https://signflow-neon.vercel.app");
            assert_eq!(BLOB_API_URL, "https://vercel.com/api/blob");
            assert_eq!(UPDATE_MANIFEST_URL, "https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/latest/download/latest.json");
        }
    }

    #[test]
    fn updates_only_from_this_repositorys_releases() {
        if cfg!(feature = "e2e") {
            return;
        }
        let ok = |u: &str| update_download_allowed(&Url::parse(u).unwrap());
        assert!(ok("https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/v0.1.4/SignFlow-Helper_0.1.4_x64-setup.exe"));
        assert!(ok("https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/v0.1.4/SignFlow-Helper_0.1.4_universal.app.tar.gz"));
        assert!(!ok("http://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/v0.1.4/x.exe"));
        assert!(!ok("https://github.com/someone-else/signflow-helper/releases/download/v0.1.4/x.exe"));
        assert!(!ok("https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/../../../someone-else/x/releases/download/v1/x.exe"));
        assert!(!ok("https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/%2e%2e/%2e%2e/x.exe"));
        assert!(!ok("https://github.com.evil.example/ahayrulabusiness-dotcom/signflow-helper/releases/download/v0.1.4/x.exe"));
        assert!(!ok("https://github.com/ahayrulabusiness-dotcom/signflow-helper/releases/download/v0.1.4/x.exe?y=1"));
        assert!(!ok("http://127.0.0.1:4702/x.exe"));
    }

    #[test]
    fn downloads_only_from_the_file_store() {
        if cfg!(feature = "e2e") {
            return;
        }
        let ok = Url::parse("https://abc.private.blob.vercel-storage.com/jobs/x/y/Lobby%20sign.fs?sig=1").unwrap();
        let old = Url::parse("https://abc.public.blob.vercel-storage.com/x.fs").unwrap();
        let http = Url::parse("http://abc.private.blob.vercel-storage.com/x.fs").unwrap();
        let other = Url::parse("https://evil.example/blob.vercel-storage.com/x.fs").unwrap();
        let tricky = Url::parse("https://blob.vercel-storage.com.evil.example/x.fs").unwrap();
        assert!(download_url_allowed(&ok));
        assert!(download_url_allowed(&old));
        assert!(!download_url_allowed(&http));
        assert!(!download_url_allowed(&other));
        assert!(!download_url_allowed(&tricky));
    }
}
