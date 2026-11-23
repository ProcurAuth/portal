//! The stop of the server on SIGTERM.
//!
//! The instance serves 5 seconds more, so the cluster can remove it from the endpoints of its
//! Service. Then it closes the listeners, tells each connection to close, and waits at most 10
//! seconds for the answers in progress.

use std::error::Error;
use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use hyper::body::{Body, Incoming};
use hyper::rt::{Read, Write};
use hyper::server::conn::http1::{Connection, UpgradeableConnection};
use hyper::service::HttpService;
use tokio::sync::watch;

/// The time between SIGTERM and the close of the listeners.
pub const SERVE_AFTER_SIGNAL: Duration = Duration::from_secs(5);
/// The longest wait for the answers in progress, after the close of the listeners.
pub const DRAIN_LIMIT: Duration = Duration::from_secs(10);

/// The state of the stop: the signal for the connections, and the number of live connections.
pub struct Stop {
    signal: watch::Sender<bool>,
    live: AtomicUsize,
}

impl Default for Stop {
    fn default() -> Self {
        Self {
            signal: watch::channel(false).0,
            live: AtomicUsize::new(0),
        }
    }
}

/// The mark of one live connection. The count goes down when the mark is dropped.
pub struct Live<'a>(&'a Stop);

impl Drop for Live<'_> {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Stop {
    /// Counts one more live connection.
    pub fn enter(&self) -> Live<'_> {
        self.live.fetch_add(1, Ordering::SeqCst);
        Live(self)
    }

    /// The number of live connections.
    pub fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Tells each connection to close: an idle one at once, a busy one after its answer.
    pub fn close_connections(&self) {
        self.signal.send_replace(true);
    }

    fn receiver(&self) -> watch::Receiver<bool> {
        self.signal.subscribe()
    }

    /// Serves one connection until it ends, or until it closes because of the stop.
    pub async fn drive<I, S, B>(&self, connection: Connection<I, S>) -> Result<(), hyper::Error>
    where
        S: HttpService<Incoming, ResBody = B>,
        S::Error: Into<Box<dyn Error + Send + Sync>>,
        I: Read + Write + Unpin,
        B: Body + 'static,
        B::Error: Into<Box<dyn Error + Send + Sync>>,
    {
        let mut stop = self.receiver();
        let mut connection = pin!(connection);
        let mut closing = *stop.borrow_and_update();
        if closing {
            connection.as_mut().graceful_shutdown();
        }
        let mut changed = pin!(stop.changed());
        poll_fn(|cx| {
            if !closing && changed.as_mut().poll(cx).is_ready() {
                closing = true;
                connection.as_mut().graceful_shutdown();
            }
            match connection.as_mut().poll(cx) {
                Poll::Ready(result) => Poll::Ready(result),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    /// Like [`Stop::drive`], for a connection that can upgrade to a WebSocket. The future ends
    /// when the upgrade hands the connection to its new owner.
    pub async fn drive_upgradable<I, S, B>(
        &self,
        connection: UpgradeableConnection<I, S>,
    ) -> Result<(), hyper::Error>
    where
        S: HttpService<Incoming, ResBody = B>,
        S::Error: Into<Box<dyn Error + Send + Sync>>,
        I: Read + Write + Unpin + Send + 'static,
        B: Body + 'static,
        B::Error: Into<Box<dyn Error + Send + Sync>>,
    {
        let mut stop = self.receiver();
        let mut connection = pin!(connection);
        let mut closing = *stop.borrow_and_update();
        if closing {
            connection.as_mut().graceful_shutdown();
        }
        let mut changed = pin!(stop.changed());
        poll_fn(|cx| {
            if !closing && changed.as_mut().poll(cx).is_ready() {
                closing = true;
                connection.as_mut().graceful_shutdown();
            }
            connection.as_mut().poll(cx)
        })
        .await
    }

    /// Waits until no connection is live, at most `limit`. Gives the number that stay live.
    pub async fn drain(&self, limit: Duration) -> usize {
        let end = tokio::time::Instant::now() + limit;
        while self.live() > 0 && tokio::time::Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.live()
    }
}

/// A flag that becomes true at SIGTERM or SIGINT. It serves the code that waits before the
/// runtime of the server exists: an instance that waits for the lease of the replica. A thread
/// with its own small runtime watches the signals.
pub fn terminated_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&flag);
    // If the thread cannot start, the flag stays false: the instance then stops only by a kill.
    let _ = std::thread::Builder::new()
        .name("portal-signals".into())
        .spawn(move || {
            use tokio::signal::unix::{signal, SignalKind};
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let (Ok(mut term), Ok(mut int)) = (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::interrupt()),
                ) else {
                    return;
                };
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
                seen.store(true, Ordering::SeqCst);
            });
        });
    flag
}
