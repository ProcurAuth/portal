//! Standby replication: lets another instance take over when the node running Inbox is lost.
//!
//! The database stays on a local disk (SQLite WAL needs a real local filesystem). A directory shared by every
//! instance that may run Inbox (`replica.dir`, a plain-file volume) holds:
//!
//! * `snapshot-<epoch>.db`: a consistent copy of the database (SQLite online backup), rewritten when the database
//!   changed and whenever an alert, enrollment or revocation is accepted, before the answer is sent;
//! * `lease.json`: who owns the directory. An instance serves only after it owns the lease; it renews it
//!   every third of `lease_ttl` and stops by itself when it cannot (fencing), so a lost instance that comes
//!   back never writes next to its successor;
//! * `claim-<epoch>`: created exclusively to take an epoch, so two standbys cannot both win it.
//!
//! A new owner takes the epoch after the last one, restores the newest snapshot that passes a
//! consistency check, and writes only `snapshot-<its epoch>.db`: a stale instance can only rewrite a file
//! nobody reads any more.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::config::Replica as ReplicaConfig;
use crate::shutdown::Shutdown;
use crate::util::{now_ms, random_id};

const LEASE_FILE: &str = "lease.json";
/// Polling period of the worker threads (also the granularity of shutdown).
const TICK: Duration = Duration::from_millis(200);
/// Snapshots of epochs older than the previous one are removed.
const KEEP_EPOCHS: u64 = 1;

// ---------------------------------------------------------------- termination while waiting

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_signal: libc::c_int) {
    TERMINATED.store(true, SeqCst);
}

/// While an instance waits for the lease the Tokio runtime does not exist yet: SIGTERM and SIGINT only set a flag.
fn catch_termination() {
    // SAFETY: the handler only stores into an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, on_terminate as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_terminate as *const () as libc::sighandler_t);
    }
}

// ---------------------------------------------------------------- lease

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LeaseRecord {
    epoch: u64,
    owner: String,
    /// Incremented at every renewal: what the other instances watch to see the owner is alive.
    beat: u64,
    /// Written by an instance that stops cleanly: the next one does not have to wait for `lease_ttl`.
    released: bool,
}

#[derive(Debug)]
enum RenewError {
    /// Another instance owns the lease now.
    Lost(String),
    /// The shared directory could not be read or written.
    Io(String),
}

/// The lease owned by this instance.
struct Lease {
    dir: PathBuf,
    owner: String,
    epoch: u64,
    beat: u64,
}

fn read_lease(dir: &Path) -> Result<Option<LeaseRecord>, String> {
    match fs::read(dir.join(LEASE_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{} is not valid: {e}", LEASE_FILE)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {LEASE_FILE}: {e}")),
    }
}

fn write_lease(dir: &Path, record: &LeaseRecord) -> Result<(), String> {
    let tmp = dir.join(format!(".{LEASE_FILE}.{}.tmp", record.owner));
    let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    let mut file = File::create(&tmp).map_err(|e| format!("cannot write {LEASE_FILE}: {e}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("cannot write {LEASE_FILE}: {e}"))?;
    replace_file(&tmp, &dir.join(LEASE_FILE)).map_err(|e| format!("cannot write {LEASE_FILE}: {e}"))
}

/// Renames `from` over `to`. The rename is atomic on a POSIX filesystem; one that refuses to replace an existing
/// file gets a remove then a rename instead.
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(first) => {
            if fs::remove_file(to).is_err() {
                return Err(first);
            }
            fs::rename(from, to)
        }
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

fn epoch_of(name: &str, prefix: &str, suffix: &str) -> Option<u64> {
    name.strip_prefix(prefix)?.strip_suffix(suffix)?.parse().ok()
}

/// Highest epoch mentioned by the lease, a claim or a snapshot.
fn highest_epoch(dir: &Path) -> Result<u64, String> {
    let mut highest = read_lease(dir)?.map_or(0, |l| l.epoch);
    for entry in fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))? {
        let name = entry.map_err(|e| e.to_string())?.file_name().to_string_lossy().to_string();
        for epoch in [epoch_of(&name, "claim-", ""), epoch_of(&name, "snapshot-", ".db")]
            .into_iter()
            .flatten()
        {
            highest = highest.max(epoch);
        }
    }
    Ok(highest)
}

