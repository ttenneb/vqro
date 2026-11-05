//! One process-local authoritative epoch with a fallible, coalesced projection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use super::registry::{RegistryDirectory, RegistryEntry};
use super::{OwnershipState, OwnershipStatus, TtyIdentity};

struct Publisher {
    state: Mutex<OwnershipState>,
    endpoint: Option<String>,
    signal: mpsc::SyncSender<()>,
    stopping: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

static GLOBAL: OnceLock<Option<Arc<Publisher>>> = OnceLock::new();

fn global() -> Option<Arc<Publisher>> {
    GLOBAL.get_or_init(|| {
        // Unit tests use injected state/directories; only real private binaries
        // participate in the human-approved actual fixed namespace.
        if cfg!(test) { return None; }
        let key = crate::platform::ownership_server_key()?;
        let endpoint = crate::api::socket_path().to_str().map(str::to_owned);
        let (signal, receiver) = mpsc::sync_channel(1);
        let publisher = Arc::new(Publisher { state: Mutex::new(OwnershipState::new(key)), endpoint, signal, stopping: AtomicBool::new(false), worker: Mutex::new(None) });
        let worker = Arc::clone(&publisher);
        let spawned = std::thread::Builder::new().name("vqro-ownership-publish".into()).spawn(move || {
            while receiver.recv().is_ok() {
                let Some(record) = worker.state.lock().ok().map(|state| state.record()) else { continue; };
                let result = (|| -> Result<RegistryDirectory, String> {
                    let base = crate::platform::ownership_runtime_base().ok_or("fixed user runtime directory is unavailable")?;
                    let directory = RegistryDirectory::prepare(&base).map_err(|error| error.to_string())?;
                    let endpoint = worker.endpoint.clone().ok_or("ownership endpoint is not bounded UTF-8")?;
                    directory.publish(&RegistryEntry { endpoint, ownership: record.clone() }).map_err(|error| error.to_string())?;
                    Ok(directory)
                })();
                match result {
                    Ok(directory) => {
                        // An incomplete snapshot can finish writing after its
                        // pending mutation completed at the SAME epoch. It is
                        // still not a complete publication acknowledgement.
                        if record.complete {
                            if let Ok(mut state) = worker.state.lock() { state.publication_succeeded(record.epoch); }
                        }
                        let empty = worker.state.lock().is_ok_and(|state| state.members.is_empty() && state.pending.is_empty());
                        if worker.stopping.load(Ordering::Acquire) && empty {
                            if let Err(error) = directory.remove(&record.server) { tracing::warn!(%error, "could not remove own ownership record"); }
                            break;
                        }
                    }
                    Err(error) => {
                        // Core lifecycle never waits for or depends on this write.
                        // No timer retry: a later ownership change sends another signal.
                        tracing::error!(%error, "PTY ownership publication unavailable; client-human actions are read-only");
                        if worker.stopping.load(Ordering::Acquire) { break; }
                    }
                }
            }
        });
        match spawned {
            Ok(handle) => { if let Ok(mut slot) = publisher.worker.lock() { *slot = Some(handle); } },
            Err(error) => tracing::error!(%error, "could not start ownership publisher; client-human actions are read-only"),
        }
        let _ = publisher.signal.try_send(());
        Some(publisher)
    }).clone()
}

pub(crate) fn start() {
    let _ = global();
}

/// Pure cached read: the status endpoint must not initialize/probe a publisher.
pub(crate) fn current_status() -> Option<OwnershipStatus> {
    GLOBAL
        .get()?
        .as_ref()?
        .state
        .lock()
        .ok()
        .map(|state| state.status())
}

pub(crate) struct OwnershipChange {
    publisher: Option<Arc<Publisher>>,
    change: Option<u64>,
}

pub(crate) fn begin_change() -> OwnershipChange {
    let publisher = global();
    let change = publisher.as_ref().and_then(|publisher| {
        publisher
            .state
            .lock()
            .ok()
            .map(|mut state| state.begin_change())
    });
    if let Some(publisher) = &publisher {
        let _ = publisher.signal.try_send(());
    }
    OwnershipChange { publisher, change }
}

impl OwnershipChange {
    pub(crate) fn attach(mut self, tty: Option<TtyIdentity>) -> Option<Arc<OwnershipLease>> {
        let change = self.change.take()?;
        let publisher = self.publisher.as_ref()?.clone();
        if let Ok(mut state) = publisher.state.lock() {
            state.finish_add(change, tty);
        }
        let _ = publisher.signal.try_send(());
        Some(Arc::new(OwnershipLease {
            publisher,
            member: change,
        }))
    }
}

impl Drop for OwnershipChange {
    fn drop(&mut self) {
        if let (Some(publisher), Some(change)) = (&self.publisher, self.change.take()) {
            if let Ok(mut state) = publisher.state.lock() {
                state.finish_abort(change);
            }
            let _ = publisher.signal.try_send(());
        }
    }
}

pub(crate) struct OwnershipLease {
    publisher: Arc<Publisher>,
    member: u64,
}

impl Drop for OwnershipLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.publisher.state.lock() {
            let change = state.begin_change();
            state.finish_remove(change, self.member);
        }
        let _ = self.publisher.signal.try_send(());
    }
}

pub(crate) fn stop() {
    let Some(Some(publisher)) = GLOBAL.get() else {
        return;
    };
    publisher.stopping.store(true, Ordering::Release);
    let _ = publisher.signal.try_send(());
    // The runtime has already requested PTY shutdown. Cleanup is bounded and
    // removes only our exact record after all tracked descriptors are released.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        let empty = publisher
            .state
            .lock()
            .is_ok_and(|state| state.members.is_empty() && state.pending.is_empty());
        let finished = publisher.worker.lock().is_ok_and(|worker| {
            worker
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
        });
        if empty && finished {
            // No publisher can race this cleanup and recreate the file.
            if let Ok(mut worker) = publisher.worker.lock() {
                if let Some(handle) = worker.take() {
                    let _ = handle.join();
                }
            }
            if let (Some(base), Some(key)) = (
                crate::platform::ownership_runtime_base(),
                publisher
                    .state
                    .lock()
                    .ok()
                    .map(|state| state.server.clone()),
            ) {
                if let Ok(directory) = RegistryDirectory::prepare(&base) {
                    if let Err(error) = directory.remove(&key) {
                        tracing::warn!(%error, "could not clean own ownership record");
                    }
                }
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    tracing::warn!("ownership cleanup awaits still-owned PTYs; other records were not touched");
}
