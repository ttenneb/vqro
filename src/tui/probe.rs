//! Opt-in bounded local counters for the private TUI probe; no protocol or UI policy.
use std::{collections::VecDeque, path::PathBuf, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use serde_json::{Value, json};

const STREAM_EVENT_CAP: usize = 256;

pub(super) struct Probe {
    path: Option<PathBuf>, start: Instant, started_unix_s: f64,
    frames: u64, stream_bytes: u64, draw: Duration, max: Duration,
    buckets: VecDeque<(u64,u64,u64,u64)>, events: VecDeque<Value>, events_dropped: u64,
}
impl Probe {
    pub fn from_env() -> Self { Self::new(std::env::var_os("VQRO_TUI_PROBE").map(PathBuf::from)) }
    fn new(path: Option<PathBuf>) -> Self {
        // The cross-process wall-clock origin is diagnostic only; latency uses monotonic clocks.
        let started_unix_s = if path.is_some() { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64() } else { 0. };
        Self { path, start: Instant::now(), started_unix_s, frames:0, stream_bytes:0, draw:Duration::ZERO,
            max:Duration::ZERO, buckets:VecDeque::new(), events:VecDeque::new(), events_dropped:0 }
    }
    fn record(&mut self, mut event: Value) {
        event["at_s"] = json!(self.start.elapsed().as_secs_f64());
        if self.events.len() == STREAM_EVENT_CAP { self.events.pop_front(); self.events_dropped += 1; }
        self.events.push_back(event);
    }
    pub fn message(&mut self, message: &super::Msg, connection: u64, connected: bool) {
        if self.path.is_none() { return; }
        if let super::Msg::Attached { connection: source, resource, result: Ok(stream), .. } = message {
            if connected && *source == connection {
                self.record(json!({"method":"resource.attach", "connection":connection, "stream":stream, "resource":resource}));
            }
        }
    }
    pub fn incoming(&mut self, message: &crate::rpc::Incoming, connection: u64) {
        if self.path.is_none() { return; }
        if let crate::rpc::Incoming::Notification { method, params } = message {
            match method.as_str() {
                "stream.closed" | "stream.gap" => {
                    let reason = params["reason"]["reason"].as_str().or_else(|| params["reason"].as_str()).unwrap_or("closed");
                    self.record(json!({"method":method, "connection":connection, "stream":params["stream"], "reason":{"reason":reason}}));
                    self.write();
                }
                "stream.error" => {
                    let kind = params["error"]["data"]["kind"].as_str().unwrap_or("error");
                    self.record(json!({"method":method, "connection":connection, "stream":params["stream"], "kind":kind}));
                    self.write();
                }
                "stream.data" => {
                    if let Some(data) = params["data"].as_str() {
                        let padding = data.as_bytes().iter().rev().take_while(|b| **b == b'=').count();
                        self.stream_bytes = self.stream_bytes.saturating_add((data.len() / 4 * 3).saturating_sub(padding) as u64);
                    }
                }
                _ => {}
            }
        }
    }
    pub fn draw<T>(&mut self, interactive: bool, draw: impl FnOnce() -> T) -> T {
        if self.path.is_none() { return draw(); }
        let start=Instant::now(); let result=draw(); let elapsed=start.elapsed();
        self.frames+=1; self.draw+=elapsed; self.max=self.max.max(elapsed);
        let second=self.start.elapsed().as_secs();
        let new_second=self.buckets.back().is_none_or(|b| b.0!=second);
        if new_second { self.buckets.push_back((second,0,0,0)); }
        let bucket=self.buckets.back_mut().unwrap();
        bucket.1+=1;
        if interactive { bucket.3+=1; } else { bucket.2+=1; }
        while self.buckets.len()>64 { self.buckets.pop_front(); }
        if new_second { self.write(); }
        result
    }
    fn report(&self) -> Value {
        let events = |method: &str| self.events.iter().filter(|e| e["method"] == method).collect::<Vec<_>>();
        let closures = self.events.iter().filter(|e| matches!(e["method"].as_str(), Some("stream.closed" | "stream.gap"))).collect::<Vec<_>>();
        json!({"kind":"vqro.tui-probe","version":1,"started_unix_s":self.started_unix_s,
            "elapsed_s":self.start.elapsed().as_secs_f64(),"frames":self.frames,"received_stream_bytes":self.stream_bytes,
            "draw_total_ms":self.draw.as_secs_f64()*1000.,"draw_max_ms":self.max.as_secs_f64()*1000.,
            "bucket_columns":["second","total","background","interactive"],"frames_by_second":self.buckets,
            "stream_closures":closures,"stream_attachments":events("resource.attach"),"stream_errors":events("stream.error"),
            "stream_events_cap":STREAM_EVENT_CAP,"stream_events_dropped":self.events_dropped})
    }
    fn write(&self) {
        if let Some(path)=&self.path {
            if let Err(error)=std::fs::write(path,format!("{}\n", self.report())) { eprintln!("vqro: TUI probe report: {error}"); }
        }
    }
}
impl Drop for Probe { fn drop(&mut self) { self.write(); } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::Incoming;

    fn attached(connection: u64) -> super::super::Msg {
        super::super::Msg::Attached { connection, view:"idle".into(), resource:"r1".into(), result:Ok(7) }
    }

    #[test]
    fn records_connection_scoped_metadata_without_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut probe = Probe::new(Some(dir.path().join("metrics.json")));
        probe.message(&attached(2), 2, true);
        probe.message(&attached(1), 2, true); // Stale attach results are never adopted.
        probe.message(&attached(2), 2, false);
        probe.incoming(&Incoming::Notification { method:"stream.error".into(), params:json!({"stream":7,"error":{"message":"private text","data":{"kind":"not_found"}}}) }, 2);
        probe.incoming(&Incoming::Notification { method:"stream.closed".into(), params:json!({"stream":7,"reason":{"reason":"slow_consumer","private":"text"}}) }, 2);
        let report = probe.report();
        assert_eq!(report["stream_attachments"].as_array().unwrap().len(), 1);
        assert_eq!(report["stream_attachments"][0]["resource"], "r1");
        assert_eq!(report["stream_errors"][0]["connection"], 2);
        assert_eq!(report["stream_errors"][0]["stream"], 7);
        assert_eq!(report["stream_errors"][0]["kind"], "not_found");
        assert_eq!(report["stream_closures"][0]["reason"], json!({"reason":"slow_consumer"}));
        assert!(report["stream_attachments"][0]["at_s"].as_f64().unwrap() >= 0.);
        assert!(!report.to_string().contains("private"));
    }

    #[test]
    fn records_are_bounded_and_eviction_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let mut probe = Probe::new(Some(dir.path().join("metrics.json")));
        for _ in 0..STREAM_EVENT_CAP + 3 { probe.message(&attached(0), 0, true); }
        assert_eq!(probe.events.len(), STREAM_EVENT_CAP);
        assert_eq!(probe.events_dropped, 3);
        assert_eq!(probe.report()["stream_events_dropped"], 3);
    }

    #[test]
    fn disabled_probe_does_not_record_or_count() {
        let mut probe = Probe::new(None);
        probe.message(&attached(0), 0, true);
        probe.incoming(&Incoming::Notification { method:"stream.data".into(), params:json!({"stream":7,"data":"YWJj"}) }, 0);
        probe.incoming(&Incoming::Notification { method:"stream.error".into(), params:json!({"stream":7}) }, 0);
        probe.draw(false, || ());
        assert_eq!(probe.events.capacity(), 0);
        assert_eq!(probe.buckets.capacity(), 0);
        assert_eq!((probe.stream_bytes, probe.frames, probe.events_dropped), (0, 0, 0));
    }
}
