//! The command `portal inbox rekey`: seals again the contents of an old key with the active key.
//!
//! A rotation adds a key and makes it active (step 1). The new contents get the new key. The old contents keep the
//! old key until this command seals them again (step 2). The operator removes the old key from the file when the
//! command says that no row uses it (step 3).
//!
//! The command visits every table that holds a sealed value: the bodies of the items, the turns, the intents of the
//! agents and the hints of the agents. It reads a page of rows, opens each value with the key that its header
//! names, seals it again with the active key and writes it back with a compare-and-set on the old sealed value: a
//! writer that changed the row first keeps its value. The associated data does not change (see
//! [`content_box::aad`]), so each contract that binds a sealed value to its row, its field and its effect keeps
//! working. A page is one batch. The command writes each batch in one transaction and holds no lock between two
//! batches, so it can run again after a stop, and it can run while the portal serves (PostgreSQL).
//!
//! The command never writes a content in a log, in a message or in its report: the report has counts and the ids of
//! the rows that do not open.

use std::path::Path;

use super::content_box::{self, ContentBox, FIELD_BODY, FIELD_HINTS, FIELD_INTENT, FIELD_TURN};
use super::items_ops::{ItemTx, RekeyCursor, RekeyRow, RekeyTable, Sealed};
use super::items_store::in_tx;
use super::store::Store;

/// The number of rows in one batch.
pub const DEFAULT_BATCH: u32 = 200;
/// The number of rounds after which the command stops when other writers keep changing the same rows.
const MAX_ROUNDS: u32 = 5;
/// The number of ids of unreadable rows that the report names.
const NAMED_UNREADABLE: usize = 5;

/// What the command works on.
pub enum Target<'a> {
    /// The store of a running portal, or the PostgreSQL database.
    Store(&'a Store),
    /// A SQLite file, opened as a plain connection (no migration, no session row).
    Sqlite(std::cell::RefCell<rusqlite::Connection>),
}

/// The options of the command.
#[derive(Clone, Debug)]
pub struct Options {
    /// Seal again only the rows of this key id. `None`: the rows of every key that is not the active key.
    pub from: Option<String>,
    /// The rows in one batch.
    pub batch: u32,
    /// A test stops the command after this number of batches, as a kill does. `None`: the command runs to its end.
    pub stop_after_batches: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            from: None,
            batch: DEFAULT_BATCH,
            stop_after_batches: None,
        }
    }
}

/// The result of the command.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The rows that now use the active key, for each table, in the order of [`RekeyTable::ALL`].
    pub moved: [u64; 4],
    /// The rows that another writer changed while the command ran, and that the command did not overwrite. The next
    /// round reads them again: this number counts those that stayed changed after the last round.
    pub changed: u64,
    /// The rows that do not open: a key that the file lost, or a value that changed. The command leaves them.
    pub unreadable: u64,
    /// The ids of the first unreadable rows (an id is not a content).
    pub unreadable_ids: Vec<String>,
    /// Each key id that still seals rows, with the number of rows, after the command.
    pub left: Vec<(String, u64)>,
    /// The key ids whose check value the command dropped, because no row uses them any more.
    pub check_dropped: Vec<String>,
    /// The command stopped before its end (a test, or a kill).
    pub stopped: bool,
}

impl Report {
    /// The rows that the command moved, in all the tables.
    #[cfg(test)]
    pub fn total(&self) -> u64 {
        self.moved.iter().sum()
    }

    /// The number of rows that still use the key `kid`.
    #[cfg(test)]
    pub fn left_of(&self, kid: &str) -> u64 {
        self.left
            .iter()
            .filter(|(k, _)| k == kid)
            .map(|(_, n)| *n)
            .sum()
    }

    /// The text of the report, with no content.
    pub fn text(&self, active: &str) -> String {
        let mut out = format!("rekey: the active key is {active:?}\n");
        for (table, n) in RekeyTable::ALL.iter().zip(self.moved) {
            out.push_str(&format!("  {}: {n} rows moved\n", table.label()));
        }
        for (kid, n) in &self.left {
            if kid != active {
                out.push_str(&format!("  rows left on the key {kid:?}: {n}\n"));
            }
        }
        if self.changed > 0 {
            out.push_str(&format!(
                "  {} rows were changed by another writer during the last round: run the command again\n",
                self.changed
            ));
        }
        if self.unreadable > 0 {
            out.push_str(&format!(
                "  {} rows do not open (ids: {}): the key of these rows is not in the file, or the stored value changed; the command left them as they are\n",
                self.unreadable,
                self.unreadable_ids.join(", ")
            ));
        }
        for kid in &self.check_dropped {
            out.push_str(&format!(
                "  0 rows use the key {kid:?}: its check value is dropped from the database; you can remove its block key from the file\n"
            ));
        }
        if self.stopped {
            out.push_str("  the command stopped before the end: run it again to finish\n");
        }
        out
    }

