//! The e2e build only (the `e2e` feature): a release build doesn't contain
//! this, and tools/check-test-hooks.mjs checks that it doesn't.
//!
//! The e2e runs talk to the helper through two small files in its folder:
//! e2e-control.txt (done, check, update, not-now…) and e2e-choice.txt (the
//! answer to a stale question). A run writes each one whole under a temporary
//! name, renames it into place, and ends its word with a line break. The helper
//! still never acts on a file it catches half written: an empty file, or a
//! word with no line break after it yet, is left where it is and read again on
//! the next look. (A Windows run once lost its Done: the helper read the file
//! in the instant between its creation and its word, then deleted it.)

use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum TestFile {
    /// No file (or it couldn't be read just now): why.
    Missing(String),
    /// There, but empty or not finished yet: what it holds so far.
    Unfinished(String),
    /// A whole word, ended by a line break.
    Word(String),
}

/// What a test file's bytes say.
pub fn parse(bytes: &[u8]) -> TestFile {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    let word = text.trim();
    if text.ends_with('\n') && !word.is_empty() {
        TestFile::Word(word.to_string())
    } else {
        TestFile::Unfinished(text.to_string())
    }
}

/// Reads a test file.
pub fn read(path: &Path) -> TestFile {
    match std::fs::read(path) {
        Ok(bytes) => parse(&bytes),
        Err(e) => TestFile::Missing(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn word(w: &str) -> TestFile {
        TestFile::Word(w.into())
    }

    #[test]
    fn a_whole_word_ended_by_a_line_break_is_read() {
        assert_eq!(parse(b"done\n"), word("done"));
        assert_eq!(parse(b"done\r\n"), word("done"), "a Windows line break");
        assert_eq!(parse("\u{feff}done\n".as_bytes()), word("done"), "a byte-order mark is not part of the word");
        assert_eq!(parse(b"upload-mine-as-newest\n"), word("upload-mine-as-newest"));
    }

    #[test]
    fn an_empty_or_half_written_file_is_left_for_the_next_look() {
        // The lost Done: the file existed, with nothing in it yet.
        assert_eq!(parse(b""), TestFile::Unfinished("".into()));
        assert_eq!(parse(b"\n"), TestFile::Unfinished("\n".into()), "a line break alone is no word");
        assert_eq!(parse(b"  \r\n"), TestFile::Unfinished("  \r\n".into()));
        assert_eq!(parse(b"do"), TestFile::Unfinished("do".into()));
        assert_eq!(parse(b"done"), TestFile::Unfinished("done".into()), "no line break yet: it may still grow");
        // "check" is a word on its own, and also the start of "check-auto".
        assert_eq!(parse(b"check"), TestFile::Unfinished("check".into()));
        assert_eq!(parse(b"check-auto\n"), word("check-auto"));
        // Cut inside a character.
        assert!(matches!(parse(&"not-now\u{2026}".as_bytes()[..9]), TestFile::Unfinished(_)));
    }

    #[test]
    fn a_file_read_while_it_is_written_only_counts_once_it_is_whole() {
        let dir = std::env::temp_dir().join(format!("sfh-e2e-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("e2e-control.txt");
        let _ = std::fs::remove_file(&f);
        assert!(matches!(read(&f), TestFile::Missing(_)));

        // The old way of writing: created first, the word after.
        let mut w = std::fs::File::create(&f).unwrap();
        assert_eq!(read(&f), TestFile::Unfinished("".into()));
        w.write_all(b"done").unwrap();
        w.flush().unwrap();
        assert_eq!(read(&f), TestFile::Unfinished("done".into()));
        w.write_all(b"\n").unwrap();
        drop(w);
        assert_eq!(read(&f), word("done"));
        std::fs::remove_file(&f).unwrap();

        // The runs' way: whole under a temporary name, then renamed into place.
        let tmp = dir.join("e2e-control.txt.writing");
        std::fs::write(&tmp, b"check-auto\n").unwrap();
        assert!(matches!(read(&f), TestFile::Missing(_)), "nothing to read until the rename");
        std::fs::rename(&tmp, &f).unwrap();
        assert_eq!(read(&f), word("check-auto"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
