//! `signflow-edit://` links. A link carries one thing: a one-time pass, made
//! by her signed-in browser in SignFlow and good once for about 2 minutes.
//! Nothing else in a link is read — not an address, not a file, not a name.
//!
//! Accepted, exactly (the scheme and `open` in any case, the pass as is):
//!   signflow-edit://open?pass=<43 characters of A-Z a-z 0-9 - _>
//!   signflow-edit://open/?pass=<…>            (some browsers add the slash)
//! Anything else is refused: another action, another or an extra parameter,
//! a fragment, a port, a user, an encoded character, a pass of another length.

use crate::config::LINK_SCHEME;

/// What a valid link asks: open the file this pass is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenLink {
    pub pass: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// Not a signflow-edit link at all.
    NotOurs,
    /// A signflow-edit link, but not one the helper understands.
    Malformed,
}

/// The length of a pass: 32 random bytes in base64url, no padding.
const PASS_LEN: usize = 43;
/// Longer than any valid link; anything past it is refused unread.
const MAX_LINK_LEN: usize = 128;

pub fn parse(raw: &str) -> Result<OpenLink, LinkError> {
    if raw.len() > MAX_LINK_LEN || !raw.is_ascii() {
        return Err(if starts_with_scheme(raw) { LinkError::Malformed } else { LinkError::NotOurs });
    }
    if !starts_with_scheme(raw) {
        return Err(LinkError::NotOurs);
    }
    let rest = &raw[LINK_SCHEME.len() + 3..];
    // The action, in any case, then an optional slash, then the query.
    let after_action = match rest.get(..4) {
        Some(a) if a.eq_ignore_ascii_case("open") => &rest[4..],
        _ => return Err(LinkError::Malformed),
    };
    let query = after_action
        .strip_prefix("/?")
        .or_else(|| after_action.strip_prefix('?'))
        .ok_or(LinkError::Malformed)?;
    let pass = query.strip_prefix("pass=").ok_or(LinkError::Malformed)?;
    if pass.len() != PASS_LEN || !pass.bytes().all(is_pass_byte) {
        return Err(LinkError::Malformed);
    }
    Ok(OpenLink { pass: pass.to_string() })
}

fn starts_with_scheme(raw: &str) -> bool {
    let prefix_len = LINK_SCHEME.len() + 3;
    raw.len() >= prefix_len
        && raw.is_char_boundary(prefix_len)
        && raw[..LINK_SCHEME.len()].eq_ignore_ascii_case(LINK_SCHEME)
        && &raw[LINK_SCHEME.len()..prefix_len] == "://"
}

fn is_pass_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// The first signflow-edit link among a program's arguments (Windows hands a
/// link to the program as an argument), parsed; other arguments are ignored.
pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Option<Result<OpenLink, LinkError>> {
    args.into_iter()
        .find(|a| starts_with_scheme(a.trim_matches('"')))
        .map(|a| parse(a.trim_matches('"')))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcde";

    #[test]
    fn a_pass_is_43_characters() {
        assert_eq!(PASS.len(), 43);
    }

    #[test]
    fn accepts_the_two_forms() {
        let want = Ok(OpenLink { pass: PASS.into() });
        assert_eq!(parse(&format!("signflow-edit://open?pass={PASS}")), want);
        assert_eq!(parse(&format!("signflow-edit://open/?pass={PASS}")), want);
        assert_eq!(parse(&format!("SIGNFLOW-EDIT://OPEN?pass={PASS}")), want, "Windows may change the case of the scheme and host");
    }

    #[test]
    fn the_pass_keeps_its_case() {
        let mixed = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaA";
        assert_eq!(parse(&format!("signflow-edit://open?pass={mixed}")).unwrap().pass, mixed);
    }

    #[test]
    fn refuses_anything_else() {
        let bad = [
            format!("signflow-edit://open?pass={PASS}&x=1"),
            format!("signflow-edit://open?x=1&pass={PASS}"),
            format!("signflow-edit://open?pass={PASS}#frag"),
            format!("signflow-edit://open?pass={PASS}/"),
            format!("signflow-edit://open?pass={}", &PASS[..42]),
            format!("signflow-edit://open?pass={PASS}a"),
            format!("signflow-edit://open?pass={}%41", &PASS[..40]),
            format!("signflow-edit://open?pass={}+", &PASS[..42]),
            format!("signflow-edit://open?pass={}=", &PASS[..42]),
            format!("signflow-edit://open?PASS={PASS}"),
            format!("signflow-edit://open?pass= {}", &PASS[..42]),
            format!("signflow-edit://open?url=https://evil.example&pass={PASS}"),
            format!("signflow-edit://openx?pass={PASS}"),
            format!("signflow-edit://download?pass={PASS}"),
            format!("signflow-edit://open/file?pass={PASS}"),
            format!("signflow-edit://open//?pass={PASS}"),
            format!("signflow-edit://user@open?pass={PASS}"),
            format!("signflow-edit://open:8080?pass={PASS}"),
            "signflow-edit://open".to_string(),
            "signflow-edit://open?pass=".to_string(),
            "signflow-edit://".to_string(),
            format!("signflow-edit://open?pass={}é", &PASS[..42]),
            format!("signflow-edit://open?pass={PASS}{}", "a".repeat(200)),
        ];
        for link in bad {
            assert_eq!(parse(&link), Err(LinkError::Malformed), "{link}");
        }
    }

    #[test]
    fn other_schemes_are_not_ours() {
        for link in [
            format!("https://signflow-neon.vercel.app/?pass={PASS}"),
            format!("signflow-edits://open?pass={PASS}"),
            format!("signflow://open?pass={PASS}"),
            format!(" signflow-edit://open?pass={PASS}"),
            format!("signflow-edit:open?pass={PASS}"),
            format!("signflow-edit:/open?pass={PASS}"),
            String::new(),
            "x".into(),
        ] {
            assert_eq!(parse(&link), Err(LinkError::NotOurs), "{link}");
        }
    }

    #[test]
    fn finds_the_link_among_arguments() {
        let args = vec!["C:\\Program\\helper.exe".to_string(), "--flag".into(), format!("\"signflow-edit://open?pass={PASS}\"")];
        assert_eq!(from_args(args), Some(Ok(OpenLink { pass: PASS.into() })));
        assert_eq!(from_args(vec!["helper.exe".to_string()]), None);
        assert_eq!(from_args(vec![format!("signflow-edit://open?pass=short")]), Some(Err(LinkError::Malformed)));
    }
}