    /// Whether the command finished its work for the keys that it visited: it did not stop, no row failed, and no row
    /// is left on a key that it visited. `from` is the option `--from`.
    pub fn complete(&self, active: &str, from: Option<&str>) -> bool {
        !self.stopped
            && self.changed == 0
            && self.unreadable == 0
            && self
                .left
                .iter()
                .all(|(k, n)| *n == 0 || k == active || from.is_some_and(|from| from != k.as_str()))
    }
}

impl Target<'_> {
    pub(super) async fn page(
        &self,
        table: RekeyTable,
        active: &str,
        from: &str,
        after: &RekeyCursor,
        limit: u32,
    ) -> Result<Vec<RekeyRow>, String> {
        let (active, from, after) = (active.to_string(), from.to_string(), after.clone());
        match self {
            Target::Store(store) => {
                let at = At { store };
                in_tx!(
                    at,
                    [table, active, from, after, limit],
                    tx,
                    tx.rekey_page(table, &active, &from, &after, limit)
                )
                .map_err(|e| format!("inbox storage: {e}"))
            }
            Target::Sqlite(conn) => super::items_db::in_tx(&mut conn.borrow_mut(), |tx| {
                super::items_db::ready(tx.rekey_page(table, &active, &from, &after, limit))
            })
            .map_err(|e| format!("inbox storage: {e}")),
        }
    }

    /// Writes the swaps of one batch in one transaction. The result is the rows that changed and the rows that
    /// another writer had changed first.
    pub(super) async fn swap(
        &self,
        table: RekeyTable,
        swaps: Vec<(RekeyRow, Sealed)>,
    ) -> Result<(u64, u64), String> {
        match self {
            Target::Store(store) => {
                let at = At { store };
                in_tx!(at, [table, swaps], tx, swap_all(tx, table, &swaps))
                    .map_err(|e| format!("inbox storage: {e}"))
            }
            Target::Sqlite(conn) => super::items_db::in_tx(&mut conn.borrow_mut(), |tx| {
                super::items_db::ready(swap_all(tx, table, &swaps))
            })
            .map_err(|e| format!("inbox storage: {e}")),
        }
    }

    /// The rows of each key, and the drop of the check value of each key in `old` that has no row.
    async fn finish(&self, old: Vec<String>) -> Result<(Vec<(String, u64)>, Vec<String>), String> {
        match self {
            Target::Store(store) => {
                let at = At { store };
                in_tx!(at, [old], tx, finish_all(tx, &old))
                    .map_err(|e| format!("inbox storage: {e}"))
            }
            Target::Sqlite(conn) => super::items_db::in_tx(&mut conn.borrow_mut(), |tx| {
                super::items_db::ready(finish_all(tx, &old))
            })
            .map_err(|e| format!("inbox storage: {e}")),
        }
    }
}

/// The store alone, for the macro `in_tx!`.
struct At<'a> {
    store: &'a Store,
}

async fn swap_all<T: ItemTx>(
    tx: &mut T,
    table: RekeyTable,
    swaps: &[(RekeyRow, Sealed)],
) -> Result<(u64, u64), super::db::DbError> {
    let (mut done, mut lost) = (0, 0);
    for (row, new) in swaps {
        if tx.rekey_swap(table, row, new).await? {
            done += 1;
        } else {
            lost += 1;
        }
    }
    Ok((done, lost))
}

async fn finish_all<T: ItemTx>(
    tx: &mut T,
    old: &[String],
) -> Result<(Vec<(String, u64)>, Vec<String>), super::db::DbError> {
    let mut dropped = Vec::new();
    for kid in old {
        if tx.drop_key_check(kid).await? {
            dropped.push(kid.clone());
        }
    }
    Ok((tx.kid_counts().await?, dropped))
}

/// The field and the associated data of a row.
fn aad_of(table: RekeyTable, row: &RekeyRow) -> Vec<u8> {
    match table {
        RekeyTable::Items => content_box::aad(&row.id, FIELD_BODY, 0, row.effect.as_deref()),
        RekeyTable::Turns => content_box::aad(&row.id, FIELD_TURN, row.seq.max(0) as u64, None),
        RekeyTable::Intents => content_box::aad(&row.id, FIELD_INTENT, 0, None),
        RekeyTable::Hints => content_box::aad(&row.id, FIELD_HINTS, 0, None),
    }
}

