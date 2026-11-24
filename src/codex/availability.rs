//! Native-only availability. No error-text classification, credentials or input retry.
use super::{Change, Endpoint};
use serde_json::{Value, json};

impl Endpoint {
    pub(super) fn reset_availability(&mut self) {
        self.usage_limits = super::usage_limits::UsageLimits::default();
        self.availability = None;
        self.availability_blocks.clear();
        self.availability_retry = None;
        self.availability_generation = self.availability_generation.saturating_add(1);
    }

    pub(super) fn invalidate_availability_auth_scope(&mut self) {
        // An auth notification is not quota recovery. Retire the old account's known
        // turn causes and require a new native turn before accepting a new episode.
        if let Some(session) = self.session.clone() {
            let turns = self
                .run
                .clone()
                .into_iter()
                .chain(
                    self.completion
                        .as_ref()
                        .and_then(|v| v["id"].as_str())
                        .map(str::to_owned),
                )
                .chain(
                    self.availability_blocks
                        .iter()
                        .chain(self.availability_retry.iter())
                        .filter_map(|v| v["native"]["turnId"].as_str().map(str::to_owned)),
                );
            for turn in turns {
                let key = (session.clone(), turn);
                if !self.availability_retired_turns.contains(&key)
                    && self.availability_retired_turns.len() < 256
                {
                    self.availability_retired_turns.push(key);
                }
            }
        }
        self.availability_auth_fenced = true;
        self.reset_availability(); // Unknown, never a fabricated available/recovered level.
    }

    // At most three persistent native classes plus one transient native retry. A clear for one
    // cause must not invent recovery for another. The public fact shows retry, else latest cause.
    fn refresh_availability(&mut self, native_clear: Value) -> bool {
        self.availability = self
            .availability_retry
            .clone()
            .or_else(|| self.availability_blocks.last().cloned())
            .or_else(|| {
                self.availability
                    .as_ref()
                    .map(|_| json!({"state":"available","native":native_clear}))
            });
        self.availability_generation = self.availability_generation.saturating_add(1);
        true
    }

    fn clear_availability_causes(&mut self, reasons: &[&str], native: Value) -> bool {
        let before = self.availability_blocks.len();
        self.availability_blocks
            .retain(|a| !reasons.contains(&a["reason"].as_str().unwrap_or("")));
        before != self.availability_blocks.len() && self.refresh_availability(native)
    }

    pub(super) fn availability_notification(&mut self, method: &str, p: &Value) -> Option<bool> {
        match method {
            "account/rateLimits/updated" => {
                // Even false/null/partial newer metadata fences an older in-flight true read.
                self.availability_generation = self.availability_generation.saturating_add(1);
                let limits_changed = self.usage_limits.observe(p);
                Some(self.usage_permission(p) || limits_changed)
            }
            // Exact native0.170 signal; no adapter login/credential flow is introduced.
            "account/login/completed" => Some(
                p["success"] == true
                    && self.clear_availability_causes(
                        &["auth"],
                        json!({"method":method,"success":true}),
                    ),
            ),
            "turn/started" => {
                if self.session.is_some() && p["threadId"].as_str() == self.session.as_deref() {
                    if let Some(turn) = p["turn"]["id"].as_str().filter(|s| {
                        !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                    }) {
                        if self
                            .availability_retired_turns
                            .contains(&(self.session.clone().unwrap(), turn.to_owned()))
                        {
                            return Some(false);
                        }
                        if self.availability_retired_turns.len() < 256 {
                            self.availability_auth_fenced = false;
                        }
                    }
                }
                None
            }
            "error" => {
                if self.session.is_none() || p["threadId"].as_str() != self.session.as_deref() {
                    return Some(false);
                }
                let Some(turn) = p["turnId"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 128)
                else {
                    return Some(false);
                };
                if self.availability_auth_fenced
                    || self
                        .availability_retired_turns
                        .contains(&(self.session.clone().unwrap(), turn.to_owned()))
                {
                    return Some(false);
                }
                if self.run.as_deref().is_some_and(|run| run != turn)
                    || self.completion.as_ref().is_some_and(|c| c["id"] == turn)
                {
                    return Some(false);
                }
                let info = p["error"]["codexErrorInfo"].as_str();
                let reason = match (p["willRetry"].as_bool(), info) {
                    (Some(true), _) => "_retrying",
                    (Some(false), Some("usageLimitExceeded")) => "quota",
                    (Some(false), Some("rateLimitExceeded")) => "rate_limited",
                    (Some(false), Some("unauthorized")) => "auth",
                    _ => return Some(false),
                };
                // Known categories only: arbitrary error text/data can contain secrets.
                let native = json!({"codexErrorInfo":info.filter(|s| matches!(*s,
                    "usageLimitExceeded" | "rateLimitExceeded" | "unauthorized" | "serverOverloaded" | "internalServerError")),
                    "willRetry":p["willRetry"],"threadId":self.session,"turnId":turn});
                let fact = json!({"state":"blocked","reason":reason,"native":native});
                if reason == "_retrying" {
                    self.availability_retry = Some(fact);
                } else {
                    self.availability_blocks.retain(|a| a["reason"] != reason);
                    self.availability_blocks.push(fact);
                }
                Some(self.refresh_availability(Value::Null))
            }
            _ => None,
        }
    }

