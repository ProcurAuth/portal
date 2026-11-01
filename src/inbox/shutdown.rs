//! The stop signal of the tasks of the inbox.
//!
//! The portal decides when the process stops. At that time it calls [`Shutdown::trigger`]. A
//! lease that the instance loses triggers it too: the portal then stops the process.

use tokio::sync::watch;

#[derive(Clone)]
pub struct Shutdown {
    tx: watch::Sender<bool>,
}

impl Shutdown {
    pub fn new() -> Shutdown {
        Shutdown {
            tx: watch::channel(false).0,
        }
    }

    pub fn trigger(&self) {
        self.tx.send_replace(true);
    }

    pub fn is_triggered(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolves once the stop was triggered.
    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}
