//! Embeds the admin front (`web/`) into the binary, with a SHA-256 ETag per file,
//! collects the CSP hashes of inline scripts, and records the git revision.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

/// Bodies of `<script>` elements without a `src` attribute.
fn inline_scripts(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let Some(tag_end) = after.find('>') else { break };
        let tag = &after[..tag_end];
        let body_start = tag_end + 1;
        let Some(close) = after[body_start..].find("</script>") else { break };
        if !tag.contains("src=") {
            out.push(after[body_start..body_start + close].to_string());
        }
        rest = &after[body_start + close + "</script>".len()..];
    }
    out
}

const REQUIRED: &[&str] = &["index.html", "app.js", "style.css"];

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect(&path, root, out);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, path));
        }
    }
}

fn main() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../web")
        .canonicalize()
        .expect("web/ directory");
    println!("cargo:rerun-if-changed={}", web.display());
    println!("cargo:rerun-if-env-changed=INBOX_GIT_SHA");

    let mut files = Vec::new();
    collect(&web, &web, &mut files);
    for required in REQUIRED {
        if !files.iter().any(|(p, _)| p == required) {
            panic!("web/{required} is missing");
        }
    }
    let mut code = String::from("pub static ASSETS: &[(&str, &str, &[u8], &str)] = &[\n");
    for (rel, path) in &files {
        let bytes = std::fs::read(path).expect("read asset");
        let etag: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        writeln!(
            code,
            "    ({rel:?}, {:?}, include_bytes!({:?}), {etag:?}),",
            mime(path),
            path.display().to_string()
        )
        .unwrap();
    }
    code.push_str("];\n");
    let mut hashes = Vec::new();
    for (rel, path) in &files {
        if rel.ends_with(".html") {
            let html = std::fs::read_to_string(path).expect("read html");
            for script in inline_scripts(&html) {
                let digest = Sha256::digest(script.as_bytes());
                hashes.push(format!("'sha256-{}'", STANDARD.encode(digest)));
            }
        }
    }
    hashes.sort();
    hashes.dedup();
    writeln!(code, "pub static CSP_SCRIPT_HASHES: &str = {:?};", hashes.join(" ")).unwrap();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("assets.rs");
    std::fs::write(out, code).expect("write assets.rs");

    let sha = std::env::var("INBOX_GIT_SHA").ok().filter(|s| !s.is_empty()).or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    println!("cargo:rustc-env=INBOX_GIT_SHA={}", sha.unwrap_or_else(|| "unknown".into()));
}
