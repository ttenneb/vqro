//! Byte/count admission for retained native resource facts, including unpublished IO-recovery facts.
use std::collections::BTreeMap;
use crate::adapter::ResourceFacts;
use crate::rpc::RpcError;
use serde_json::json;

pub const MAX_FACT_BYTES: usize = 128 * 1024;
pub const MAX_RESOURCES: usize = 4096;
pub const MAX_RESOURCE_BYTES: usize = 32 * 1024 * 1024;

pub fn encoded_bytes(value: &impl serde::Serialize) -> usize {
    #[derive(Default)] struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.0 = self.0.saturating_add(b.len()); Ok(b.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut count = Counter::default();
    if serde_json::to_writer(&mut count, value).is_err() { usize::MAX } else { count.0 }
}

pub fn charge(provider: &str, instance: &str, host: Option<&str>, facts: &ResourceFacts) -> usize {
    let mut value = serde_json::to_value(facts).unwrap_or(serde_json::Value::Null);
    value["id"] = json!("r18446744073709551615");
    value["provider"] = json!(provider);
    value["adapter"] = json!(provider);
    value["instance"] = json!(instance);
    // false is the longer representation, so availability transitions never exceed this charge.
    value["available"] = json!(false); value["stale"] = json!(false);
    value["host"] = json!(host);
    encoded_bytes(&value)
}

#[derive(Default,Clone)]
pub struct Admission {
    resources: BTreeMap<String, (String, String, usize)>,
    pending: BTreeMap<(String, String), usize>,
    references: BTreeMap<(String, String), usize>,
    counts: BTreeMap<String, usize>,
    bytes: usize,
}
impl Admission {
    pub fn has_provider(&self,name:&str)->bool{self.counts.get(name).copied().unwrap_or(0)>0}
    pub fn check(&self, provider: &str, reference: &str, facts: &ResourceFacts, bytes: usize) -> Result<(), RpcError> {
        let key = (provider.to_string(), reference.to_string());
        if encoded_bytes(facts) > MAX_FACT_BYTES { return Err(crate::provider::rejected("overloaded", format!("provider {provider}: whole facts record exceeds 128 KiB"))); }
        if !self.references.contains_key(&key) && self.counts.get(provider).copied().unwrap_or(0) >= MAX_RESOURCES {
            return Err(crate::provider::rejected("overloaded", format!("provider {provider}: resource count exceeds 4096")));
        }
        // Reserve the candidate in addition to current facts until its durable publication. A
        // failed write may need both the stale published record and its pending replacement.
        let projected = self.bytes.saturating_sub(self.pending.get(&key).copied().unwrap_or(0)).saturating_add(bytes);
        if projected > MAX_RESOURCE_BYTES { return Err(crate::provider::rejected("overloaded", format!("provider {provider}: retained resource facts exceed 32 MiB"))); }
        Ok(())
    }
    fn add_reference(&mut self, key: &(String, String)) {
        let count = self.references.entry(key.clone()).or_default();
        if *count == 0 { *self.counts.entry(key.0.clone()).or_default() += 1; }
        *count += 1;
    }
    fn remove_reference(&mut self, key: &(String, String)) {
        if let Some(count) = self.references.get_mut(key) {
            *count -= 1;
            if *count == 0 {
                self.references.remove(key);
                if let Some(count) = self.counts.get_mut(&key.0) {
                    *count -= 1;
                    if *count == 0 {self.counts.remove(&key.0);}
                }
            }
        }
    }
    pub fn resource(&mut self, id: &str, provider: &str, reference: &str, bytes: usize) {
        self.remove_resource(id);
        let key = (provider.into(), reference.into());
        self.add_reference(&key);
        self.bytes += bytes;
        self.resources.insert(id.into(), (key.0, key.1, bytes));
    }
    pub fn remove_resource(&mut self, id: &str) {
        if let Some((provider, reference, bytes)) = self.resources.remove(id) {
            self.bytes -= bytes; self.remove_reference(&(provider, reference));
        }
    }
    pub fn pending(&mut self, provider: &str, reference: &str, bytes: usize) {
        self.remove_pending(provider, reference);
        let key = (provider.into(), reference.into());
        self.add_reference(&key); self.bytes += bytes; self.pending.insert(key, bytes);
    }
    pub fn remove_pending(&mut self, provider: &str, reference: &str) {
        let key = (provider.into(), reference.into());
        if let Some(bytes) = self.pending.remove(&key) { self.bytes -= bytes; self.remove_reference(&key); }
    }
    pub fn clear_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for (key, bytes) in pending { self.bytes -= bytes; self.remove_reference(&key); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn facts(reference: &str) -> ResourceFacts {
        serde_json::from_value(json!({"backend_ref":reference,"kind":"terminal","state":"live","name":null,"title":null,"argv":[],"data":{}})).unwrap()
    }
    #[test]
    fn counts_whole_records_global_bytes_and_pending_are_bounded_and_reusable() {
        let mut admission = Admission::default();
        for i in 0..MAX_RESOURCES {
            let reference = format!("f{i}"); let f = facts(&reference);
            admission.check("p", &reference, &f, 128).unwrap();
            admission.resource(&format!("r{i}"), "p", &reference, 128);
        }
        assert_eq!(admission.check("p", "extra", &facts("extra"), 128).unwrap_err().kind(), "overloaded");
        admission.remove_resource("r0");
        admission.check("p", "extra", &facts("extra"), 128).unwrap();
        let mut giant = facts("giant"); giant.status = json!({"payload":"x".repeat(MAX_FACT_BYTES)});
        assert_eq!(admission.check("other", "giant", &giant, 1).unwrap_err().kind(), "overloaded");
        let mut bytes = Admission::default();
        bytes.pending("p", "pending", MAX_RESOURCE_BYTES - 100);
        let error = bytes.check("offender", "next", &facts("next"), 200).unwrap_err();
        assert!(error.message.contains("offender"));
        bytes.clear_pending();
        bytes.check("offender", "next", &facts("next"), 200).unwrap();
        bytes.pending("p", "same", 123); bytes.resource("r1", "p", "same", 456);
        bytes.remove_pending("p", "same"); bytes.remove_resource("r1");
        assert_eq!(bytes.bytes, 0);
        assert!(bytes.references.is_empty());
        assert!(bytes.counts.is_empty(),"retired provider names must not accumulate in the admission index");
    }
}
