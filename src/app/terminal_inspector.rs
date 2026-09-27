use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::api::schema::{PaneTelemetryAvailability, PaneTelemetryProcess};
use crate::platform::{ProcessTelemetry, TerminalTelemetrySample};
use crate::terminal::TerminalId;

pub(crate) const TELEMETRY_CACHE_TTL: Duration = Duration::from_millis(750);
const MAX_TELEMETRY_PROCESSES: usize = 32;
const MAX_TELEMETRY_CACHE_ENTRIES: usize = 128;

#[derive(Debug, Clone)]
pub(crate) struct CachedTerminalTelemetry {
    sampled_at: Instant,
    availability: PaneTelemetryAvailability,
    detail: Option<String>,
    process_group_id: Option<u32>,
    truncated: bool,
    processes: Vec<PaneTelemetryProcess>,
}

#[derive(Debug)]
pub(crate) struct TerminalTelemetryView {
    pub(crate) availability: PaneTelemetryAvailability,
    pub(crate) detail: Option<String>,
    pub(crate) cache_age_ms: u64,
    pub(crate) process_group_id: Option<u32>,
    pub(crate) truncated: bool,
    pub(crate) processes: Vec<PaneTelemetryProcess>,
}

impl super::App {
    pub(crate) fn terminal_telemetry(
        &mut self,
        terminal_id: &TerminalId,
        shell_pid: Option<u32>,
        now: Instant,
    ) -> TerminalTelemetryView {
        if let Some(cached) = self.terminal_telemetry_cache.get(terminal_id) {
            if now.saturating_duration_since(cached.sampled_at) < TELEMETRY_CACHE_TTL {
                return cached.view(now);
            }
        }

        let previous = self.terminal_telemetry_cache.get(terminal_id).cloned();
        let sample = shell_pid.map_or(
            TerminalTelemetrySample::Unavailable("terminal process is unavailable"),
            |pid| crate::platform::sample_terminal_telemetry(pid, MAX_TELEMETRY_PROCESSES),
        );
        let cached = CachedTerminalTelemetry::from_sample(sample, previous.as_ref(), now);
        let view = cached.view(now);
        self.terminal_telemetry_cache
            .insert(terminal_id.clone(), cached);
        if self.terminal_telemetry_cache.len() > MAX_TELEMETRY_CACHE_ENTRIES {
            if let Some(oldest) = self
                .terminal_telemetry_cache
                .iter()
                .min_by_key(|(_, cached)| cached.sampled_at)
                .map(|(terminal_id, _)| terminal_id.clone())
            {
                self.terminal_telemetry_cache.remove(&oldest);
            }
        }
        view
    }
}

impl CachedTerminalTelemetry {
    fn from_sample(
        sample: TerminalTelemetrySample,
        previous: Option<&Self>,
        sampled_at: Instant,
    ) -> Self {
        match sample {
            TerminalTelemetrySample::Available {
                process_group_id,
                processes,
                truncated,
            } => {
                let elapsed_ms = previous
                    .map(|cached| {
                        sampled_at
                            .saturating_duration_since(cached.sampled_at)
                            .as_millis()
                    })
                    .and_then(|elapsed| u64::try_from(elapsed).ok())
                    .filter(|elapsed| *elapsed > 0);
                let previous_processes = previous
                    .map(|cached| {
                        cached
                            .processes
                            .iter()
                            .map(|process| (process.pid, process))
                            .collect::<HashMap<_, _>>()
                    })
                    .unwrap_or_default();
                let processes = processes
                    .into_iter()
                    .map(|process| process_view(process, &previous_processes, elapsed_ms))
                    .collect();
                Self {
                    sampled_at,
                    availability: PaneTelemetryAvailability::Available,
                    detail: None,
                    process_group_id: Some(process_group_id),
                    truncated,
                    processes,
                }
            }
            TerminalTelemetrySample::Unavailable(detail) => Self {
                sampled_at,
                availability: PaneTelemetryAvailability::Unavailable,
                detail: Some(detail.to_owned()),
                process_group_id: None,
                truncated: false,
                processes: Vec::new(),
            },
            TerminalTelemetrySample::Unsupported => Self {
                sampled_at,
                availability: PaneTelemetryAvailability::Unsupported,
                detail: Some("process telemetry is currently available on Linux".to_owned()),
                process_group_id: None,
                truncated: false,
                processes: Vec::new(),
            },
        }
    }

    fn view(&self, now: Instant) -> TerminalTelemetryView {
        TerminalTelemetryView {
            availability: self.availability,
            detail: self.detail.clone(),
            cache_age_ms: u64::try_from(now.saturating_duration_since(self.sampled_at).as_millis())
                .unwrap_or(u64::MAX),
            process_group_id: self.process_group_id,
            truncated: self.truncated,
            processes: self.processes.clone(),
        }
    }
}

fn process_view(
    process: ProcessTelemetry,
    previous: &HashMap<u32, &PaneTelemetryProcess>,
    elapsed_ms: Option<u64>,
) -> PaneTelemetryProcess {
    let cpu_tenths_percent = elapsed_ms.and_then(|elapsed_ms| {
        let previous = previous.get(&process.pid)?.cpu_time_ms?;
        let current = process.cpu_time_ms?;
        let elapsed_cpu = current.checked_sub(previous)?;
        u32::try_from(elapsed_cpu.saturating_mul(1_000) / elapsed_ms).ok()
    });
    PaneTelemetryProcess {
        pid: process.pid,
        name: process.name,
        state: process.state,
        cpu_time_ms: process.cpu_time_ms,
        cpu_tenths_percent,
        resident_bytes: process.resident_bytes,
        thread_count: process.thread_count,
        read_bytes: process.read_bytes,
        write_bytes: process.write_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, cpu_time_ms: Option<u64>) -> ProcessTelemetry {
        ProcessTelemetry {
            pid,
            name: "worker".into(),
            state: Some("R".into()),
            cpu_time_ms,
            resident_bytes: Some(0),
            thread_count: Some(1),
            read_bytes: Some(0),
            write_bytes: Some(0),
        }
    }

    #[test]
    fn utilization_requires_two_samples_and_preserves_real_zeroes() {
        let start = Instant::now();
        let first = CachedTerminalTelemetry::from_sample(
            TerminalTelemetrySample::Available {
                process_group_id: 7,
                processes: vec![process(7, Some(100))],
                truncated: false,
            },
            None,
            start,
        );
        assert_eq!(first.processes[0].cpu_tenths_percent, None);
        assert_eq!(first.processes[0].resident_bytes, Some(0));

        let second = CachedTerminalTelemetry::from_sample(
            TerminalTelemetrySample::Available {
                process_group_id: 7,
                processes: vec![process(7, Some(125))],
                truncated: false,
            },
            Some(&first),
            start + Duration::from_millis(250),
        );
        assert_eq!(second.processes[0].cpu_tenths_percent, Some(100));
        assert_eq!(second.processes[0].read_bytes, Some(0));
    }

    #[test]
    fn unavailable_is_distinct_from_an_available_empty_value() {
        let cached = CachedTerminalTelemetry::from_sample(
            TerminalTelemetrySample::Unavailable("gone"),
            None,
            Instant::now(),
        );
        assert_eq!(cached.availability, PaneTelemetryAvailability::Unavailable);
        assert_eq!(cached.detail.as_deref(), Some("gone"));
        assert!(cached.processes.is_empty());
    }
}
