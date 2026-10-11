//! The offline icon library: three SVG sets shipped in the image under `/icons`.
//!
//! | set | origin | look |
//! |---|---|---|
//! | `di` | dashboard-icons | brand and service logos, in colour |
//! | `si` | simple-icons | brand glyphs, monochrome |
//! | `tb` | tabler icons (outline) | generic interface icons, monochrome |
//!
//! Nothing is fetched at runtime. A service names its icon in any of the forms people already
//! use in Homepage labels (`windmill`, `docker.png`, `si-grafana`) or with an explicit set
//! (`di:windmill`, `tb:server`); a bare name is looked up in `di`, then `si`, then `tb`.
//!
//! Interface: [`Icons::load`], [`Icons::resolve`] (spec to [`Icon`]) and [`Icons::svg`] (bytes for
//! the route). The directory is scanned once; the files are read on demand.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// An icon larger than this is not offered. A service icon is drawn at 24 pixels, and the browser
/// parses and rasterises the whole document to do it: the library has logos of 250 KiB (embedded
/// bitmaps) that cost a launcher more than the rest of the page. The ones a lab uses weigh 1 to 5 KiB.
const MAX_SVG_BYTES: u64 = 32 * 1024;
const EXTENSIONS: [&str; 6] = [".png", ".svg", ".webp", ".jpg", ".jpeg", ".ico"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    Dashboard,
    Simple,
    Tabler,
}

impl Set {
    const ALL: [Set; 3] = [Set::Dashboard, Set::Simple, Set::Tabler];

    pub fn dir(self) -> &'static str {
        match self {
            Set::Dashboard => "di",
            Set::Simple => "si",
            Set::Tabler => "tb",
        }
    }

    pub fn from_dir(dir: &str) -> Option<Set> {
        Set::ALL.into_iter().find(|set| set.dir() == dir)
    }

    /// Monochrome sets draw with `currentColor`/black: the page recolours them for the theme.
    pub fn is_monochrome(self) -> bool {
        self != Set::Dashboard
    }

    fn index(self) -> usize {
        match self {
            Set::Dashboard => 0,
            Set::Simple => 1,
            Set::Tabler => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    pub set: Set,
    pub name: String,
}

impl Icon {
    pub fn url(&self) -> String {
        format!("/auth/icons/{}/{}.svg", self.set.dir(), self.name)
    }
}

pub struct Icons {
    root: PathBuf,
    names: [HashSet<String>; 3],
}

impl Icons {
    /// An empty library: every icon falls back to the service's initial.
    pub fn empty() -> Self {
        Self {
            root: PathBuf::new(),
            names: Default::default(),
        }
    }

    /// Indexes `root/{di,si,tb}/*.svg` within the size budget. A missing directory gives an empty
    /// library.
    pub fn load(root: &Path) -> Self {
        let mut icons = Self {
            root: root.to_path_buf(),
            names: Default::default(),
        };
        for set in Set::ALL {
            let Ok(entries) = fs::read_dir(root.join(set.dir())) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.file_name();
                if let Some(name) = file.to_str().and_then(|f| f.strip_suffix(".svg")) {
                    let light = entry
                        .metadata()
                        .is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_SVG_BYTES);
                    if light && is_safe_name(name) {
                        icons.names[set.index()].insert(name.to_string());
                    }
                }
            }
        }
        icons
    }

    pub fn len(&self) -> usize {
        self.names.iter().map(HashSet::len).sum()
    }

    /// Maps what a label says (`docker.png`, `si-grafana`, `tb:server`) to an icon of the library.
    pub fn resolve(&self, spec: &str) -> Option<Icon> {
        let spec = spec.trim().to_ascii_lowercase();
        let (only, rest) = split_set(&spec);
        let mut name = rest;
        for extension in EXTENSIONS {
            if let Some(stripped) = name.strip_suffix(extension) {
                name = stripped;
                break;
            }
        }
        let name = name.replace([' ', '_'], "-");
        if !is_safe_name(&name) {
            return None;
        }
        let order: &[Set] = match &only {
            Some(set) => std::slice::from_ref(set),
            None => &Set::ALL,
        };
        order
            .iter()
            .find(|set| self.names[set.index()].contains(&name))
            .map(|set| Icon { set: *set, name })
    }

    /// The SVG of a known icon. Only names found by the scan can be read: no path is built from
    /// request input.
    pub fn svg(&self, set: Set, name: &str) -> Option<Vec<u8>> {
        if !self.names[set.index()].contains(name) {
            return None;
        }
        let path = self.root.join(set.dir()).join(format!("{name}.svg"));
        let metadata = fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_SVG_BYTES {
            return None;
        }
        fs::read(path).ok()
    }
}