/// Creates `claim-<epoch>` exclusively: false when another instance already took that epoch.
fn claim(dir: &Path, epoch: u64, owner: &str) -> Result<bool, String> {
    match OpenOptions::new().write(true).create_new(true).open(dir.join(format!("claim-{epoch}"))) {
        Ok(mut file) => {
            let _ = file.write_all(owner.as_bytes());
            let _ = file.sync_all();
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("cannot claim epoch {epoch}: {e}")),
    }
}

impl Lease {
    /// Waits until the lease is free (released, or not renewed for `ttl`), then takes the next epoch.
    /// `None` when `stop` became true first.
    fn acquire(
        dir: &Path,
        ttl: Duration,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<Lease>, String> {
        let owner = random_id();
        let poll = (ttl / 10).clamp(Duration::from_millis(100), Duration::from_secs(1));
        let mut observed: Option<(LeaseRecord, Instant)> = None;
        let mut announced = false;
        loop {
            if stop() {
                return Ok(None);
            }
            match read_lease(dir) {
                Err(e) => tracing::warn!(event = "replica_lease_unreadable", error = %e),
                Ok(current) => {
                    // The owner is alive as long as its record keeps changing; the clock used is ours alone.
                    let expired = match (&current, &observed) {
                        (Some(now), Some((before, since))) if now == before => {
                            since.elapsed() >= ttl
                        }
                        _ => false,
                    };
                    match (&current, &observed) {
                        (Some(now), Some((before, _))) if now == before => {}
                        (Some(now), _) => observed = Some((now.clone(), Instant::now())),
                        (None, _) => observed = None,
                    }
                    if current.as_ref().is_none_or(|l| l.released) || expired {
                        let epoch = highest_epoch(dir)? + 1;
                        if claim(dir, epoch, &owner)? {
                            let record = LeaseRecord {
                                epoch,
                                owner: owner.clone(),
                                beat: 0,
                                released: false,
                            };
                            write_lease(dir, &record)?;
                            // A competitor that claimed the epoch after ours would have overwritten the record.
                            std::thread::sleep(poll);
                            if read_lease(dir)?.as_ref() == Some(&record) {
                                return Ok(Some(Lease {
                                    dir: dir.to_path_buf(),
                                    owner,
                                    epoch,
                                    beat: 0,
                                }));
                            }
                            tracing::warn!(event = "replica_lease_contended", epoch);
                        }
                    } else if !announced {
                        announced = true;
                        tracing::info!(
                            event = "replica_waiting",
                            reason = "the lease is held by another instance",
                            ttl_ms = ttl.as_millis() as u64
                        );
                    }
                }
            }
            std::thread::sleep(poll);
        }
    }

    fn renew(&mut self) -> Result<(), RenewError> {
        match read_lease(&self.dir).map_err(RenewError::Io)? {
            Some(l) if l.epoch == self.epoch && l.owner == self.owner => {}
            Some(l) => {
                return Err(RenewError::Lost(format!("epoch {} is owned by {}", l.epoch, l.owner)));
            }
            None => return Err(RenewError::Lost("the lease file is gone".into())),
        }
        self.beat += 1;
        write_lease(&self.dir, &self.record(false)).map_err(RenewError::Io)
    }

    fn record(&self, released: bool) -> LeaseRecord {
        LeaseRecord { epoch: self.epoch, owner: self.owner.clone(), beat: self.beat, released }
    }

    /// Lets the next instance take over at once. Only when the lease is still ours.
    fn release(&mut self) {
        match read_lease(&self.dir) {
            Ok(Some(l)) if l.epoch == self.epoch && l.owner == self.owner => {
                self.beat += 1;
                match write_lease(&self.dir, &self.record(true)) {
                    Ok(()) => tracing::info!(event = "replica_lease_released", epoch = self.epoch),
                    Err(e) => tracing::warn!(event = "replica_lease_release_failed", error = %e),
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- snapshots

fn snapshot_path(dir: &Path, epoch: u64) -> PathBuf {
    dir.join(format!("snapshot-{epoch}.db"))
}

/// `(epoch, path)` of every snapshot, newest epoch first.
fn snapshots(dir: &Path) -> Result<Vec<(u64, PathBuf)>, String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if let Some(epoch) = epoch_of(&entry.file_name().to_string_lossy(), "snapshot-", ".db") {
            found.push((epoch, entry.path()));
        }
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    Ok(found)
}

/// Writes a consistent copy of the database read through `source` into `snapshot-<epoch>.db`.
fn take_snapshot(source: &Connection, dir: &Path, epoch: u64, owner: &str) -> Result<u64, String> {
    let tmp = dir.join(format!(".snapshot-{epoch}.{owner}.tmp"));
    let _ = fs::remove_file(&tmp);
    let copied = (|| -> Result<(), String> {
        let mut destination =
            Connection::open(&tmp).map_err(|e| format!("cannot create the snapshot: {e}"))?;
        Backup::new(source, &mut destination)
            .and_then(|backup| backup.run_to_completion(512, Duration::ZERO, None))
            .map_err(|e| format!("snapshot backup failed: {e}"))?;
        drop(destination);
        File::open(&tmp)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("cannot sync the snapshot: {e}"))
    })();
    if let Err(e) = copied {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let bytes = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if let Err(e) = replace_file(&tmp, &snapshot_path(dir, epoch)) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot publish the snapshot: {e}"));
    }
    sync_dir(dir);
    Ok(bytes)
}

/// Removes what older epochs left behind.
fn prune(dir: &Path, epoch: u64) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let old_snapshot =
            epoch_of(&name, "snapshot-", ".db").is_some_and(|e| e + KEEP_EPOCHS < epoch);
        let old_claim = epoch_of(&name, "claim-", "").is_some_and(|e| e + KEEP_EPOCHS < epoch);
        let old_tmp = name.starts_with('.')
            && name.ends_with(".tmp")
            && !name.contains(&format!("-{epoch}."));
        if old_snapshot || old_claim || old_tmp {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn remove_database_files(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = fs::remove_file(PathBuf::from(name));
    }
}

/// Opens a copy of a snapshot and runs SQLite's consistency check on it.
fn verify(copy: &Path) -> Result<(), String> {
    let conn = Connection::open(copy).map_err(|e| e.to_string())?;
    let verdict: String =
        conn.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    drop(conn);
    if verdict == "ok" { Ok(()) } else { Err(verdict) }
}

/// Replaces the local database by the newest snapshot that passes the consistency check.
/// Returns its epoch, or `None` when there is no snapshot (first start: the local database is kept).
fn restore(dir: &Path, local: &Path) -> Result<Option<u64>, String> {
    let mut staged = local.as_os_str().to_owned();
    staged.push(".restore");
    let staged = PathBuf::from(staged);
    for (epoch, path) in snapshots(dir)? {
        remove_database_files(&staged);
        let attempt = fs::copy(&path, &staged)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))
            .and_then(|_| verify(&staged));
        match attempt {
            Ok(()) => {
                // The consistency check may have left a journal next to the copy: only the main file moves.
                for suffix in ["-wal", "-shm", "-journal"] {
                    let mut name = staged.as_os_str().to_owned();
                    name.push(suffix);
                    let _ = fs::remove_file(PathBuf::from(name));
                }
                remove_database_files(local);
                fs::rename(&staged, local)
                    .map_err(|e| format!("cannot restore the database: {e}"))?;
                return Ok(Some(epoch));
            }
            Err(e) => {
                tracing::error!(event = "replica_snapshot_rejected", epoch, error = %e);
                remove_database_files(&staged);
            }
        }
    }
    if !snapshots(dir)?.is_empty() {
        return Err("every snapshot in replica.dir failed its consistency check".into());
    }
    Ok(None)
}

// ---------------------------------------------------------------- running replication

/// Counters and gauges exported by telemetry.
#[derive(Default)]
pub struct Stats {
    pub epoch: AtomicU64,
    pub snapshots: AtomicU64,
    pub failures: AtomicU64,
    pub last_snapshot_ms: AtomicI64,
}

enum Message {
    /// Resolves once a snapshot that started after the request has been published.
    Sync(oneshot::Sender<Result<(), String>>),
    /// Final snapshot, then the thread ends.
    Finish,
}

struct Shared {
    stop: AtomicBool,
    fenced: AtomicBool,
}

/// Handle used by the request handlers. A disabled replica does nothing.
#[derive(Clone)]
pub struct Replica {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    tx: mpsc::Sender<Message>,
    sync_timeout: Duration,
    sync_alerts: bool,
}

impl Replica {
    pub fn disabled() -> Replica {
        Replica { inner: None }
    }

    /// Whether an alert must be replicated before it is acknowledged.
    pub fn strict_alerts(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| i.sync_alerts)
    }

    /// Waits until everything committed so far is in the shared directory.
    pub async fn sync(&self) -> Result<(), String> {
        let Some(inner) = &self.inner else { return Ok(()) };
        let (tx, rx) = oneshot::channel();
        inner.tx.send(Message::Sync(tx)).map_err(|_| "the replicator is stopped".to_string())?;
        match tokio::time::timeout(inner.sync_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("the replicator stopped".into()),
            Err(_) => Err(format!("no snapshot within {}ms", inner.sync_timeout.as_millis())),
        }
    }
}

/// An instance that owns the lease and has restored the database; replication has not started yet.
pub struct Boot {
    config: ReplicaConfig,
    lease: Lease,
}

/// Waits for the lease and restores the database. `None` when the process was told to stop while waiting.
/// Runs before the database is opened: a standby serves nothing.
pub fn boot(config: &ReplicaConfig, local: &Path) -> Result<Option<Boot>, String> {
    fs::create_dir_all(&config.dir)
        .map_err(|e| format!("replica.dir: cannot use {}: {e}", config.dir.display()))?;
    catch_termination();
    let Some(lease) = Lease::acquire(&config.dir, config.lease_ttl, &|| TERMINATED.load(SeqCst))?
    else {
        return Ok(None);
    };
    let restored = restore(&config.dir, local)?;
    tracing::info!(
        event = "replica_ready",
        epoch = lease.epoch,
        restored_epoch = restored.map(|e| e as i64).unwrap_or(-1)
    );
    Ok(Some(Boot { config: config.clone(), lease }))
}

/// The running replication: a snapshot thread and a lease thread.
pub struct Running {
    replica: Replica,
    shared: Arc<Shared>,
    snapshotter: JoinHandle<()>,
    leaser: JoinHandle<()>,
    tx: mpsc::Sender<Message>,
}

impl Boot {
    /// Starts the snapshot and lease threads. The database file must exist.
    pub fn start(
        self,
        local: &Path,
        shutdown: Shutdown,
        stats: Arc<Stats>,
    ) -> Result<Running, String> {
        let Boot { config, lease } = self;
        let source = Connection::open_with_flags(local, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| format!("replica: cannot open {}: {e}", local.display()))?;
        source.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        stats.epoch.store(lease.epoch, SeqCst);
        let shared =
            Arc::new(Shared { stop: AtomicBool::new(false), fenced: AtomicBool::new(false) });
        let (tx, rx) = mpsc::channel();

        let worker = Worker {
            source,
            dir: config.dir.clone(),
            epoch: lease.epoch,
            owner: lease.owner.clone(),
            interval: config.snapshot_interval,
            rx,
            stats,
            shared: shared.clone(),
        };
        let snapshotter = std::thread::Builder::new()
            .name("inbox-replica".into())
            .spawn(move || worker.run())
            .map_err(|e| format!("cannot start the replica thread: {e}"))?;
        let ttl = config.lease_ttl;
        let leaser = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("inbox-lease".into())
                .spawn(move || lease_loop(lease, ttl, shared, shutdown))
                .map_err(|e| format!("cannot start the lease thread: {e}"))?
        };
        let replica = Replica {
            inner: Some(Arc::new(Inner {
                tx: tx.clone(),
                sync_timeout: config.sync_timeout,
                sync_alerts: config.sync_alerts,
            })),
        };
        Ok(Running { replica, shared, snapshotter, leaser, tx })
    }
}

