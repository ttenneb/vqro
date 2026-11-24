//! Optional native included-usage capability, guarded by the existing caps revision.
use super::Endpoint;
use serde_json::{Value, json};
use tokio::time::Instant;

pub(super) struct IncludedUsage {
    deadline: Instant,
    expires_at_ms: u64,
    session: Option<String>,
    epoch: String,
    config_rev: u64,
}

impl Endpoint {
    fn included_usage_current(&self) -> bool {
        self.included_usage.as_ref().is_some_and(|p| {
            Instant::now() < p.deadline
                && p.session == self.session
                && p.epoch == self.epoch
                && p.config_rev == self.config_rev
        })
    }

    pub(super) fn effective_caps_rev(&self) -> u64 {
        self.caps_rev.saturating_add(u64::from(
            self.included_usage.is_some() && !self.included_usage_current(),
        ))
    }

    pub(super) fn invalidate_included_usage(&mut self) {
        let revision = self.effective_caps_rev();
        let live = self.included_usage_current();
        if self.included_usage.take().is_some() {
            self.caps_rev = revision.saturating_add(u64::from(live));
        }
    }

    pub(super) fn bind_included_usage(&mut self, deadline: Instant, expires_at_ms: u64) {
        if Instant::now() >= deadline {
            self.invalidate_included_usage();
            return;
        }
        if self.included_usage_current()
            && self
                .included_usage
                .as_ref()
                .is_some_and(|p| p.deadline == deadline && p.expires_at_ms == expires_at_ms)
        {
            return;
        }
        self.invalidate_included_usage();
        self.caps_rev = self.caps_rev.saturating_add(1);
        self.included_usage = Some(IncludedUsage {
            deadline,
            expires_at_ms,
            session: self.session.clone(),
            epoch: self.epoch.clone(),
            config_rev: self.config_rev,
        });
    }

    pub(super) fn included_usage_fact(&self) -> Option<Value> {
        self.included_usage_current().then(||json!({"caps_rev":self.effective_caps_rev(),"expires_at_ms":self.included_usage.as_ref().unwrap().expires_at_ms,"source":"codex.account/rateLimits"}))
    }
}
