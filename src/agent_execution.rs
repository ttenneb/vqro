//! Explicit API-only, bounded execution observation; no render/detection/fanout work.
use crate::api::schema::{AgentInfo, ResponseResult, SuccessResponse};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static READS: AtomicUsize = AtomicUsize::new(0);
struct ReadSlot;
impl ReadSlot {
    fn acquire() -> Option<Self> {
        READS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 8).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for ReadSlot {
    fn drop(&mut self) {
        READS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Probe {
    index: usize,
    terminal_id: String,
    root_pid: u32,
    root_birth: Option<u64>,
    known_agent: Option<crate::detect::Agent>,
    foreground: Box<dyn Fn() -> Option<u32> + Send>,
}
impl Probe {
    fn observe(&self, deadline: Instant) -> Option<String> {
        if Instant::now() >= deadline || self.root_birth.is_none() || self.known_agent.is_none() {
            return None;
        }
        let foreground = (self.foreground)()?;
        if Instant::now() >= deadline {
            return None;
        }
        // The platform captures a host-account process observation; this does
        // not substitute for kernel socket credentials on caller attribution.
        let peer = crate::platform::invocation_peer_for_process(foreground)?;
        if !crate::platform::invocation_peer_is_current(&peer)
            || (self.foreground)() != Some(foreground)
        {
            return None;
        }
        crate::action_invocation::observed_execution(
            &peer,
            self.terminal_id.clone(),
            self.root_pid,
            self.root_birth,
            foreground,
            self.known_agent,
        )
    }
}

fn agents_mut(result: &mut ResponseResult) -> Option<&mut [AgentInfo]> {
    match result {
        ResponseResult::AgentList { agents } => Some(agents),
        ResponseResult::AgentInfo { agent } => Some(std::slice::from_mut(agent)),
        _ => None,
    }
}

/// Captures only narrow handles/host facts on the App actor, then does all process
/// capture and revalidation off actor. Saturation leaves optional proof absent.
pub(crate) fn respond(
    app: &crate::app::App,
    response: String,
    tx: std::sync::mpsc::Sender<String>,
) {
    let Ok(mut parsed) = serde_json::from_str::<SuccessResponse>(&response) else {
        let _ = tx.send(response);
        return;
    };
    let Some(agents) = agents_mut(&mut parsed.result) else {
        let _ = tx.send(response);
        return;
    };
    let Some(slot) = ReadSlot::acquire() else {
        let _ = tx.send(response);
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    let probes = agents
        .iter()
        .take(128)
        .enumerate()
        .filter_map(|(index, agent)| {
            let id = crate::terminal::TerminalId::parse(&agent.terminal_id).ok()?;
            let runtime = app.terminal_runtimes.get(&id)?;
            let terminal = app.state.terminals.get(&id)?;
            Some(Probe {
                index,
                terminal_id: agent.terminal_id.clone(),
                root_pid: runtime.child_pid()?,
                root_birth: Some(runtime.invocation_root_birth()?),
                known_agent: Some(terminal.effective_known_agent()?),
                foreground: runtime.invocation_foreground_reader(),
            })
        })
        .collect::<Vec<_>>();
    if probes.is_empty() {
        let _ = tx.send(response);
        return;
    }
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        if let Some(agents) = agents_mut(&mut parsed.result) {
            for probe in probes {
                if Instant::now() >= deadline {
                    break;
                }
                if let Some(agent) = agents.get_mut(probe.index) {
                    agent.execution = probe.observe(deadline);
                }
            }
        }
        let enriched = serde_json::to_string(&parsed).unwrap_or(response);
        let _ = tx.send(enriched);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_budget_is_bounded_and_released() {
        let slots = (0..8)
            .map(|_| ReadSlot::acquire().expect("available slot"))
            .collect::<Vec<_>>();
        assert!(ReadSlot::acquire().is_none());
        drop(slots);
        assert!(ReadSlot::acquire().is_some());
        let unwind = std::panic::catch_unwind(|| {
            let _slot = ReadSlot::acquire().expect("panic slot");
            panic!("test drop on worker panic");
        });
        assert!(unwind.is_err());
        assert_eq!(READS.load(Ordering::Acquire), 0);
    }

    #[test]
    fn expired_or_unprovable_probe_never_queries_foreground() {
        for (birth, known, expired) in [
            (Some(1), Some(crate::detect::Agent::Pi), true),
            (None, Some(crate::detect::Agent::Pi), false),
            (Some(1), None, false),
        ] {
            let probe = Probe {
                index: 0,
                terminal_id: "terminal".into(),
                root_pid: 1,
                root_birth: birth,
                known_agent: known,
                foreground: Box::new(|| panic!("must not query")),
            };
            let deadline = if expired {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(2)
            };
            assert_eq!(probe.observe(deadline), None);
        }
    }

    #[test]
    fn delayed_foreground_does_not_start_process_capture_after_deadline() {
        let probe = Probe {
            index: 0,
            terminal_id: "terminal".into(),
            root_pid: 1,
            root_birth: Some(1),
            known_agent: Some(crate::detect::Agent::Pi),
            foreground: Box::new(|| {
                std::thread::sleep(Duration::from_millis(15));
                Some(u32::MAX)
            }),
        };
        assert_eq!(
            probe.observe(Instant::now() + Duration::from_millis(5)),
            None
        );
    }
}