    fn usage_permission(&mut self, p: &Value) -> bool {
        p["ordinaryUsageAllowed"] == true
            && self.clear_availability_causes(
                &["quota", "rate_limited"],
                json!({"method":"account/rateLimits","ordinaryUsageAllowed":true}),
            )
    }

    pub(super) fn availability_turn_completed(&mut self, turn: &Value) {
        let Some(id) = turn["id"].as_str() else {
            return;
        };
        let terminal = matches!(
            turn["status"].as_str(),
            Some("completed" | "failed" | "interrupted")
        );
        let retry_end = self
            .availability_retry
            .as_ref()
            .is_some_and(|a| a["native"]["turnId"] == id);
        let success = self.run.as_deref() == Some(id) && turn["status"] == "completed";
        if terminal && (retry_end || success) {
            if success {
                self.availability_blocks.clear();
            }
            self.availability_retry = None;
            self.refresh_availability(
                json!({"method":"turn/completed","turnId":id,"status":turn["status"]}),
            );
        }
    }

    pub(super) fn availability_read_fence(&self) -> Option<(String, u64)> {
        (self.lifecycle == "live"
            && self
                .availability_blocks
                .iter()
                .any(|a| matches!(a["reason"].as_str(), Some("quota" | "rate_limited"))))
        .then(|| (self.epoch.clone(), self.availability_generation))
    }

    pub(super) fn finish_availability_read(
        &mut self,
        fence: &(String, u64),
        response: &Value,
    ) -> Vec<Change> {
        if self.availability_read_fence().as_ref() != Some(fence) {
            return Vec::new();
        }
        let limits_changed = self.usage_limits.observe(response);
        if self.usage_permission(response) || limits_changed {
            vec![self.state_entity()]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_read_publishes_limits_without_inventing_recovery() {
        let mut e = Endpoint::new("e".into(), "inc".into(), "host".into(), "epoch".into());
        e.set_session(
            &json!({"id":"s1","status":{"type":"idle"}}),
            "epoch1".into(),
        )
        .unwrap();
        e.notification(
            "error",
            &json!({"threadId":"s1","turnId":"t1","willRetry":false,
            "error":{"codexErrorInfo":"usageLimitExceeded"}}),
        );
        let fence = e.availability_read_fence().unwrap();
        let response =
            json!({"ordinaryUsageAllowed":null,"rateLimits":{"primary":{"usedPercent":0}}});
        assert!(!e.finish_availability_read(&fence, &response).is_empty());
        assert_eq!(
            e.facts()["data"]["agent"]["usage_limits"][0]["used_percent"],
            0
        );
        assert_eq!(
            e.facts()["data"]["agent"]["availability"]["state"],
            "blocked"
        );
        let stale = e.availability_read_fence().unwrap();
        e.notification(
            "account/rateLimits/updated",
            &json!({"ordinaryUsageAllowed":null}),
        );
        assert!(
            e.finish_availability_read(
                &stale,
                &json!({"rateLimits":{"primary":{"usedPercent":90}}})
            )
            .is_empty()
        );
        assert_eq!(
            e.facts()["data"]["agent"]["usage_limits"][0]["used_percent"],
            0
        );
    }

    #[test]
    fn late_permission_never_clears_a_new_block_epoch_or_ended_endpoint() {
        for change in ["block", "epoch", "end", "null_permission"] {
            let mut e = Endpoint::new("e".into(), "inc".into(), "host".into(), "epoch".into());
            e.set_session(
                &json!({"id":"s1","status":{"type":"idle"}}),
                "epoch1".into(),
            )
            .unwrap();
            let error = json!({"threadId":"s1","turnId":"t1","willRetry":false,"error":{"codexErrorInfo":"usageLimitExceeded"}});
            e.notification("error", &error);
            let old = e.availability_read_fence().unwrap();
            match change {
                "block" => {
                    e.notification("error", &error);
                }
                "epoch" => {
                    e.set_session(
                        &json!({"id":"s1","status":{"type":"idle"}}),
                        "epoch2".into(),
                    )
                    .unwrap();
                }
                "null_permission" => {
                    e.notification(
                        "account/rateLimits/updated",
                        &json!({"ordinaryUsageAllowed":null}),
                    );
                }
                _ => {
                    e.host_exited("native EOF");
                }
            }
            let before = e.facts();
            assert!(
                e.finish_availability_read(&old, &json!({"ordinaryUsageAllowed":true}))
                    .is_empty()
            );
            assert_eq!(e.facts(), before);
        }
    }
}
