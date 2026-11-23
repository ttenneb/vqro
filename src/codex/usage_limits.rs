//! Read-only, bounded native account windows. These facts never change availability.
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ROWS: usize = 32;
const MAX_ID_BYTES: usize = 256;

#[derive(PartialEq)]
enum Representation {
    Map,
    Legacy(String),
}

#[derive(Default)]
pub(super) struct UsageLimits {
    account: Option<String>,
    representation: Option<Representation>,
    rows: BTreeMap<(String, String), Value>,
}

fn identifier(v: &Value) -> Option<&str> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= MAX_ID_BYTES && !s.chars().any(char::is_control))
}

impl UsageLimits {
    pub(super) fn facts(&self) -> Option<Value> {
        (!self.rows.is_empty()).then(|| Value::Array(self.rows.values().cloned().collect()))
    }

    pub(super) fn observe(&mut self, p: &Value) -> bool {
        let before = self.facts();
        if let Some(account) = identifier(&p["accountId"]) {
            if self.account.as_deref() != Some(account) {
                self.rows.clear();
                self.representation = None;
            }
            self.account = Some(account.to_owned());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let now = u64::try_from(now).unwrap_or(u64::MAX);
        // Representations are alternatives, not independent pools. Never guess which named
        // bucket an anonymous mirror belonged to. Retire it on a map/named-legacy transition.
        // An explicitly supplied empty map still supersedes the legacy mirror.
        if let Some(buckets) = p["rateLimitsByLimitId"].as_object() {
            self.select_representation(Representation::Map);
            for (key, snapshot) in buckets.iter().take(MAX_ROWS) {
                if let Some(bucket) = identifier(&Value::String(key.clone())) {
                    self.bucket(bucket, snapshot, now);
                }
            }
        } else if ["primary", "secondary"].iter().any(|window| {
            p["rateLimits"][window]["usedPercent"]
                .as_u64()
                .is_some_and(|n| n <= 100)
        }) {
            let bucket = identifier(&p["rateLimits"]["limitId"]).unwrap_or("");
            self.select_representation(Representation::Legacy(bucket.to_owned()));
            self.bucket(bucket, &p["rateLimits"], now);
        }
        self.facts() != before
    }

    fn select_representation(&mut self, representation: Representation) {
        if self.representation.as_ref() != Some(&representation) {
            self.rows.clear();
            self.representation = Some(representation);
        }
    }

    fn bucket(&mut self, bucket: &str, snapshot: &Value, now: u64) {
        for window in ["primary", "secondary"] {
            let native = &snapshot[window];
            // Native windows require usedPercent. Ignore malformed windows, not unknowns-as-zero.
            let Some(percent) = native["usedPercent"].as_u64().filter(|n| *n <= 100) else {
                continue;
            };
            let key = (bucket.to_owned(), window.to_owned());
            if self.rows.len() >= MAX_ROWS && !self.rows.contains_key(&key) {
                continue;
            }
            let row = self.rows.entry(key).or_insert_with(|| {
                json!({
                    "scope":{"kind":"account"}, "window":{"id":window},
                    "source":"codex.account/rateLimits"
                })
            });
            if let Some(account) = &self.account {
                row["scope"]["native_account_id"] = json!(account);
            }
            if !bucket.is_empty() {
                row["scope"]["bucket"] = json!(bucket);
            }
            row["used_percent"] = json!(percent);
            row["observed_at_ms"] = json!(now);
            if let Some(ms) = native["resetsAt"]
                .as_u64()
                .and_then(|n| n.checked_mul(1000))
            {
                row["resets_at_ms"] = json!(ms);
            }
            if let Some(ms) = native["windowDurationMins"]
                .as_u64()
                .and_then(|n| n.checked_mul(60_000))
            {
                row["window"]["duration_ms"] = json!(ms);
            }
        }
    }
}
