use std::collections::HashMap;

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}

/// These characters draw nothing or reorder the text around them. They are the soft hyphen, the
/// bidirectional controls and isolates, the zero-width characters, the line and paragraph
/// separators, and the byte order mark.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD | 0x061C | 0x180E | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF
            | 0xFFF9..=0xFFFB
    )
}

/// The longest login that is kept from a sign-in attempt, in characters. An escape counts as the
/// characters that it prints.
pub const TYPED_LOGIN_MAX: usize = 64;

/// The text that a visitor typed in the login field. The text is made safe to keep and to show back.
///
/// The text is data from anyone on the network. It never travels raw. Control characters, and
/// characters that hide or reorder text, become a visible `\u{..}` escape. A newline cannot forge
/// a log line. An escape sequence cannot drive a terminal. A right-to-left mark cannot disguise a
/// row. The length is bounded, and the surrounding blanks are removed.
///
/// Quotes and angle brackets stay as typed. They are harmless in the database, because the
/// database always uses a bound parameter. The page escapes them where the page prints them.
pub fn typed_login(input: &str) -> String {
    let mut out = String::new();
    let mut printed = 0usize;
    for c in input.trim().chars() {
        let piece = if c.is_control() || is_invisible_format(c) {
            format!("\\u{{{:x}}}", c as u32)
        } else {
            c.to_string()
        };
        let width = piece.chars().count();
        if printed + width > TYPED_LOGIN_MAX {
            out.push('\u{2026}');
            break;
        }
        out.push_str(&piece);
        printed += width;
    }
    out
}

pub fn percent_encode_query_component(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

pub fn percent_decode_form(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err("truncated percent-encoding".into());
                }
                let hi =
                    from_hex(bytes[i + 1]).ok_or_else(|| "invalid percent-encoding".to_string())?;
                let lo =
                    from_hex(bytes[i + 2]).ok_or_else(|| "invalid percent-encoding".to_string())?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "form field is not valid UTF-8".into())
}

pub fn parse_form(body: &[u8]) -> Result<HashMap<String, String>, String> {
    let raw = std::str::from_utf8(body).map_err(|_| "form body is not valid UTF-8".to_string())?;
    let mut out = HashMap::new();
    if raw.is_empty() {
        return Ok(out);
    }
    for pair in raw.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode_form(key)?;
        let value = percent_decode_form(value)?;
        if key.is_empty() {
            return Err("empty form field name".into());
        }
        if out.insert(key.clone(), value).is_some() {
            return Err(format!("duplicate form field: {key}"));
        }
    }
    Ok(out)
}

/// Tells whether `uri` is a safe return target: a local path and query.
///
/// The function refuses every ASCII control byte (0x00 to 0x1F and 0x7F), in the raw value and in
/// the decoded path. A URL parser of the URL Standard removes TAB, CR and LF before it reads the
/// URL. Thus `/<TAB>/host` becomes `//host`, which is a link to another host.
pub fn validate_return_uri(uri: &str, max_len: usize) -> bool {
    if uri.is_empty() || uri.len() > max_len || !uri.starts_with('/') || uri.starts_with("//") {
        return false;
    }
    if uri.contains('#') || uri.bytes().any(|b| b.is_ascii_control()) {
        return false;
    }
    if uri.contains('\\') {
        return false;
    }

    // Validate the path after one percent-decoding pass as well. This check prevents
    // redirect targets, such as /%2f%2fevil.example or /%5cevil.example, from
    // becoming network-path or backslash URLs. This happens after user-agent normalization.
    let path = path_only(uri);
    let decoded_path = match percent_decode_uri_component(path) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if !decoded_path.starts_with('/')
        || decoded_path.starts_with("//")
        || decoded_path.contains('\\')
        || decoded_path.contains('#')
        || decoded_path.bytes().any(|b| b.is_ascii_control())
    {
        return false;
    }
    true
}

fn percent_decode_uri_component(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("truncated percent-encoding".into());
            }
            let hi =
                from_hex(bytes[i + 1]).ok_or_else(|| "invalid percent-encoding".to_string())?;
            let lo =
                from_hex(bytes[i + 2]).ok_or_else(|| "invalid percent-encoding".to_string())?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "URI component is not valid UTF-8".into())
}

