//! The external services of the file: the blocks `external_service`.
//!
//! Each key of a block is checked once, when the table of keys is resolved: the link, the description and the data URI
//! of the icon. This module adds what one key cannot check: the label, the file of the icon, and the ids of the blocks
//! together. An error names the block, the key and the value. The module never changes a value: it removes no space
//! and clamps nothing.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use super::resolve::Resolved;
use crate::catalog::external::{self, ExternalEntry};
use crate::catalog::image::{Image, MAX_BYTES};

/// The most blocks that the file can have: the number that the database holds.
pub const MAX_BLOCKS: usize = external::MAX_SERVICES;

/// The external services of the file, in the order of the file.
pub fn build(r: &Resolved) -> Result<Vec<ExternalEntry>, String> {
    let mut entries: Vec<ExternalEntry> = Vec::new();
    for block in r.labeled.iter().filter(|b| b.kind == "external_service") {
        let label = &block.label;
        let path = format!("external_service.{label}");
        let (name, id) = match external::check_name(label) {
            Ok((name, id)) if name == *label => (name, id),
            Ok(_) => {
                return Err(format!(
                    "external_service {label:?}: the label has a space at its start or at its end: write the name without them, because the label is the display name"
                ))
            }
            Err(_) => {
                return Err(format!(
                    "external_service {label:?}: expected a name of 1 to {} characters, with no control character and at least one letter or digit, got {label:?}",
                    external::MAX_NAME_CHARS
                ))
            }
        };
        if let Some(other) = entries.iter().find(|e| e.id == id) {
            return Err(format!(
                "external_service {label:?} and external_service {:?} have the same id {id}: give each service a name with its own slug",
                other.name
            ));
        }
        let icon = match (
            block.optional_text("icon"),
            block.optional_text("icon_path"),
        ) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "{path}: give at most one of icon and icon_path, not both"
                ))
            }
            (Some(uri), None) => Some(Image::from_data_uri(&uri).map_err(|reason| {
                format!("invalid {path}.icon: expected a data URI of an image ({reason})")
            })?),
            (None, Some(file)) => Some(read_icon(&path, &file)?),
            (None, None) => None,
        };
        entries.push(ExternalEntry {
            id,
            name,
            href: block.text("href")?,
            description: block.text("description")?,
            icon: icon.map(Arc::new),
            created_at: 0,
            from_file: true,
        });
        if entries.len() > MAX_BLOCKS {
            return Err(format!(
                "external_service {label:?}: the file has more than {MAX_BLOCKS} blocks external_service"
            ));
        }
    }
    Ok(entries)
}

/// The bytes of an image file, exactly: the function removes no line end. The file is at most [`MAX_BYTES`] bytes.
fn read_icon(path: &str, file: &str) -> Result<Image, String> {
    let attribute = format!("{path}.icon_path");
    let mut bytes = Vec::new();
    File::open(Path::new(file))
        .map_err(|e| format!("{attribute}: cannot open {file}: {e}"))?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{attribute}: cannot read {file}: {e}"))?;
    if bytes.is_empty() {
        return Err(format!("{attribute}: {file} is empty"));
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!(
            "{attribute}: {file} is larger than {MAX_BYTES} bytes"
        ));
    }
    Image::from_bytes(bytes, None).map_err(|reason| format!("{attribute}: {file}: {reason}"))
}
