//! Opt-in bounded local counters for the private TUI probe; no protocol or UI policy.
use std::{collections::VecDeque, path::PathBuf, time::{Duration, Instant}};
use serde_json::json;

pub(super) struct Probe { path: Option<PathBuf>, start: Instant, frames: u64, stream_bytes: u64, draw: Duration, max: Duration, buckets: VecDeque<(u64,u64,u64,u64)>, closures: VecDeque<serde_json::Value> }
impl Probe {
    pub fn from_env() -> Self { Self { path: std::env::var_os("VQRO_TUI_PROBE").map(PathBuf::from), start: Instant::now(), frames:0, stream_bytes:0, draw:Duration::ZERO, max:Duration::ZERO, buckets:VecDeque::new(), closures:VecDeque::new() } }
    pub fn incoming(&mut self, message: &crate::rpc::Incoming) {
        if self.path.is_none() { return; }
        if let crate::rpc::Incoming::Notification { method, params } = message {
            // Why streams ended (the last 16), for diagnosing reattaches under load.
            if method == "stream.closed" || method == "stream.gap" {
                self.closures.push_back(json!({"at_s": self.start.elapsed().as_secs_f64(), "method": method, "stream": params["stream"], "reason": params["reason"]}));
                while self.closures.len() > 16 { self.closures.pop_front(); }
                self.write();
            }
            if method == "stream.data" {
                if let Some(data) = params["data"].as_str() {
                    let padding = data.as_bytes().iter().rev().take_while(|b| **b == b'=').count();
                    self.stream_bytes = self.stream_bytes.saturating_add((data.len() / 4 * 3).saturating_sub(padding) as u64);
                }
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
    fn write(&self) {
        if let Some(path)=&self.path {
            let report=json!({"kind":"vqro.tui-probe","version":1,"elapsed_s":self.start.elapsed().as_secs_f64(),"frames":self.frames,"received_stream_bytes":self.stream_bytes,
                "draw_total_ms":self.draw.as_secs_f64()*1000.,"draw_max_ms":self.max.as_secs_f64()*1000.,"bucket_columns":["second","total","background","interactive"],"frames_by_second":self.buckets,"stream_closures":self.closures});
            if let Err(error)=std::fs::write(path,format!("{report}\n")) { eprintln!("vqro: TUI probe report: {error}"); }
        }
    }
}
impl Drop for Probe { fn drop(&mut self) { self.write(); } }