impl Running {
    pub fn replica(&self) -> Replica {
        self.replica.clone()
    }

    /// True when another instance took the lease: the process must stop and report a failure.
    pub fn fenced(&self) -> bool {
        self.shared.fenced.load(SeqCst)
    }

    /// Final snapshot (the database is closed), then the lease is handed over.
    pub fn finish(self) {
        let _ = self.tx.send(Message::Finish);
        let _ = self.snapshotter.join();
        self.shared.stop.store(true, SeqCst);
        let _ = self.leaser.join();
    }
}

fn fence(shared: &Shared, shutdown: &Shutdown, reason: &str) {
    tracing::error!(
        event = "replica_fenced",
        reason,
        action = "stopping: another instance may own the database now"
    );
    shared.fenced.store(true, SeqCst);
    shutdown.trigger();
}

fn lease_loop(mut lease: Lease, ttl: Duration, shared: Arc<Shared>, shutdown: Shutdown) {
    let every = ttl / 3;
    let mut last_renewal = Instant::now();
    let mut last_success = Instant::now();
    while !shared.stop.load(SeqCst) {
        std::thread::sleep(TICK);
        if last_renewal.elapsed() < every {
            continue;
        }
        last_renewal = Instant::now();
        match lease.renew() {
            Ok(()) => last_success = Instant::now(),
            Err(RenewError::Lost(why)) => {
                return fence(&shared, &shutdown, &format!("lease lost: {why}"));
            }
            Err(RenewError::Io(why)) => {
                tracing::warn!(event = "replica_renew_failed", error = %why);
                if last_success.elapsed() >= ttl {
                    return fence(
                        &shared,
                        &shutdown,
                        "the lease could not be renewed within its ttl",
                    );
                }
            }
        }
    }
    lease.release();
}

