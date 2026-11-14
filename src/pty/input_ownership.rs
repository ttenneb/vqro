//! Transient user-input admission ownership. Never persisted or client-authored.
//! Callers hold the actor's existing input mutex for every transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Open,
    Handoff,
    Cleanup,
    Closed,
}

#[derive(Debug)]
pub(crate) struct InputOwnership {
    owner: Owner,
    epoch: u64,
    fresh: bool,
    attempted: bool,
}

impl InputOwnership {
    pub(crate) fn new(quiesced: bool) -> Self {
        Self {
            owner: if quiesced {
                Owner::Handoff
            } else {
                Owner::Open
            },
            epoch: u64::from(quiesced),
            fresh: false,
            attempted: quiesced,
        }
    }

    // Only the App's positively fresh split path may arm, before publication.
    // Even a failed earlier input attempt or lifecycle change prevents arming.
    pub(crate) fn arm_fresh_split(&mut self) -> bool {
        if self.epoch != 0 || self.attempted || self.owner != Owner::Open {
            return false;
        }
        self.epoch = 1;
        self.fresh = true;
        true
    }

    pub(crate) fn record_input_attempt(&mut self) {
        self.attempted = true;
        self.fresh = false;
    }

    pub(crate) fn invalidate(&mut self) {
        self.record_input_attempt();
    }

    fn advance(&mut self) -> bool {
        let Some(epoch) = self.epoch.checked_add(1) else {
            self.fresh = false;
            self.owner = Owner::Closed;
            return false;
        };
        self.epoch = epoch;
        true
    }

    pub(crate) fn claim_cleanup(&mut self) -> Option<u64> {
        if !self.fresh || self.attempted || self.owner != Owner::Open || !self.advance() {
            return None;
        }
        self.owner = Owner::Cleanup;
        Some(self.epoch)
    }

    pub(crate) fn rollback_cleanup(&mut self, epoch: u64) -> bool {
        if self.owner != Owner::Cleanup || self.epoch != epoch || !self.advance() {
            return false;
        }
        self.owner = Owner::Open;
        true
    }

    #[cfg(unix)]
    pub(crate) fn claim_handoff(&mut self) -> Option<u64> {
        if self.owner != Owner::Open {
            return None;
        }
        self.invalidate();
        if !self.advance() {
            return None;
        }
        self.owner = Owner::Handoff;
        Some(self.epoch)
    }

    #[cfg(unix)]
    pub(crate) fn handoff_epoch(&self) -> Option<u64> {
        (self.owner == Owner::Handoff).then_some(self.epoch)
    }

    #[cfg(unix)]
    pub(crate) fn rollback_handoff(&mut self, epoch: u64) -> bool {
        if self.owner != Owner::Handoff || self.epoch != epoch || !self.advance() {
            return false;
        }
        self.owner = Owner::Open;
        true
    }

    pub(crate) fn shutdown(&mut self) {
        self.invalidate();
        self.advance();
        self.owner = Owner::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_shot_positive_freshness_allows_cleanup() {
        let mut gate = InputOwnership::new(false);
        assert_eq!(gate.claim_cleanup(), None);
        assert!(gate.arm_fresh_split());
        assert!(!gate.arm_fresh_split());
        let epoch = gate.claim_cleanup().unwrap();
        assert_eq!(gate.claim_cleanup(), None);
        assert!(gate.rollback_cleanup(epoch));
        assert!(gate.claim_cleanup().is_some());
        for attempted in [false, true] {
            let mut gate = InputOwnership::new(false);
            if attempted {
                gate.record_input_attempt();
            } else {
                gate.invalidate();
            }
            assert!(!gate.arm_fresh_split());
            assert_eq!(gate.claim_cleanup(), None);
        }
        assert!(!InputOwnership::new(true).arm_fresh_split());
    }

    #[test]
    fn rejected_input_dirties_without_stealing_cleanup_rollback_ownership() {
        let mut gate = InputOwnership::new(false);
        assert!(gate.arm_fresh_split());
        let epoch = gate.claim_cleanup().unwrap();
        gate.record_input_attempt();
        assert!(gate.rollback_cleanup(epoch));
        assert_eq!(gate.claim_cleanup(), None);
    }

    #[test]
    fn stale_cleanup_rollback_cannot_reopen_shutdown_or_new_claim() {
        let mut gate = InputOwnership::new(false);
        assert!(gate.arm_fresh_split());
        let old = gate.claim_cleanup().unwrap();
        assert!(gate.rollback_cleanup(old));
        let current = gate.claim_cleanup().unwrap();
        assert!(!gate.rollback_cleanup(old));
        gate.shutdown();
        assert!(!gate.rollback_cleanup(current));
    }

    #[cfg(unix)]
    #[test]
    fn stale_handoff_rollback_cannot_reopen_cleanup_or_shutdown() {
        let mut gate = InputOwnership::new(false);
        assert!(gate.arm_fresh_split());
        let old = gate.claim_handoff().unwrap();
        assert!(gate.rollback_handoff(old));
        assert_eq!(gate.claim_cleanup(), None);
        let current = gate.claim_handoff().unwrap();
        assert!(!gate.rollback_handoff(old));
        gate.shutdown();
        assert!(!gate.rollback_handoff(current));
        assert!(!gate.arm_fresh_split());
    }
}