pub fn path_only(uri: &str) -> &str {
    uri.split_once('?').map(|(path, _)| path).unwrap_or(uri)
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `2026-10-10 14:03` (UTC). The image has no time zone database. The pages have no script.
/// Therefore, every time shown is UTC.
pub fn format_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Compute the civil date from a day count. The method is Howard Hinnant's algorithm.
    // The calendar is proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        secs / 3_600,
        (secs % 3_600) / 60
    )
}

/// `2026-10-10 14:03:27` (UTC). The sign-in log uses this format. The seconds tell attempts apart.
pub fn format_utc_secs(unix: i64) -> String {
    format!("{}:{:02}", format_utc(unix), unix.rem_euclid(60))
}

/// Examples of the output: `45 s`, `12 min`, `2 h 5 min`, `3 d 4 h`.
pub fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..=59 => format!("{seconds} s"),
        60..=3_599 => format!("{} min", seconds / 60),
        3_600..=86_399 => {
            let (h, m) = (seconds / 3_600, (seconds % 3_600) / 60);
            if m == 0 {
                format!("{h} h")
            } else {
                format!("{h} h {m} min")
            }
        }
        _ => {
            let (d, h) = (seconds / 86_400, (seconds % 86_400) / 3_600);
            if h == 0 {
                format!("{d} d")
            } else {
                format!("{d} d {h} h")
            }
        }
    }
}

