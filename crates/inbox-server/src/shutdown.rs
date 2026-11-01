//! Process-wide shutdown signal.

use tokio::sync::watch;

#[derive(Clone)]
pub struct Shutdown {
    tx: watch::Sender<bool>,
}

impl Shutdown {
    pub fn new() -> Shutdown {
        Shutdown { tx: watch::channel(false).0 }
    }

    pub fn trigger(&self) {
        self.tx.send_replace(true);
    }

    pub fn is_triggered(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolves once shutdown has been triggered.
    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }

    /// Triggers shutdown on SIGINT or SIGTERM.
    pub fn listen_for_signals(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
            tokio::select! {
                _ = term.recv() => tracing::info!(event = "signal", signal = "SIGTERM"),
                _ = int.recv() => tracing::info!(event = "signal", signal = "SIGINT"),
            }
            this.trigger();
        });
    }
}
