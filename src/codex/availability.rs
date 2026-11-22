//! Native-only availability. No error-text classification, credential handling or input retry.
use super::{Change, Endpoint};
use serde_json::{Value, json};

impl Endpoint {
    pub(super) fn reset_availability(&mut self) {
        self.availability = None;
        self.availability_turn = None;
        self.availability_generation = self.availability_generation.saturating_add(1);
    }

    fn available(&mut self, native: Value) -> bool {
        if self
            .availability
            .as_ref()
            .is_none_or(|a| a["state"] != "blocked")
        {
            return false;
        }
        self.availability = Some(json!({"state":"available","native":native}));
        self.availability_turn = None;
        self.availability_generation = self.availability_generation.saturating_add(1);
        true
    }

    fn reason(&self) -> Option<&str> {
        self.availability.as_ref()?.get("reason")?.as_str()
    }

    pub(super) fn availability_notification(&mut self, method: &str, p: &Value) -> Option<bool> {
        match method {
            "account/rateLimits/updated" => Some(self.usage_permission(p)),
            // Exact native0.170 login completion signal; the adapter never initiates login.
            "account/login/completed" => Some(
                self.reason() == Some("auth")
                    && p["success"] == true
                    && self.available(json!({"method":method,"success":true})),
            ),
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
                // Known short category only: arbitrary error text/data can contain secrets.
                let native = json!({"codexErrorInfo":info.filter(|s| matches!(*s, "usageLimitExceeded" | "rateLimitExceeded" | "unauthorized" | "serverOverloaded" | "internalServerError")),
                    "willRetry":p["willRetry"],"threadId":self.session,"turnId":turn});
                self.availability =
                    Some(json!({"state":"blocked","reason":reason,"native":native}));
                self.availability_turn = Some(turn.into());
                self.availability_generation = self.availability_generation.saturating_add(1);
                Some(true)
            }
            _ => None,
        }
    }

    fn usage_permission(&mut self, p: &Value) -> bool {
        if matches!(self.reason(), Some("quota" | "rate_limited"))
            && p["ordinaryUsageAllowed"] == true
        {
            self.available(json!({"method":"account/rateLimits","ordinaryUsageAllowed":true}))
        } else {
            false
        }
    }

    pub(super) fn availability_turn_completed(&mut self, turn: &Value) {
        let Some(id) = turn["id"].as_str() else {
            return;
        };
        let terminal = matches!(
            turn["status"].as_str(),
            Some("completed" | "failed" | "interrupted")
        );
        let retry_end =
            self.reason() == Some("_retrying") && self.availability_turn.as_deref() == Some(id);
        let success = self.run.as_deref() == Some(id) && turn["status"] == "completed";
        if terminal && (retry_end || success) {
            self.available(json!({"method":"turn/completed","turnId":id,"status":turn["status"]}));
        }
    }

    pub(super) fn availability_read_fence(&self) -> Option<(String, u64)> {
        (self.lifecycle == "live" && matches!(self.reason(), Some("quota" | "rate_limited")))
            .then(|| (self.epoch.clone(), self.availability_generation))
    }

    pub(super) fn finish_availability_read(
        &mut self,
        fence: &(String, u64),
        response: &Value,
    ) -> Vec<Change> {
        if self.availability_read_fence().as_ref() == Some(fence) && self.usage_permission(response)
        {
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
    fn late_permission_never_clears_a_new_block_epoch_or_ended_endpoint() {
        for change in ["block", "epoch", "end"] {
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
