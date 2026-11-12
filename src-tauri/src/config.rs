//! What is built into the helper. SignFlow's address and the file store's
//! upload address are fixed when the helper is built; nothing in a link, a
//! file or a server answer can change where the helper talks to or uploads.
//!
//! The release build uses the defaults below. The end-to-end test build sets
//! the two `SIGNFLOW_HELPER_*` variables at build time to a throwaway local
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

/// The helper's own folder for the files it opens, its log and its notes
/// about each file (never a key or a token): %LOCALAPPDATA%\SignFlow Helper
/// Files. Deliberately NOT the install folder (%LOCALAPPDATA%\SignFlow
/// Helper), so uninstalling can never take her files with it.
pub fn home_dir() -> std::path::PathBuf {
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

/// The release talks to SignFlow and the store over HTTPS only.
pub fn endpoints_are_safe() -> bool {
    if cfg!(feature = "e2e") {
        return true;
    }
    SIGNFLOW_URL.starts_with("https://") && BLOB_API_URL.starts_with("https://")
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
        }
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
