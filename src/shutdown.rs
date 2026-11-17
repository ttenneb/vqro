//! Core lifetime, independent of clients, extensions, backend execution and platform signals.
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase { Running, Requested, Stopped }
#[derive(Clone)]
pub struct Shutdown { phase: watch::Sender<Phase> }
impl Default for Shutdown { fn default() -> Self { Self { phase: watch::channel(Phase::Running).0 } } }
impl Shutdown {
    pub fn is_requested(&self) -> bool { *self.phase.borrow() != Phase::Running }
    pub(crate) fn is_stopped(&self) -> bool { *self.phase.borrow() == Phase::Stopped }
    pub fn request(&self) { self.phase.send_if_modified(|p| { if *p == Phase::Running { *p = Phase::Requested; true } else { false } }); }
    pub async fn requested(&self) { let mut rx = self.phase.subscribe(); let _ = rx.wait_for(|p| *p != Phase::Running).await; }
    pub async fn stopped(&self) { let mut rx = self.phase.subscribe(); let _ = rx.wait_for(|p| *p == Phase::Stopped).await; }
    pub(crate) fn finish(&self) { self.phase.send_replace(Phase::Stopped); }
}
