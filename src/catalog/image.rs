//! The own icon of an external service: an image that the portal stores and serves itself.
//!
//! The image comes from the file (a data URI, or the bytes of an image file) or from the form of the Settings page.
//! The portal accepts PNG, JPEG, WebP, GIF and SVG, up to [`MAX_BYTES`] bytes after decoding. The portal finds the
//! type from the bytes (the magic numbers). A declared type must match the bytes, and it never replaces them.
//!
//! An SVG is only ever served as an image. The route sends a policy that allows no script and no source, and the
//! page shows it in an `img` element. The check here is a second line: it refuses an SVG that holds a script, an
//! event attribute, a `foreignObject` or an external reference.

use base64ct::{Base64, Encoding};
use sha2::{Digest, Sha256};

/// The largest image, in bytes after decoding.
pub const MAX_BYTES: usize = 32 * 1024;

/// The most characters of a data URI of an image: the prefix of the longest type and the Base64 text of the largest image.
pub const MAX_DATA_URI_CHARS: usize =
    "data:image/svg+xml;base64,".len() + MAX_BYTES.div_ceil(3) * 4;

/// The types that the portal accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Webp,
    Gif,
    Svg,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Png, Kind::Jpeg, Kind::Webp, Kind::Gif, Kind::Svg];

    /// The media type. The database keeps it, and the route sends it.
    pub fn mime(self) -> &'static str {
        match self {
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
            Kind::Webp => "image/webp",
            Kind::Gif => "image/gif",
            Kind::Svg => "image/svg+xml",
        }
    }

    pub fn from_mime(mime: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.mime() == mime)
    }
}

/// A checked image, with the validator that the route sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    kind: Kind,
    bytes: Vec<u8>,
    etag: String,
}

impl Image {
    /// Checks the bytes. `declared` is the type that the source states, if it states one: it must match the bytes.
    pub fn from_bytes(bytes: Vec<u8>, declared: Option<Kind>) -> Result<Image, String> {
        if bytes.is_empty() {
            return Err("the image is empty".into());
        }
        if bytes.len() > MAX_BYTES {
            return Err(format!(
                "the image has {} bytes, the largest size is {MAX_BYTES} bytes",
                bytes.len()
            ));
        }
        let Some(kind) = sniff(&bytes) else {
            return Err(
                "the bytes are not a PNG, a JPEG, a WebP, a GIF or an SVG image (the portal reads the type from the bytes)"
                    .into(),
            );
        };
        if let Some(declared) = declared {
            if declared != kind {
                return Err(format!(
                    "the declared type is {}, and the bytes are {}",
                    declared.mime(),
                    kind.mime()
                ));
            }
        }
        if kind == Kind::Svg {
            check_svg(&bytes)?;
        }
        let digest = Sha256::digest(&bytes);
        let etag = format!(
            "\"{}\"",
            digest[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        Ok(Image { kind, bytes, etag })
    }

    /// Checks the type that a database row keeps, and its bytes.
    pub fn from_stored(mime: &str, bytes: Vec<u8>) -> Result<Image, String> {
        let kind = Kind::from_mime(mime)
            .ok_or_else(|| format!("the stored image type {mime:?} is not known"))?;
        Image::from_bytes(bytes, Some(kind))
    }

    /// Reads a data URI: `data:<type>;base64,<payload>`. The type is one of the five types, in lower case. The
    /// payload is standard Base64 with its padding, and has no space.
    pub fn from_data_uri(text: &str) -> Result<Image, String> {
        let rest = text
            .strip_prefix("data:")
            .ok_or("the value is not a data URI: it starts with data:")?;
        let (head, payload) = rest
            .split_once(',')
            .ok_or("the data URI has no comma before its data")?;
        let mime = head
            .strip_suffix(";base64")
            .ok_or("the data URI is not in the form data:<type>;base64,<data>")?;
        let declared = Kind::from_mime(mime).ok_or_else(|| {
            format!("the declared type {mime:?} is not image/png, image/jpeg, image/webp, image/gif or image/svg+xml")
        })?;
        // The longest payload of the largest image: no decoding of a larger text.
        if payload.len() > MAX_BYTES.div_ceil(3) * 4 {
            return Err(format!(
                "the data has {} characters, more than the image limit of {MAX_BYTES} bytes allows",
                payload.len()
            ));
        }
        let bytes = Base64::decode_vec(payload)
            .map_err(|_| "the data is not standard Base64 with its padding".to_string())?;
        Image::from_bytes(bytes, Some(declared))
    }

    #[cfg(test)]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn mime(&self) -> &'static str {
        self.kind.mime()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The strong validator, with its quotes: the first 16 bytes of the SHA-256 digest of the image, in hexadecimal.
    pub fn etag(&self) -> &str {
        &self.etag
    }
}

/// The type of the bytes, from the magic numbers. An SVG has no magic number: it is a text whose root element is
/// `svg`.
pub fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.len() >= 16 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR" {
        return Some(Kind::Png);
    }
    if bytes.len() >= 4 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Kind::Jpeg);
    }
    if bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some(Kind::Gif);
    }
    if bytes.len() >= 16 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(Kind::Webp);
    }
    svg_root(bytes).then_some(Kind::Svg)
}

