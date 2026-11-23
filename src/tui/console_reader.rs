//! A bounded-poll console reader whose ownership can be handed to a replacement process.
use std::{io, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread::{self, JoinHandle}, time::Duration};

pub(super) struct ConsoleReader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ConsoleReader {
    pub(super) fn start(mut next: impl FnMut(Duration) -> io::Result<()> + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                if next(Duration::from_millis(50)).is_err() { break; }
            }
        });
        Self { stop, thread: Some(thread) }
    }

    fn stop(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| io::Error::other("console input reader panicked; replacement not started"))?;
        }
        Ok(())
    }

    pub(super) fn stop_then<T>(&mut self, spawn: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.stop()?;
        spawn()
    }
}

impl Drop for ConsoleReader {
    fn drop(&mut self) { let _ = self.stop(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, mpsc};

    #[test]
    fn reader_is_stopped_and_joined_before_spawn() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let reader_order = order.clone();
        let mut reader = ConsoleReader::start(move |_| {
            reader_order.lock().unwrap().push("reading");
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            reader_order.lock().unwrap().push("reader finished");
            Ok(())
        });
        entered_rx.recv().unwrap();
        // Release only once cancellation is visible, so a detached/not-joined implementation fails.
        let stopping = reader.stop.clone();
        let release = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) { thread::yield_now(); }
            release_tx.send(()).unwrap();
        });
        reader.stop_then(|| {
            assert!(reader_order_is_finished(&order));
            order.lock().unwrap().push("spawn");
            Ok(())
        }).unwrap();
        release.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["reading", "reader finished", "spawn"]);
    }

    fn reader_order_is_finished(order: &Mutex<Vec<&str>>) -> bool {
        order.lock().unwrap().last() == Some(&"reader finished")
    }

    #[test]
    fn failed_reader_join_refuses_to_spawn() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let mut reader = ConsoleReader::start(move |_| {
            entered_tx.send(()).unwrap();
            panic!("fake console failure");
        });
        entered_rx.recv().unwrap();
        let mut spawned = false;
        assert!(reader.stop_then(|| { spawned = true; Ok(()) }).is_err());
        assert!(!spawned);
    }
}
