//! Native availability evidence, not a recovery timer or a quota percentage heuristic.
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Availability {
    causes: BTreeMap<&'static str, Value>,
    pub rate_info: Option<Value>,
    pub limit_episode: Option<Value>,
    rate_current: bool,
    reported: bool,
}
impl Availability {
    /// Withhold an old limit freshness witness as soon as newer work may supersede it.
    /// Absence does not claim that a new native turn actually started or completed.
    pub fn invalidate_episode(&mut self) {
        self.limit_episode = None;
    }
    pub fn begin_turn(&mut self) {
        self.limit_episode = None;
        self.rate_info = None;
        self.rate_current = false;
    }
    pub fn observe(&mut self, frame: &Value, active: bool) {
        if frame["type"] == "rate_limit_event" {
            let info = &frame["rate_limit_info"];
            match info["status"].as_str() {
                Some("rejected") => {
                    // No independent no-prompt recovery is established for this snapshot. It
                    // blocks only observed live work; the native result ends that block scope.
                    if active {
                        self.reported = true;
                    }
                    // This native type is documented for claude.ai subscription rate limits.
                    let reason = if info["errorCode"] == "credits_required"
                        || info["overageDisabledReason"] == "out_of_credits"
                    {
                        "quota"
                    } else {
                        "rate_limited"
                    };
                    if active {
                        self.causes.insert("rate_snapshot",json!({"reason":reason,"source":"rate_limit_event","rate_limit_info":info}));
                    }
                    self.rate_info = Some(info.clone());
                    self.rate_current = active;
                }
                Some("allowed" | "allowed_warning") => {
                    self.reported = true;
                    // Only the native current rate-permission snapshot clears rate/credit causes;
                    // it says nothing about an independent authentication or retry failure.
                    self.causes.remove("rate_snapshot");
                    self.causes.remove("rate_error");
                    self.rate_info = Some(info.clone());
                    self.limit_episode = None;
                    self.rate_current = false;
                }
                _ => {}
            }
        }
        if frame["type"] == "assistant" {
            if active {
                if let Some(error) = frame["error"].as_str() {
                    self.native_error(error);
                }
            }
        }
        if frame["type"] == "system" && frame["subtype"] == "api_retry" {
            self.reported = true;
            let mut evidence = json!({"reason":"_retrying","source":"api_retry"});
            for key in ["attempt", "max_retries", "retry_delay_ms", "error_status"] {
                if let Some(v) = frame.get(key) {
                    evidence[key] = v.clone();
                }
            }
            self.causes.insert("retry", evidence);
            if let Some(error) = frame["error"].as_str() {
                self.native_error(error);
            }
        }
        if frame["type"] == "result" {
            self.limit_episode = None;
            self.reported = true;
            // All published blocks are TURN scoped. Native query termination is the independent
            // clear; never wedge mail behind a block that needs another prompt to recover.
            self.causes.clear();
            if self.rate_current
                && frame["terminal_reason"] == "blocking_limit"
                && self
                    .rate_info
                    .as_ref()
                    .is_some_and(|info| info["status"] == "rejected")
            {
                if let (Some(uuid), Some(session)) =
                    (frame["uuid"].as_str(), frame["session_id"].as_str())
                {
                    if uuid.len() <= 128 && session.len() <= 128 {
                        self.limit_episode = Some(json!({"episode":uuid,"session_id":session,
                            "terminal_reason":"blocking_limit","subscription_limit":true}));
                    }
                }
            }
            self.rate_current = false;
        }
    }
    fn native_error(&mut self, error: &str) {
        let (key, reason) = match error {
            "authentication_failed" | "oauth_org_not_allowed" | "cloud_credential_error" => {
                ("auth", "auth")
            }
            "rate_limit" => ("rate_error", "rate_limited"),
            "overloaded" => ("overloaded", "overloaded"),
            "billing_error" | "account_on_hold" | "verification_required" => {
                ("native_error", "_native_error")
            }
            _ => return,
        };
        self.reported = true;
        self.causes.insert(
            key,
            json!({"reason":reason,"source":"native_error","error":error}),
        );
    }
    pub fn value(&self) -> Option<Value> {
        if !self.reported {
            return None;
        }
        let reason = [
            "auth",
            "quota",
            "rate_limited",
            "overloaded",
            "_native_error",
            "_retrying",
        ]
        .into_iter()
        .find(|reason| self.causes.values().any(|cause| cause["reason"] == *reason));
        let mut value = json!({"state":if reason.is_some(){"blocked"}else{"available"},
            "native":{"block_scope":"turn","causes":self.causes.values().collect::<Vec<_>>()}});
        if let Some(reason) = reason {
            value["reason"] = json!(reason);
        }
        if let Some(info) = &self.rate_info {
            value["native"]["rate_limit_info"] = info.clone();
        }
        if let Some(episode) = &self.limit_episode {
            for (key, value2) in episode.as_object().unwrap() {
                value["native"][key] = value2.clone();
            }
        }
        (crate::limits::encoded_bytes(&value) <= 16 * 1024).then_some(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_rate_clear_does_not_clear_auth_and_percent_does_not_block() {
        let mut a = Availability::default();
        a.observe(
            &json!({"type":"rate_limit_event","rate_limit_info":{"utilization":1.0}}),
            false,
        );
        assert!(a.value().is_none());
        a.observe(
            &json!({"type":"assistant","error":"authentication_failed"}),
            true,
        );
        a.observe(
            &json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}),
            true,
        );
        a.observe(
            &json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}),
            true,
        );
        assert_eq!(a.value().unwrap()["reason"], "auth");
    }
    #[test]
    fn automatic_subscription_episode_requires_native_terminal_and_subscription_evidence() {
        let mut a = Availability::default();
        let terminal = json!({"type":"result","uuid":"result1","session_id":"s1","terminal_reason":"blocking_limit"});
        a.observe(&terminal, false);
        assert!(a.limit_episode.is_none());
        a.observe(&json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour"}}),true);
        a.observe(&terminal, false);
        assert_eq!(a.limit_episode.as_ref().unwrap()["episode"], "result1");
        assert_eq!(a.value().unwrap()["state"], "available");
        assert_eq!(a.value().unwrap()["native"]["subscription_limit"], true);
        a.observe(
            &json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning"}}),
            false,
        );
        assert!(a.limit_episode.is_none());
    }
}