struct Worker {
    source: Connection,
    dir: PathBuf,
    epoch: u64,
    owner: String,
    interval: Duration,
    rx: mpsc::Receiver<Message>,
    stats: Arc<Stats>,
    shared: Arc<Shared>,
}

impl Worker {
    fn run(self) {
        let mut data_version = self.data_version();
        // The first snapshot of an epoch makes it the one the next instance restores.
        let mut dirty = true;
        let mut last = Instant::now() - self.interval;
        let mut ok = false;
        loop {
            if self.shared.fenced.load(SeqCst) {
                return;
            }
            match self.rx.recv_timeout(TICK) {
                Ok(Message::Sync(first)) => {
                    let mut waiters = vec![first];
                    let mut finish = false;
                    while let Ok(next) = self.rx.try_recv() {
                        match next {
                            Message::Sync(w) => waiters.push(w),
                            Message::Finish => finish = true,
                        }
                    }
                    let version = self.data_version();
                    dirty |= version != data_version;
                    data_version = version;
                    let answer = if dirty || !ok {
                        let result = self.snapshot();
                        ok = result.is_ok();
                        dirty = result.is_err();
                        last = Instant::now();
                        result
                    } else {
                        Ok(())
                    };
                    for waiter in waiters {
                        let _ = waiter.send(answer.clone());
                    }
                    if finish {
                        return self.finish();
                    }
                }
                Ok(Message::Finish) => return self.finish(),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let version = self.data_version();
                    dirty |= version != data_version;
                    data_version = version;
                    if (dirty || !ok) && last.elapsed() >= self.interval {
                        let result = self.snapshot();
                        ok = result.is_ok();
                        dirty = result.is_err();
                        last = Instant::now();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Changes committed by another connection since the previous call (the database thread, not this one).
    fn data_version(&self) -> i64 {
        self.source.query_row("PRAGMA data_version", [], |r| r.get(0)).unwrap_or(-1)
    }

    fn snapshot(&self) -> Result<(), String> {
        match take_snapshot(&self.source, &self.dir, self.epoch, &self.owner) {
            Ok(bytes) => {
                let first = self.stats.snapshots.fetch_add(1, SeqCst) == 0;
                self.stats.last_snapshot_ms.store(now_ms(), SeqCst);
                if first {
                    // One line per process proves the shared directory works; the following ones are routine.
                    tracing::info!(event = "replica_first_snapshot", epoch = self.epoch, bytes, dir = %self.dir.display());
                } else {
                    tracing::debug!(event = "replica_snapshot", epoch = self.epoch, bytes);
                }
                prune(&self.dir, self.epoch);
                Ok(())
            }
            Err(e) => {
                self.stats.failures.fetch_add(1, SeqCst);
                tracing::warn!(event = "replica_snapshot_failed", error = %e);
                Err(e)
            }
        }
    }

    fn finish(&self) {
        if self.shared.fenced.load(SeqCst) {
            return;
        }
        if let Err(e) = self.snapshot() {
            tracing::error!(event = "replica_final_snapshot_failed", error = %e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "inbox-replica-{name}-{}-{}",
            std::process::id(),
            random_id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn database(path: &Path, rows: &[&str]) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; CREATE TABLE IF NOT EXISTS t (v TEXT NOT NULL);",
        )
        .unwrap();
        for row in rows {
            conn.execute("INSERT INTO t (v) VALUES (?1)", [row]).unwrap();
        }
        conn
    }

    fn rows(path: &Path) -> Vec<String> {
        let conn = Connection::open(path).unwrap();
        let mut stmt = conn.prepare("SELECT v FROM t ORDER BY rowid").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    const TTL: Duration = Duration::from_millis(600);

    fn acquire(dir: &Path) -> Lease {
        Lease::acquire(dir, TTL, &|| false).unwrap().unwrap()
    }

    #[test]
    fn a_snapshot_restores_into_a_new_place() {
        let shared = temp("snap");
        let local = temp("local");
        let source = database(&local.join("a.db"), &["one", "two"]);
        take_snapshot(&source, &shared, 3, "me").unwrap();
        // The WAL is not checkpointed: the snapshot must still contain the committed rows.
        assert!(local.join("a.db-wal").exists());
        let target = local.join("b.db");
        fs::write(&target, b"stale local database").unwrap();
        assert_eq!(restore(&shared, &target).unwrap(), Some(3));
        assert_eq!(rows(&target), ["one", "two"]);
        assert!(!local.join("b.db.restore").exists());
    }

    #[test]
    fn no_snapshot_keeps_the_local_database() {
        let shared = temp("none");
        let local = temp("keep");
        drop(database(&local.join("a.db"), &["kept"]));
        assert_eq!(restore(&shared, &local.join("a.db")).unwrap(), None);
        assert_eq!(rows(&local.join("a.db")), ["kept"]);
    }

    #[test]
    fn a_corrupt_newest_snapshot_falls_back_to_the_previous_one() {
        let shared = temp("corrupt");
        let local = temp("fallback");
        let source = database(&local.join("a.db"), &["good"]);
        take_snapshot(&source, &shared, 1, "me").unwrap();
        take_snapshot(&source, &shared, 2, "me").unwrap();
        let mut bytes = fs::read(snapshot_path(&shared, 2)).unwrap();
        for byte in bytes.iter_mut().skip(100).take(4000) {
            *byte = 0xff;
        }
        fs::write(snapshot_path(&shared, 2), bytes).unwrap();
        assert_eq!(restore(&shared, &local.join("b.db")).unwrap(), Some(1));
        assert_eq!(rows(&local.join("b.db")), ["good"]);
        // All of them corrupt: refuse to start with an empty database.
        fs::write(snapshot_path(&shared, 1), b"not a database at all, not even close").unwrap();
        assert!(restore(&shared, &local.join("c.db")).is_err());
    }

    #[test]
    fn a_released_lease_is_taken_at_once_and_epochs_increase() {
        let dir = temp("release");
        let mut first = acquire(&dir);
        assert_eq!(first.epoch, 1);
        first.renew().unwrap();
        first.release();
        let started = Instant::now();
        let second = acquire(&dir);
        assert_eq!(second.epoch, 2);
        assert!(started.elapsed() < TTL, "a released lease needs no waiting");
    }

    #[test]
    fn a_lease_that_is_not_renewed_is_taken_after_the_ttl_and_the_old_owner_is_fenced() {
        let dir = temp("expire");
        let mut old = acquire(&dir);
        let started = Instant::now();
        let new = acquire(&dir);
        assert_eq!(new.epoch, old.epoch + 1);
        assert!(started.elapsed() >= TTL, "the standby must wait for the ttl");
        // The lost instance comes back: it must see it no longer owns the lease.
        assert!(matches!(old.renew(), Err(RenewError::Lost(_))));
        // And it must not release what belongs to the new owner.
        old.release();
        assert!(!read_lease(&dir).unwrap().unwrap().released);
    }

    #[test]
    fn a_renewing_owner_keeps_its_standby_waiting_and_stop_ends_the_wait() {
        let dir = temp("renewing");
        let mut owner = acquire(&dir);
        let waiting = {
            let dir = dir.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let handle = std::thread::spawn(move || {
                Lease::acquire(&dir, TTL, &|| flag.load(SeqCst)).unwrap().is_none()
            });
            (handle, stop)
        };
        for _ in 0..8 {
            std::thread::sleep(TTL / 3);
            owner.renew().unwrap();
        }
        waiting.1.store(true, SeqCst);
        assert!(waiting.0.join().unwrap(), "the standby gave up without ever owning the lease");
        assert_eq!(read_lease(&dir).unwrap().unwrap().epoch, 1);
    }

    #[test]
    fn an_orphan_claim_only_skips_an_epoch() {
        let dir = temp("orphan");
        fs::write(dir.join("claim-1"), "crashed before writing the lease").unwrap();
        assert_eq!(acquire(&dir).epoch, 2);
    }

    #[test]
    fn old_epochs_are_pruned() {
        let dir = temp("prune");
        let local = temp("prune-local");
        let source = database(&local.join("a.db"), &["x"]);
        for epoch in 1..=4 {
            take_snapshot(&source, &dir, epoch, "me").unwrap();
            fs::write(dir.join(format!("claim-{epoch}")), "x").unwrap();
        }
        prune(&dir, 4);
        let left: Vec<u64> = snapshots(&dir).unwrap().into_iter().map(|s| s.0).collect();
        assert_eq!(left, [4, 3]);
        assert!(!dir.join("claim-1").exists() && !dir.join("claim-2").exists());
        assert!(dir.join("claim-3").exists() && dir.join("claim-4").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_waits_for_a_snapshot_that_contains_the_write() {
        let shared = temp("sync");
        let local = temp("sync-local");
        let path = local.join("inbox.db");
        let writer = database(&path, &["before"]);
        let config = ReplicaConfig {
            dir: shared.clone(),
            snapshot_interval: Duration::from_secs(3600),
            lease_ttl: Duration::from_secs(5),
            sync_timeout: Duration::from_secs(5),
            sync_alerts: true,
        };
        let boot = boot(&config, &path).unwrap().unwrap();
        let running = boot.start(&path, Shutdown::new(), Arc::new(Stats::default())).unwrap();
        let replica = running.replica();
        assert!(replica.strict_alerts());
        for i in 0..3 {
            writer.execute("INSERT INTO t (v) VALUES (?1)", [format!("after-{i}")]).unwrap();
            replica.sync().await.unwrap();
            // The interval is an hour: only the sync request can have produced this snapshot.
            let copy = local.join(format!("copy-{i}.db"));
            fs::copy(snapshot_path(&shared, 1), &copy).unwrap();
            assert_eq!(rows(&copy).last().unwrap(), &format!("after-{i}"));
        }
        // Nothing changed: no new snapshot is needed, the answer is immediate.
        replica.sync().await.unwrap();
        drop(writer);
        running.finish();
        assert!(read_lease(&shared).unwrap().unwrap().released);
    }

    #[tokio::test]
    async fn a_disabled_replica_answers_at_once() {
        let replica = Replica::disabled();
        assert!(!replica.strict_alerts());
        replica.sync().await.unwrap();
    }
}
