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
                let hi = from_hex(bytes[i + 1]).ok_or_else(|| "invalid percent-encoding".to_string())?;
                let lo = from_hex(bytes[i + 2]).ok_or_else(|| "invalid percent-encoding".to_string())?;
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

pub fn validate_return_uri(uri: &str, max_len: usize) -> bool {
    if uri.is_empty() || uri.len() > max_len || !uri.starts_with('/') || uri.starts_with("//") {
        return false;
    }
    if uri.contains('#') || uri.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n') {
        return false;
    }
    if uri.contains('\\') {
        return false;
    }

    // Validate the path after one percent-decoding pass as well. This prevents
    // redirect targets such as /%2f%2fevil.example or /%5cevil.example from
    // becoming network-path/backslash URLs after user-agent normalization.
    let path = path_only(uri);
    let decoded_path = match percent_decode_uri_component(path) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if !decoded_path.starts_with('/')
        || decoded_path.starts_with("//")
        || decoded_path.contains('\\')
        || decoded_path.contains('#')
        || decoded_path
            .bytes()
            .any(|b| b == 0 || b == b'\r' || b == b'\n')
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
            let hi = from_hex(bytes[i + 1]).ok_or_else(|| "invalid percent-encoding".to_string())?;
            let lo = from_hex(bytes[i + 2]).ok_or_else(|| "invalid percent-encoding".to_string())?;
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

/// `2026-10-10 14:03` (UTC). The image has no time zone database and the pages have no script,
/// so every time shown is UTC.
pub fn format_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Civil date from a day count (Howard Hinnant's algorithm), proleptic Gregorian.
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

/// `45 s`, `12 min`, `2 h 5 min`, `3 d 4 h`.
pub fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..=59 => format!("{seconds} s"),
        60..=3_599 => format!("{} min", seconds / 60),
        3_600..=86_399 => {
            let (h, m) = (seconds / 3_600, (seconds % 3_600) / 60);
            if m == 0 { format!("{h} h") } else { format!("{h} h {m} min") }
        }
        _ => {
            let (d, h) = (seconds / 86_400, (seconds % 86_400) / 3_600);
            if h == 0 { format!("{d} d") } else { format!("{d} d {h} h") }
        }
    }
}

/// A User-Agent reduced to what a person recognises: `Firefox 131 · Linux`. Anything not
/// recognised is cut short. Never trusted: the caller escapes it.
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
        Some(if version.is_empty() { (*name).to_string() } else { format!("{name} {version}") })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_parsing_rejects_duplicates() {
        assert!(parse_form(b"a=1&a=2").is_err());
    }

    #[test]
    fn form_decoding() {
        assert_eq!(percent_decode_form("hello+world%21").unwrap(), "hello world!");
    }

    #[test]
    fn return_uri_validation() {
        assert!(validate_return_uri("/grafana/d/abc?x=1", 4096));
        assert!(!validate_return_uri("https://evil.invalid", 4096));
        assert!(!validate_return_uri("//evil.invalid/x", 4096));
        assert!(!validate_return_uri("/ok\r\nLocation:%20https://evil.invalid", 4096));
        assert!(!validate_return_uri("/%2f%2fevil.invalid", 4096));
        assert!(!validate_return_uri("/%5cevil.invalid", 4096));
        assert!(!validate_return_uri("/bad%ZZ", 4096));
        assert!(!validate_return_uri("/path#fragment", 4096));
    }

    #[test]
    fn utc_dates_and_durations_read_naturally() {
        assert_eq!(format_utc(0), "1970-01-01 00:00");
        assert_eq!(format_utc(1_791_623_502), "2026-10-10 09:11");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00");
        assert_eq!(format_utc(-86_400), "1969-12-31 00:00");
        assert_eq!(format_duration(-5), "0 s");
        assert_eq!(format_duration(45), "45 s");
        assert_eq!(format_duration(720), "12 min");
        assert_eq!(format_duration(7_500), "2 h 5 min");
        assert_eq!(format_duration(7_200), "2 h");
        assert_eq!(format_duration(100_000), "1 d 3 h");
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
