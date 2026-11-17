//! Coalesce background invalidations; interactive UI changes do not wait for this deadline.
use std::time::{Duration,Instant};
const INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
pub(super) struct FrameBudget { last: Option<Instant> }
impl FrameBudget {
    pub fn new() -> Self { Self { last:None } }
    pub fn ready(&self, dirty:bool, interactive:bool, now:Instant) -> bool {
        dirty && (interactive || self.last.is_none_or(|last| now.saturating_duration_since(last)>=INTERVAL))
    }
    pub fn drawn(&mut self, now:Instant) { self.last=Some(now); }
    pub fn deadline(&self, dirty:bool) -> Option<Instant> { if dirty { self.last.map(|last| last+INTERVAL) } else { None } }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_bursts_wait_but_interactive_changes_draw_immediately() {
        let mut frame=FrameBudget::new(); let now=Instant::now();
        assert!(frame.ready(true,false,now)); frame.drawn(now);
        for millis in 0..16 { assert!(!frame.ready(true,false,now+Duration::from_millis(millis))); }
        assert!(frame.ready(true,false,now+Duration::from_millis(17)));
        assert!(frame.ready(true,true,now+Duration::from_millis(1)));
        assert!(!frame.ready(false,true,now+Duration::from_secs(1)));
        assert_eq!(frame.deadline(false),None); assert_eq!(frame.deadline(true),Some(now+INTERVAL));
    }
}