/// Whether the bytes are a text whose first element, after the prolog and the comments, is `svg`.
fn svg_root(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("<!--") {
            match after.split_once("-->") {
                Some((_, tail)) => rest = tail,
                None => return false,
            }
        } else if rest.starts_with("<?xml ") || rest.starts_with("<?xml?") {
            match rest.split_once("?>") {
                Some((_, tail)) => rest = tail,
                None => return false,
            }
        } else if rest
            .get(..9)
            .is_some_and(|head| head.eq_ignore_ascii_case("<!doctype"))
        {
            // A declaration of the type of the document, with no internal subset (the check of the SVG refuses a subset).
            match rest.split_once('>') {
                Some((_, tail)) => rest = tail,
                None => return false,
            }
        } else {
            break;
        }
    }
    let lower = rest.get(..5).map(str::to_ascii_lowercase);
    lower
        .as_deref()
        .is_some_and(|head| head.starts_with("<svg"))
        && rest[4..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_whitespace() || c == '>' || c == '/')
}

/// Refuses an SVG that can run code or load another resource. The check reads the text in lower case and fails
/// closed: a form that it cannot read as an attribute is a refusal.
pub fn check_svg(bytes: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the SVG is not valid UTF-8".to_string())?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err("the SVG has a control character".into());
    }
    let lower = text.to_ascii_lowercase();
    for needle in [
        "<script",
        "<foreignobject",
        "<!entity",
        "javascript:",
        "vbscript:",
        "@import",
        "<?xml-stylesheet",
    ] {
        if lower.contains(needle) {
            return Err(format!("the SVG has {needle:?}"));
        }
    }
    for tag in ["iframe", "embed", "object", "link", "meta"] {
        if has_tag(&lower, tag) {
            return Err(format!("the SVG has the element <{tag}>"));
        }
    }
    if let Some(at) = lower.find("<!doctype") {
        let declaration = &lower[at..];
        let end = declaration.find('>').unwrap_or(declaration.len());
        if declaration[..end].contains('[') {
            return Err("the SVG has a DOCTYPE with an internal subset".into());
        }
    }
    if has_event_attribute(&lower) {
        return Err("the SVG has an event attribute (on...=)".into());
    }
    for value in attribute_values(&lower, "href") {
        if !value.starts_with('#') {
            return Err(format!(
                "the SVG has a link that is not a fragment (href={value:?}): only #name is allowed"
            ));
        }
    }
    for value in attribute_values(&lower, "attributename") {
        if value.contains("href") {
            return Err("the SVG changes a link with an animation".into());
        }
    }
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("url(") {
        let tail = rest[at + 4..].trim_start();
        let tail = tail
            .strip_prefix('"')
            .or_else(|| tail.strip_prefix('\''))
            .or_else(|| tail.strip_prefix("&quot;"))
            .unwrap_or(tail);
        if !tail.starts_with('#') {
            return Err("the SVG has url() that is not a fragment (url(#name))".into());
        }
        rest = &rest[at + 4..];
    }
    Ok(())
}

/// Whether the text holds the start tag `<name` followed by a space, `>` or `/`.
fn has_tag(lower: &str, name: &str) -> bool {
    let needle = format!("<{name}");
    let mut rest = lower;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at + needle.len()..];
        if after
            .chars()
            .next()
            .is_none_or(|c| c.is_ascii_whitespace() || c == '>' || c == '/')
        {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether an attribute name that starts with `on` and has letters only is written: `onload=`, ` onclick = `.
fn has_event_attribute(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(found) = lower[from..].find("on") {
        let at = from + found;
        from = at + 2;
        let before_is_boundary = at > 0
            && (bytes[at - 1].is_ascii_whitespace()
                || matches!(bytes[at - 1], b'"' | b'\'' | b'/'));
        if !before_is_boundary {
            continue;
        }
        let mut end = at + 2;
        while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
            end += 1;
        }
        if end == at + 2 {
            continue;
        }
        while end < bytes.len() && bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'=' {
            return true;
        }
    }
    false
}