/// Splits an explicit set prefix: `di:`, `si:`, `tb:`, `tabler:`, `simple:`, and Homepage's `si-`.
fn split_set(spec: &str) -> (Option<Set>, &str) {
    for (prefix, set) in [
        ("di:", Set::Dashboard),
        ("dashboard:", Set::Dashboard),
        ("si:", Set::Simple),
        ("simple:", Set::Simple),
        ("si-", Set::Simple),
        ("tb:", Set::Tabler),
        ("tabler:", Set::Tabler),
    ] {
        if let Some(rest) = spec.strip_prefix(prefix) {
            return (Some(set), rest);
        }
    }
    (None, spec)
}

fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn library(files: &[(&str, &str)]) -> (Icons, PathBuf) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "portal-icons-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for (path, content) in files {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, content).unwrap();
        }
        (Icons::load(&root), root)
    }

    fn sample() -> (Icons, PathBuf) {
        library(&[
            ("di/docker.svg", "<svg id=\"d\"/>"),
            ("di/windmill.svg", "<svg/>"),
            ("si/grafana.svg", "<svg id=\"s\"/>"),
            ("si/docker.svg", "<svg/>"),
            ("tb/server.svg", "<svg id=\"t\"/>"),
            ("tb/docker.svg", "<svg/>"),
            ("di/Bad Name.svg", "<svg/>"),
            ("di/notes.txt", "x"),
        ])
    }

    #[test]
    fn bare_names_search_di_then_si_then_tb() {
        let (icons, root) = sample();
        assert_eq!(icons.resolve("docker").unwrap().set, Set::Dashboard);
        assert_eq!(icons.resolve("grafana").unwrap().set, Set::Simple);
        assert_eq!(icons.resolve("server").unwrap().set, Set::Tabler);
        assert_eq!(
            icons.len(),
            7 - 1,
            "unsafe names and non-svg files are not indexed"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn homepage_style_names_resolve() {
        let (icons, root) = sample();
        assert_eq!(
            icons.resolve("docker.png").unwrap().url(),
            "/auth/icons/di/docker.svg"
        );
        assert_eq!(icons.resolve("Windmill.SVG").unwrap().name, "windmill");
        assert_eq!(icons.resolve("si-grafana").unwrap().set, Set::Simple);
        assert_eq!(
            icons.resolve("si-docker").unwrap().set,
            Set::Simple,
            "si- forces the simple-icons set"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_explicit_set_is_respected_and_never_falls_back() {
        let (icons, root) = sample();
        assert_eq!(icons.resolve("tb:docker").unwrap().set, Set::Tabler);
        assert_eq!(icons.resolve("tabler:server").unwrap().set, Set::Tabler);
        assert_eq!(icons.resolve("di:grafana"), None);
        assert_eq!(icons.resolve("tb:windmill"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_or_hostile_specs_resolve_to_nothing() {
        let (icons, root) = sample();
        for spec in [
            "",
            "nope",
            "../etc/passwd",
            "di:../x",
            "a/b",
            ".hidden",
            "docker\0",
        ] {
            assert_eq!(icons.resolve(spec), None, "{spec:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_indexed_icons_can_be_read() {
        let (icons, root) = sample();
        assert_eq!(
            icons.svg(Set::Dashboard, "docker").unwrap(),
            b"<svg id=\"d\"/>"
        );
        assert_eq!(icons.svg(Set::Dashboard, "grafana"), None);
        assert_eq!(icons.svg(Set::Dashboard, "../si/grafana"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_heavy_icon_is_not_offered_and_a_bare_name_falls_through_to_a_lighter_set() {
        let heavy = format!("<svg>{}</svg>", "x".repeat(MAX_SVG_BYTES as usize));
        let (icons, root) = library(&[
            ("di/memos.svg", heavy.as_str()),
            ("si/memos.svg", "<svg id=\"s\"/>"),
            ("di/bulky.svg", heavy.as_str()),
        ]);
        let memos = icons.resolve("memos").unwrap();
        assert_eq!(memos.set, Set::Simple, "the heavy colour logo is skipped");
        assert_eq!(icons.resolve("di:memos"), None, "an explicit set does not fall back");
        assert_eq!(icons.resolve("bulky"), None, "no lighter one: the tile shows the initial");
        assert!(icons.svg(Set::Dashboard, "memos").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_directory_is_an_empty_library() {
        let icons = Icons::load(Path::new("/nonexistent/portal-icons"));
        assert_eq!(icons.len(), 0);
        assert_eq!(icons.resolve("docker"), None);
        assert!(Icons::empty().svg(Set::Tabler, "server").is_none());
    }

    #[test]
    fn monochrome_sets_are_flagged_for_recolouring() {
        assert!(!Set::Dashboard.is_monochrome());
        assert!(Set::Simple.is_monochrome());
        assert!(Set::Tabler.is_monochrome());
        assert_eq!(Set::from_dir("tb"), Some(Set::Tabler));
        assert_eq!(Set::from_dir("x"), None);
    }
}