/// The rows of a round that do not open.
#[derive(Default)]
struct Unreadable {
    count: u64,
    ids: Vec<String>,
}

/// Seals again each row of a page. A row that does not open is counted and left.
fn reseal(
    content: &ContentBox,
    table: RekeyTable,
    rows: Vec<RekeyRow>,
    unreadable: &mut Unreadable,
) -> Result<Vec<(RekeyRow, Sealed)>, String> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let aad = aad_of(table, &row);
        let Some(plain) = content.open(&row.sealed.ct, &aad) else {
            unreadable.count += 1;
            if unreadable.ids.len() < NAMED_UNREADABLE {
                unreadable.ids.push(format!("{}:{}", table.label(), row.id));
            }
            continue;
        };
        let ct = content.seal(&plain, &aad)?;
        let kid = ContentBox::kid_of(&ct)
            .ok_or("internal error: the sealed value has no key id")?
            .to_string();
        out.push((row, Sealed { kid, ct }));
    }
    Ok(out)
}

/// Runs the command. It returns the report, also when it stopped.
pub async fn run(
    target: &Target<'_>,
    content: &ContentBox,
    options: &Options,
) -> Result<Report, String> {
    let active = content.active_kid().to_string();
    let from = options.from.clone().unwrap_or_default();
    if let Some(kid) = &options.from {
        if *kid == active {
            return Err(format!(
                "--from {kid:?} is the active key: the command seals again with the active key, so it has nothing to do for it; give the id of an old key"
            ));
        }
        if !content.has_kid(kid) {
            return Err(format!(
                "--from {kid:?} is not in the file: the command needs the secret of the old key to open its contents; add the block key {kid:?} with its secret (the file has {}); for a key that is lost, use `portal inbox content-purge`",
                content
                    .kids()
                    .map(|k| format!("{k:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let batch = options.batch.max(1);
    let mut report = Report::default();
    let mut batches = 0usize;
    let mut round = 0;
    'rounds: loop {
        round += 1;
        let mut lost_round = 0u64;
        let mut unreadable = Unreadable::default();
        for (index, table) in RekeyTable::ALL.into_iter().enumerate() {
            let mut after = RekeyCursor {
                id: String::new(),
                seq: -1,
            };
            loop {
                let rows = target.page(table, &active, &from, &after, batch).await?;
                let Some(last) = rows.last() else { break };
                after = RekeyCursor {
                    id: last.id.clone(),
                    seq: last.seq,
                };
                let full = rows.len() as u32 >= batch;
                let swaps = reseal(content, table, rows, &mut unreadable)?;
                if !swaps.is_empty() {
                    let (done, lost) = target.swap(table, swaps).await?;
                    report.moved[index] += done;
                    lost_round += lost;
                }
                batches += 1;
                if options.stop_after_batches.is_some_and(|n| batches >= n) {
                    report.stopped = true;
                    report.unreadable = unreadable.count;
                    report.unreadable_ids = unreadable.ids;
                    break 'rounds;
                }
                if !full {
                    break;
                }
            }
        }
        report.unreadable = unreadable.count;
        report.unreadable_ids = unreadable.ids;
        report.changed = lost_round;
        // A writer changed some rows after the read of the command, and kept them as they are. That writer seals with
        // its own active key: read the rows again.
        if lost_round == 0 || round >= MAX_ROUNDS {
            break;
        }
    }
    // A stop checks no key: the rows are not all moved.
    let old: Vec<String> = if report.stopped {
        Vec::new()
    } else {
        content
            .kids()
            .filter(|k| *k != active && (from.is_empty() || *k == from))
            .map(str::to_string)
            .collect()
    };
    let (left, dropped) = target.finish(old).await?;
    report.left = left;
    report.check_dropped = dropped;
    Ok(report)
}

/// `portal inbox rekey` on a SQLite file. It refuses while an instance runs: the file has one writer at a time, and
/// the running portal holds its sessions in it.
pub fn open_sqlite(path: &Path) -> Result<Target<'static>, String> {
    if crate::store::sqlite_instance_running(path) {
        return Err(format!(
            "the portal runs and holds the SQLite file {}: stop it first, or use PostgreSQL",
            path.display()
        ));
    }
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| format!("storage.database: cannot open {}: {e}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    Ok(Target::Sqlite(std::cell::RefCell::new(conn)))
}
