//! Native availability evidence, not a recovery timer or a quota percentage heuristic.
use std::collections::BTreeMap;
use serde_json::{Value, json};
#[derive(Default)]
pub struct Availability { causes: BTreeMap<&'static str, Value>, pub rate_info: Option<Value>, pub limit_episode: Option<Value>, reported: bool }
impl Availability {
    pub fn observe(&mut self, frame: &Value) {
        if frame["type"] == "rate_limit_event" {
            let info = &frame["rate_limit_info"];
            match info["status"].as_str() {
                Some("rejected") => {
                    self.reported = true;
                    // This native type is documented for claude.ai subscription rate limits.
                    let reason = if info["errorCode"] == "credits_required" || info["overageDisabledReason"] == "out_of_credits" { "quota" } else { "rate_limited" };
                    self.causes.insert("rate_snapshot",json!({"reason":reason,"source":"rate_limit_event","rate_limit_info":info}));
                    self.rate_info = Some(info.clone());
                }
                Some("allowed" | "allowed_warning") => {
                    self.reported = true;
                    // Only the native current rate-permission snapshot clears rate/credit causes;
                    // it says nothing about an independent authentication or retry failure.
                    self.causes.remove("rate_snapshot"); self.causes.remove("rate_error");
                    self.rate_info = Some(info.clone()); self.limit_episode = None;
                }
                _ => {}
            }
        }
        if frame["type"] == "assistant" {
            if let Some(error) = frame["error"].as_str() { self.native_error(error); }
        }
        if frame["type"] == "system" && frame["subtype"] == "api_retry" {
            self.reported = true;
            let mut evidence = json!({"reason":"_retrying","source":"api_retry"});
            for key in ["attempt","max_retries","retry_delay_ms","error_status"] {
                if let Some(v) = frame.get(key) { evidence[key] = v.clone(); }
            }
            self.causes.insert("retry",evidence);
            if let Some(error) = frame["error"].as_str() { self.native_error(error); }
        }
        if frame["type"] == "result" {
            self.limit_episode = None;
            // A native terminal result ends this query's retrying state, not rate/auth blocking.
            self.causes.remove("retry");
            if frame["terminal_reason"] == "blocking_limit" && self.rate_info.as_ref().is_some_and(|info| info["status"] == "rejected") {
                if let (Some(uuid),Some(session)) = (frame["uuid"].as_str(),frame["session_id"].as_str()) {
                    if uuid.len()<=128 && session.len()<=128 {
                        self.limit_episode = Some(json!({"episode":uuid,"result_uuid":uuid,"session_id":session,
                            "terminal_reason":"blocking_limit","subscription_limit":true}));
                    }
                }
            }
            // Positive native main-loop output on a normally completed model turn establishes
            // that this process's auth worked. A synthetic/hook-only zero-model turn does not.
            if frame["terminal_reason"] == "completed" && frame["is_error"] == false
                && frame["usage"]["output_tokens"].as_u64().is_some_and(|v|v>0) {
                self.causes.remove("auth"); self.causes.remove("overloaded");
            }
        }
    }
    fn native_error(&mut self, error: &str) {
        let (key, reason) = match error {
            "authentication_failed" | "oauth_org_not_allowed" | "cloud_credential_error" => ("auth","auth"),
            "rate_limit" => ("rate_error","rate_limited"),
            "overloaded" => ("overloaded","overloaded"),
            "billing_error" | "account_on_hold" | "verification_required" => ("native_error","_native_error"),
            _ => return,
        };
        self.reported = true;
        self.causes.insert(key,json!({"reason":reason,"source":"native_error","error":error}));
    }
    pub fn value(&self) -> Option<Value> {
        if !self.reported { return None; }
        let reason = ["auth","quota","rate_limited","overloaded","_native_error","_retrying"].into_iter()
            .find(|reason| self.causes.values().any(|cause| cause["reason"] == *reason));
        let mut value = json!({"state":if reason.is_some(){"blocked"}else{"available"},
            "native":{"causes":self.causes.values().collect::<Vec<_>>()}});
        if let Some(reason) = reason { value["reason"] = json!(reason); }
        if let Some(info) = &self.rate_info { value["native"]["rate_limit_info"] = info.clone(); }
        if let Some(episode) = &self.limit_episode {
            for (key,value2) in episode.as_object().unwrap() { value["native"][key] = value2.clone(); }
        }
        (crate::limits::encoded_bytes(&value) <= 16*1024).then_some(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_rate_clear_does_not_clear_auth_and_percent_does_not_block() {
        let mut a = Availability::default();
        a.observe(&json!({"type":"rate_limit_event","rate_limit_info":{"utilization":1.0}})); assert!(a.value().is_none());
        a.observe(&json!({"type":"assistant","error":"authentication_failed"}));
        a.observe(&json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}));
        a.observe(&json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}));
        assert_eq!(a.value().unwrap()["reason"],"auth");
    }
    #[test]
    fn automatic_subscription_episode_requires_native_terminal_and_subscription_evidence() {
        let mut a = Availability::default();
        let terminal = json!({"type":"result","uuid":"result1","session_id":"s1","terminal_reason":"blocking_limit"});
        a.observe(&terminal); assert!(a.limit_episode.is_none());
        a.observe(&json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour"}}));
        a.observe(&terminal); assert_eq!(a.limit_episode.as_ref().unwrap()["episode"],"result1");
        a.observe(&json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning"}}));
        assert!(a.limit_episode.is_none());
    }
}