/// Reduces a User-Agent to what a person recognises, for example `Firefox 131 · Linux`. The
/// function cuts short anything that it does not recognise. The result is never trusted. The
/// caller escapes it.
pub fn short_agent(agent: &str) -> String {
    let browser = [
        ("Edg/", "Edge"),
        ("OPR/", "Opera"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
        ("curl/", "curl"),
    ]
    .iter()
    .find_map(|(token, name)| {
        let rest = agent.split_once(token)?.1;
        let version: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        Some(if version.is_empty() {
            (*name).to_string()
        } else {
            format!("{name} {version}")
        })
    });
    let os = [
        ("Android", "Android"),
        ("iPhone", "iOS"),
        ("iPad", "iPadOS"),
        ("Windows", "Windows"),
        ("Mac OS X", "macOS"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(token, _)| agent.contains(token))
    .map(|(_, name)| *name);
    match (browser, os) {
        (Some(b), Some(o)) => format!("{b} \u{b7} {o}"),
        (Some(b), None) => b,
        (None, Some(o)) => o.to_string(),
        (None, None) if agent.is_empty() => "unknown".to_string(),
        (None, None) => agent.chars().filter(|c| !c.is_control()).take(40).collect(),
    }
}

/// Runs blocking work, such as a call to the database, from code that may run on an async worker.
/// The function tells the runtime to move its other tasks away from this thread while the work
/// runs. Anywhere else, for example on a plain thread or in a test, the function only runs the work.
pub fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn blocking_work_runs_on_a_plain_thread_and_on_either_kind_of_runtime() {
        assert_eq!(blocking(|| 1), 1);
        let multi = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap();
        // This call panics when the runtime is not told first. The call is the block_on of another runtime.
        let inner = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(
            multi.block_on(async { blocking(|| inner.block_on(async { 2 })) }),
            2
        );
        let single = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(
            single.block_on(async { blocking(|| 3) }),
            3,
            "no hand-off on one thread"
        );
    }

    use super::*;

    #[test]
    fn form_parsing_rejects_duplicates() {
        assert!(parse_form(b"a=1&a=2").is_err());
    }

    #[test]
    fn form_decoding() {
        assert_eq!(
            percent_decode_form("hello+world%21").unwrap(),
            "hello world!"
        );
    }

    #[test]
    fn return_uri_validation() {
        assert!(validate_return_uri("/grafana/d/abc?x=1", 4096));
        assert!(!validate_return_uri("https://evil.invalid", 4096));
        assert!(!validate_return_uri("//evil.invalid/x", 4096));
        assert!(!validate_return_uri(
            "/ok\r\nLocation:%20https://evil.invalid",
            4096
        ));
        assert!(!validate_return_uri("/%2f%2fevil.invalid", 4096));
        assert!(!validate_return_uri("/%5cevil.invalid", 4096));
        assert!(!validate_return_uri("/bad%ZZ", 4096));
        assert!(!validate_return_uri("/path#fragment", 4096));
    }

    /// A URL parser of the URL Standard removes a TAB, a CR and an LF before it reads the URL.
    /// `/<TAB>/evil.example/x` then becomes `//evil.example/x`, a link to another host.
    #[test]
    fn a_return_uri_with_an_ascii_control_byte_is_refused() {
        for uri in [
            "/\t/evil.example/x",
            "/\t\t/x",
            "/\x0b/x",
            "/\x1f/x",
            "/x\x7f",
            "/%09/x",
            "/%0b/x",
            "/%7f/x",
            "/ok?q=\t",
        ] {
            assert!(!validate_return_uri(uri, 4096), "{uri:?}");
        }
        assert!(validate_return_uri("/grafana/%20space?q=a%09b", 4096));
    }

    #[test]
    fn utc_dates_and_durations_read_naturally() {
        assert_eq!(format_utc(0), "1970-01-01 00:00");
        assert_eq!(format_utc(1_791_623_502), "2026-10-10 09:11");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00");
        assert_eq!(format_utc(-86_400), "1969-12-31 00:00");
        assert_eq!(format_utc_secs(1_791_623_502), "2026-10-10 09:11:42");
        assert_eq!(format_duration(-5), "0 s");
        assert_eq!(format_duration(45), "45 s");
        assert_eq!(format_duration(720), "12 min");
        assert_eq!(format_duration(7_500), "2 h 5 min");
        assert_eq!(format_duration(7_200), "2 h");
        assert_eq!(format_duration(100_000), "1 d 3 h");
    }

    #[test]
    fn a_typed_login_cannot_carry_control_characters_or_hidden_text() {
        assert_eq!(typed_login("  Alice "), "Alice");
        assert_eq!(
            typed_login("a\nb\r\nc"),
            "a\\u{a}b\\u{d}\\u{a}c",
            "no forged line"
        );
        assert_eq!(
            typed_login("\u{1b}[31mred"),
            "\\u{1b}[31mred",
            "no terminal escape"
        );
        assert_eq!(
            typed_login("x\0y"),
            "x\\u{0}y",
            "no NUL in the database text"
        );
        assert_eq!(
            typed_login("admin\u{202e}gnp.exe"),
            "admin\\u{202e}gnp.exe",
            "a right-to-left override is shown, not obeyed"
        );
        assert_eq!(typed_login("a\u{200b}b\u{feff}"), "a\\u{200b}b\\u{feff}");
        assert_eq!(typed_login("é-日本"), "é-日本", "ordinary text is kept");
        assert_eq!(typed_login(""), "");
        assert_eq!(typed_login(" \t "), "");
    }

    #[test]
    fn a_typed_login_is_bounded_without_cutting_an_escape_in_half() {
        let long = "x".repeat(500);
        let kept = typed_login(&long);
        assert_eq!(
            kept.chars().count(),
            TYPED_LOGIN_MAX + 1,
            "the limit plus an ellipsis"
        );
        assert!(kept.ends_with('\u{2026}'));
        let escapes = "\u{7}".repeat(100);
        let kept = typed_login(&escapes);
        assert!(kept.chars().count() <= TYPED_LOGIN_MAX + 1);
        assert!(
            kept.trim_end_matches('\u{2026}').ends_with("\\u{7}"),
            "whole escapes only"
        );
        assert_eq!(
            typed_login(&"y".repeat(TYPED_LOGIN_MAX)),
            "y".repeat(TYPED_LOGIN_MAX)
        );
    }

    #[test]
    fn sql_and_markup_in_a_typed_login_stay_inert_text() {
        let nasty = "'; DROP TABLE users; --";
        assert_eq!(
            typed_login(nasty),
            nasty,
            "kept as typed: only a bound parameter ever carries it"
        );
        assert_eq!(
            html_escape(&typed_login("<img src=x onerror=1>")),
            "&lt;img src=x onerror=1&gt;"
        );
        assert_eq!(
            json_escape(&typed_login("a\"},{\"event\":\"forged")),
            "a\\\"},{\\\"event\\\":\\\"forged"
        );
    }

    #[test]
    fn user_agents_are_reduced_to_browser_and_system() {
        assert_eq!(
            short_agent("Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0"),
            "Firefox 131 \u{b7} Linux"
        );
        assert_eq!(
            short_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/118.0.0.0 Safari/537.36 Edg/118.0.2088.46"),
            "Edge 118 \u{b7} Windows"
        );
        assert_eq!(short_agent("curl/8.5.0"), "curl 8");
        assert_eq!(short_agent(""), "unknown");
        assert_eq!(short_agent(&"x".repeat(100)).len(), 40);
    }
}