/// The value of each attribute whose name is `name` or ends with `:name`, for example `href` and `xlink:href`.
/// A value has quotes, or it ends at a space or at `>`. The text is in lower case.
fn attribute_values(lower: &str, name: &str) -> Vec<String> {
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let boundary = at > 0
            && (bytes[at - 1].is_ascii_whitespace()
                || matches!(bytes[at - 1], b':' | b'"' | b'\''));
        if !boundary {
            continue;
        }
        let rest = lower[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or("").to_string(),
            _ => rest
                .split(|c: char| c.is_ascii_whitespace() || c == '>')
                .next()
                .unwrap_or("")
                .to_string(),
        };
        out.push(value.trim_start().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] =
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";
    const JPEG: &[u8] = b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0\xff\xd9";
    const GIF: &[u8] = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff;";
    const WEBP: &[u8] = b"RIFF\x1a\0\0\0WEBPVP8L\x0d\0\0\0/\0\0\0\x10";
    const SVG: &[u8] =
        br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><defs><linearGradient id="g"/></defs><rect width="8" height="8" fill="url(#g)"/><use href="#g"/></svg>"##;

    fn uri(mime: &str, bytes: &[u8]) -> String {
        format!("data:{mime};base64,{}", Base64::encode_string(bytes))
    }

    #[test]
    fn each_type_is_found_by_its_bytes() {
        for (bytes, kind) in [
            (PNG, Kind::Png),
            (JPEG, Kind::Jpeg),
            (GIF, Kind::Gif),
            (WEBP, Kind::Webp),
            (SVG, Kind::Svg),
        ] {
            assert_eq!(sniff(bytes), Some(kind), "{kind:?}");
            let image = Image::from_bytes(bytes.to_vec(), Some(kind)).unwrap();
            assert_eq!(image.kind(), kind);
            assert_eq!(image.bytes(), bytes);
            assert_eq!(Image::from_bytes(bytes.to_vec(), None).unwrap(), image);
            let from_uri = Image::from_data_uri(&uri(kind.mime(), bytes)).unwrap();
            assert_eq!(from_uri, image);
            assert_eq!(
                Image::from_stored(kind.mime(), bytes.to_vec()).unwrap(),
                image
            );
        }
    }

    #[test]
    fn the_validator_is_strong_and_follows_the_bytes() {
        let a = Image::from_bytes(PNG.to_vec(), None).unwrap();
        let b = Image::from_bytes(GIF.to_vec(), None).unwrap();
        assert!(
            a.etag().starts_with('"') && a.etag().ends_with('"') && !a.etag().starts_with("W/")
        );
        assert_eq!(a.etag().len(), 34);
        assert_ne!(a.etag(), b.etag());
        assert_eq!(
            a.etag(),
            Image::from_bytes(PNG.to_vec(), Some(Kind::Png))
                .unwrap()
                .etag()
        );
    }

    #[test]
    fn a_declared_type_that_does_not_match_the_bytes_is_refused() {
        for (bytes, declared) in [
            (PNG, Kind::Jpeg),
            (JPEG, Kind::Png),
            (SVG, Kind::Png),
            (PNG, Kind::Svg),
            (GIF, Kind::Webp),
        ] {
            let error = Image::from_data_uri(&uri(declared.mime(), bytes)).unwrap_err();
            assert!(error.contains("declared type"), "{error}");
            assert!(Image::from_bytes(bytes.to_vec(), Some(declared)).is_err());
        }
    }

    #[test]
    fn bytes_that_are_no_image_are_refused() {
        for bytes in [
            &b""[..],
            b"hello",
            b"<html><svg></svg></html>",
            b"\x89PNG\r\n\x1a\n",
            b"MZ\x90\0",
            b"<svg",
            b"<svgx/>",
            b"%PDF-1.4",
            b"\xff\xfe<\0s\0v\0g\0>\0",
        ] {
            assert!(
                Image::from_bytes(bytes.to_vec(), None).is_err(),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn the_size_limit_is_32_kib_after_decoding() {
        let mut png = PNG.to_vec();
        png.resize(MAX_BYTES, 0);
        assert!(Image::from_data_uri(&uri("image/png", &png)).is_ok());
        png.push(0);
        let error = Image::from_data_uri(&uri("image/png", &png)).unwrap_err();
        assert!(
            error.contains("32768") || error.contains("more than"),
            "{error}"
        );
        assert!(Image::from_bytes(png, None).is_err());
        // A payload much larger than the limit is refused before it is decoded.
        let huge = format!("data:image/png;base64,{}", "A".repeat(MAX_BYTES * 2));
        assert!(Image::from_data_uri(&huge)
            .unwrap_err()
            .contains("characters"));
    }

    #[test]
    fn the_data_uri_has_one_strict_form() {
        let good = uri("image/png", PNG);
        assert!(Image::from_data_uri(&good).is_ok());
        // A payload with its padding is good, and the same payload without it is not.
        let padded = Base64::encode_string(&PNG[..28]);
        assert!(padded.ends_with("=="));
        assert!(Image::from_data_uri(&format!("data:image/png;base64,{padded}")).is_ok());
        let payload = Base64::encode_string(PNG);
        for bad in [
            String::new(),
            "image/png;base64,AAAA".to_string(),
            format!("data:image/png,{payload}"),
            format!("data:image/png;charset=utf-8;base64,{payload}"),
            format!("data:IMAGE/PNG;base64,{payload}"),
            format!("data:image/x-icon;base64,{payload}"),
            format!("data:text/html;base64,{payload}"),
            format!("data:image/png;base64,{payload}\n"),
            format!("data:image/png;base64, {payload}"),
            format!(
                "data:image/png;base64,{}",
                Base64::encode_string(&PNG[..28]).trim_end_matches('=')
            ),
            format!("data:image/png;base64,{}", payload.replace('A', "-")),
            "data:image/png;base64".to_string(),
            "data:image/png;base64,".to_string(),
        ] {
            assert!(Image::from_data_uri(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_svg_attack_is_refused() {
        let wrap =
            |inner: &str| format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{inner}</svg>"#);
        let attacks = [
            wrap("<script>alert(1)</script>"),
            wrap("<SCRIPT>alert(1)</SCRIPT>"),
            wrap(r#"<rect onload="alert(1)"/>"#),
            wrap(r#"<rect ONCLICK = "alert(1)"/>"#),
            wrap("<rect onmouseover='x'/>"),
            wrap("<foreignObject><div>hi</div></foreignObject>"),
            wrap(r#"<a href="javascript:alert(1)"><rect/></a>"#),
            wrap(r#"<a xlink:href="JaVaScRiPt:alert(1)"><rect/></a>"#),
            wrap(r#"<image href="https://evil.example/x.png"/>"#),
            wrap(r#"<image xlink:href="//evil.example/x.png"/>"#),
            wrap(r#"<image href="data:image/png;base64,AAAA"/>"#),
            wrap(r#"<use href = "other.svg#a"/>"#),
            wrap(r#"<use href=http://evil.example/a.svg#a />"#),
            wrap(r#"<rect style="fill:url(https://evil.example/a)"/>"#),
            wrap(r#"<rect fill="url( 'http://evil.example/a' )"/>"#),
            wrap(r#"<rect style="background:url(data:image/png;base64,AAAA)"/>"#),
            wrap("<style>@import url(//evil.example/a.css);</style>"),
            wrap(r##"<set attributeName="href" to="#a"/>"##),
            wrap("<iframe src='x'/>"),
            wrap("<embed src='x'/>"),
            wrap("<object data='x'/>"),
            wrap("<link rel='stylesheet' href='#a'/>"),
            wrap("<meta http-equiv='refresh'/>"),
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/passwd">]><svg xmlns="http://www.w3.org/2000/svg">&x;</svg>"#.to_string(),
            r#"<!DOCTYPE svg [ <!ELEMENT svg ANY> ]><svg xmlns="http://www.w3.org/2000/svg"/>"#.to_string(),
            r#"<?xml-stylesheet href="x.css"?><svg xmlns="http://www.w3.org/2000/svg"/>"#.to_string(),
            "<svg xmlns=\"http://www.w3.org/2000/svg\">\u{7}</svg>".to_string(),
        ];
        for svg in attacks {
            assert!(
                Image::from_data_uri(&uri("image/svg+xml", svg.as_bytes())).is_err(),
                "{svg}"
            );
        }
    }

    #[test]
    fn a_clean_svg_with_fragments_comments_and_a_plain_doctype_is_accepted() {
        let svg = br##"<?xml version="1.0" encoding="UTF-8"?>
<!-- an icon -->
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="24" height="24">
  <metadata>made by an editor</metadata>
  <defs><linearGradient id="g"><stop offset="0" stop-color="#fff"/></linearGradient><path id="p" d="M0 0h8v8z"/></defs>
  <use xlink:href="#p" fill="url(#g)"/>
  <rect style="fill:url('#g');stroke:url(&quot;#g&quot;)" width="3" height="3"/>
  <text>version on</text>
</svg>"##;
        let image = Image::from_bytes(svg.to_vec(), Some(Kind::Svg)).unwrap();
        assert_eq!(image.kind(), Kind::Svg);
        let with_bom = [&b"\xef\xbb\xbf"[..], svg].concat();
        assert!(Image::from_bytes(with_bom, Some(Kind::Svg)).is_ok());
    }
}
