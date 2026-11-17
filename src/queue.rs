//! Byte-bounded in-process queues. A queued item's permit stays charged while its consumer
//! handles it, not just until dequeue. Small messages have a floor charge to bound metadata too.

use std::sync::Arc;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

const MIN_CHARGE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError { Full, Closed, Oversized }

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "queue {self:?}") }
}
impl std::error::Error for QueueError {}

pub struct Item<T> {
    pub value: T,
    _permit: OwnedSemaphorePermit,
}

pub struct Sender<T> {
    tx: mpsc::Sender<Item<T>>,
    budget: Arc<Semaphore>,
    cap: usize,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self { Self { tx: self.tx.clone(), budget: self.budget.clone(), cap: self.cap } }
}

pub struct Receiver<T> {
    rx: mpsc::Receiver<Item<T>>,
    budget: Arc<Semaphore>,
}

pub fn channel<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    assert!(cap >= MIN_CHARGE && cap <= u32::MAX as usize);
    let budget = Arc::new(Semaphore::new(cap));
    let (tx, rx) = mpsc::channel(cap / MIN_CHARGE);
    (Sender { tx, budget: budget.clone(), cap }, Receiver { rx, budget })
}

impl<T> Sender<T> {
    fn charge(&self, bytes: usize) -> Result<u32, QueueError> {
        let charge = bytes.max(MIN_CHARGE);
        if charge > self.cap { Err(QueueError::Oversized) } else { Ok(charge as u32) }
    }

    pub fn try_send(&self, value: T, bytes: usize) -> Result<(), QueueError> {
        let charge = self.charge(bytes)?;
        let permit = self.budget.clone().try_acquire_many_owned(charge).map_err(|e| match e {
            tokio::sync::TryAcquireError::Closed => QueueError::Closed,
            tokio::sync::TryAcquireError::NoPermits => QueueError::Full,
        })?;
        self.tx.try_send(Item { value, _permit: permit }).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => QueueError::Full,
            mpsc::error::TrySendError::Closed(_) => QueueError::Closed,
        })
    }

    pub async fn send(&self, value: T, bytes: usize) -> Result<(), QueueError> {
        let charge = self.charge(bytes)?;
        let permit = self.budget.clone().acquire_many_owned(charge).await.map_err(|_| QueueError::Closed)?;
        self.tx.send(Item { value, _permit: permit }).await.map_err(|_| QueueError::Closed)
    }

    pub fn is_closed(&self) -> bool { self.tx.is_closed() }
    pub fn used_bytes(&self) -> usize { self.cap - self.budget.available_permits() }
}

impl<T> Receiver<T> {
    pub async fn recv(&mut self) -> Option<Item<T>> { self.rx.recv().await }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) { self.budget.close(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bytes_remain_charged_until_consumed_and_waiters_wake_on_close() {
        let (tx, mut rx) = channel(128);
        tx.try_send(vec![0; 100], 100).unwrap();
        let item = rx.recv().await.unwrap();
        assert_eq!(tx.used_bytes(), 100);
        assert_eq!(tx.try_send(vec![0; 1], 1), Err(QueueError::Full));
        drop(item);
        tx.try_send(vec![0; 128], 128).unwrap();
        let waiting = tokio::spawn({ let tx = tx.clone(); async move { tx.send(vec![0], 1).await } });
        drop(rx);
        assert_eq!(waiting.await.unwrap(), Err(QueueError::Closed));
        assert_eq!(tx.try_send(vec![0; 129], 129), Err(QueueError::Oversized));
    }
}
