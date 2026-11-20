//! Intrinsic synchronous-work measurements, not end-to-end latency or I/O SLAs.
//! The clock is local to the measuring thread: never carry a sample across an await.
use std::{marker::PhantomData, rc::Rc, time::{Duration, Instant}};

fn thread_cpu() -> Duration {
    let mut value = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes one valid, exclusively borrowed timespec. This clock exists
    // on our Linux/macOS test hosts; syscall failure is an error, never a wall-clock fallback.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut value) };
    assert_eq!(result, 0, "thread CPU clock: {}", std::io::Error::last_os_error());
    Duration::new(value.tv_sec.try_into().unwrap(), value.tv_nsec.try_into().unwrap())
}

pub struct WorkClock {
    cpu: Duration,
    wall: Instant,
    // Prevent moving an in-flight thread measurement to another runtime worker.
    _same_thread: PhantomData<Rc<()>>,
}
#[derive(Clone, Copy, Debug)]
pub struct WorkSample { pub cpu: Duration, pub wall: Duration }
impl WorkClock {
    pub fn start() -> Self {
        Self { cpu: thread_cpu(), wall: Instant::now(), _same_thread: PhantomData }
    }
    pub fn finish(self) -> WorkSample {
        let sample = WorkSample { cpu: thread_cpu().checked_sub(self.cpu).expect("thread CPU clock moved backwards"), wall: self.wall.elapsed() };
        // A hang/blocked filesystem is still a failure. This is not the work-performance oracle.
        assert!(sample.wall < Duration::from_secs(30), "work wall-clock backstop exceeded: {sample:?}");
        sample
    }
}
impl WorkSample {
    pub fn assert_cpu_budget(self, limit: Duration, label: &str) {
        assert!(self.cpu < limit, "CPU work budget exceeded for {label}: {:?} >= {limit:?} (wall {:?})", self.cpu, self.wall);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descheduled_time_is_not_work() {
        let clock = WorkClock::start();
        std::thread::sleep(Duration::from_millis(60));
        let sample = clock.finish();
        assert!(sample.wall >= Duration::from_millis(60));
        // Relational, not another tiny absolute timing guard under machine load.
        assert!(sample.cpu < sample.wall / 2, "sleep was charged as CPU work: {sample:?}");
    }
    #[test]
    #[should_panic(expected="work wall-clock backstop exceeded")]
    fn a_blocked_path_still_has_a_wall_backstop() {
        let mut clock = WorkClock::start();
        clock.wall -= Duration::from_secs(31);
        clock.finish();
    }
    #[test]
    #[should_panic(expected="CPU work budget exceeded")]
    fn deliberately_slow_cpu_work_is_rejected() {
        let clock = WorkClock::start();
        let start = thread_cpu();
        while thread_cpu() - start < Duration::from_millis(12) { std::hint::black_box(7_u64.wrapping_mul(37)); }
        clock.finish().assert_cpu_budget(Duration::from_millis(5), "negative control");
    }
}
