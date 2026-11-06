use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::{KcpError, KcpResult};

pub(super) struct Lifecycle {
    open: Mutex<bool>,
    stopped: Notify,
}

impl Lifecycle {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self { open: Mutex::new(true), stopped: Notify::new() })
    }

    pub(super) fn is_open(&self) -> bool {
        *self.open.lock().expect("lifecycle lock poisoned")
    }

    pub(super) fn try_is_open(&self) -> KcpResult<bool> {
        self.open
            .try_lock()
            .map(|open| *open)
            .map_err(|error| KcpError::IoError(format!("lifecycle unavailable: {error}")))
    }

    pub(super) fn with_open<T>(&self, commit: impl FnOnce() -> T) -> KcpResult<T> {
        let open = self.open.lock().expect("lifecycle lock poisoned");
        if !*open {
            return Err(KcpError::IoError("send worker stopped".to_string()));
        }
        Ok(commit())
    }

    pub(super) fn close(&self) {
        let mut open = self.open.lock().expect("lifecycle lock poisoned");
        *open = false;
        self.stopped.notify_waiters();
    }

    pub(super) async fn closed(&self) {
        let stopped = self.stopped.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        if self.is_open() {
            stopped.await;
        }
    }
}

pub(super) struct StopGuard(pub(super) Arc<Lifecycle>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}
