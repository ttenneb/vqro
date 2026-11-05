//! Neutral attribution for trusted executable action calls.
//! Cooperative local process-lifetime identity, not same-user human authentication.
//! Same-PID exec is the same process lifetime; imported roots lack authority proof.
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvocationProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub birth: u64,
    pub process: crate::platform::ForegroundProcess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(crate) enum InvocationAccount {
    #[cfg_attr(not(unix), allow(dead_code))]
    // Unix account serialization is also a contract test on Windows.
    Unix { uid: u32 },
    #[cfg_attr(not(windows), allow(dead_code))]
    // Windows account serialization is also a contract test on Unix.
    Windows { sid: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvocationForeground {
    pub root_pid: u32,
    pub root_birth: u64,
    pub pid: u32,
    pub birth: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvocationPeer {
    pub account: InvocationAccount,
    pub boot_id: String,
    pub ancestry: Vec<InvocationProcess>,
    /// Off-actor proof of the exact one-hop foreground CLI launcher. Never
    /// inferred from a name, environment marker or arbitrary grandchild depth.
    pub foreground_cli: bool,
    /// None uses live runtime PTY evidence. Some uses only these request-local
    /// off-actor selected-job observations; an empty set grants no agent proof.
    pub foreground: Option<Vec<InvocationForeground>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub(crate) enum InvocationCaller {
    Unknown,
    HumanUnverified {
        terminal_id: String,
        #[serde(flatten)]
        account: InvocationAccount,
    },
    /// Explicitly negotiated client source, never sent to legacy
    /// action.invoke.v1 providers. It carries no synthetic terminal identity.
    #[serde(rename = "human_unverified")]
    ClientHumanUnverified {
        #[serde(flatten)]
        account: InvocationAccount,
        client: crate::client_invocation::ClientIdentity,
    },
    Agent {
        terminal_id: String,
        agent: String,
        execution: String,
        session: Option<serde_json::Value>,
    },
}

struct TerminalEvidence<'a> {
    terminal_id: String,
    root_pid: u32,
    root_birth: Option<u64>,
    foreground: Option<u32>,
    known_agent: Option<crate::detect::Agent>,
    has_agent_label: bool,
    session: Option<&'a crate::agent_resume::PersistedAgentSession>,
}

fn is_shell(name: &str, account: &InvocationAccount) -> bool {
    match account {
        InvocationAccount::Unix { .. } => matches!(name, "bash" | "sh" | "zsh" | "fish" | "dash"),
        InvocationAccount::Windows { .. } => {
            let name = name.to_ascii_lowercase();
            matches!(
                name.trim_end_matches(".exe"),
                "bash" | "sh" | "zsh" | "fish" | "dash" | "cmd" | "powershell" | "pwsh"
            )
        }
    }
}

fn is_shell_wrapper(process: &crate::platform::ForegroundProcess) -> bool {
    [
        process.name.as_str(),
        process.argv0.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .any(|name| {
        let basename = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase();
        matches!(
            basename.trim_end_matches(".exe"),
            "sh" | "bash" | "zsh" | "fish" | "dash" | "cmd" | "powershell" | "pwsh"
        )
    })
}

fn classify(peer: &InvocationPeer, terminal: TerminalEvidence<'_>) -> Option<InvocationCaller> {
    let root_index = peer
        .ancestry
        .iter()
        .position(|entry| entry.pid == terminal.root_pid)?;
    if terminal.root_birth != Some(peer.ancestry[root_index].birth) {
        return Some(InvocationCaller::Unknown);
    }
    let foreground = match &peer.foreground {
        None => terminal.foreground,
        Some(observations) => {
            let mut proofs = observations
                .iter()
                .filter(|proof| proof.root_pid == terminal.root_pid);
            let first = proofs.next();
            if proofs.next().is_some() {
                None
            } else {
                first
                    .filter(|proof| {
                        Some(proof.root_birth) == terminal.root_birth
                            && peer
                                .ancestry
                                .iter()
                                .any(|entry| entry.pid == proof.pid && entry.birth == proof.birth)
                    })
                    .map(|proof| proof.pid)
                    .filter(|pid| terminal.foreground.is_none_or(|observed| observed == *pid))
            }
        }
    };
    for entry in &peer.ancestry[..=root_index] {
        let job = crate::platform::ForegroundJob {
            process_group_id: entry.pid,
            processes: vec![entry.process.clone()],
        };
        if let Some((agent, _)) = crate::detect::identify_agent_in_job(&job) {
            if foreground != Some(entry.pid)
                || is_shell_wrapper(&entry.process)
                || terminal.known_agent != Some(agent)
            {
                // Nested agents and shell command wrappers cannot inherit the foreground
                // principal. A label alone is never enough to bind an execution.
                return Some(InvocationCaller::Unknown);
            }
            return Some(InvocationCaller::Agent {
                terminal_id: terminal.terminal_id,
                agent: crate::detect::agent_label(agent).to_string(),
                execution: format!("{}:{}:{}", peer.boot_id, entry.pid, entry.birth),
                session: terminal.session.map(|session| {
                    serde_json::json!({
                        "source": session.source, "agent": session.agent,
                        "kind": session.session_ref.kind, "value": session.session_ref.value,
                    })
                }),
            });
        }
    }
    let root = &peer.ancestry[root_index];
    Some(
        if (root_index == 1 || (root_index == 2 && peer.foreground_cli))
            && !terminal.has_agent_label
            && is_shell(&root.process.name, &peer.account)
        {
            InvocationCaller::HumanUnverified {
                terminal_id: terminal.terminal_id,
                account: peer.account.clone(),
            }
        } else {
            InvocationCaller::Unknown
        },
    )
}

pub(crate) fn observed_execution(
    peer: &InvocationPeer,
    terminal_id: String,
    root_pid: u32,
    root_birth: Option<u64>,
    foreground: u32,
    known_agent: Option<crate::detect::Agent>,
) -> Option<String> {
    match classify(
        peer,
        TerminalEvidence {
            terminal_id,
            root_pid,
            root_birth,
            foreground: Some(foreground),
            known_agent,
            has_agent_label: true,
            session: None,
        },
    ) {
        Some(InvocationCaller::Agent { execution, .. }) => Some(execution),
        _ => None,
    }
}

impl crate::app::App {
    pub(crate) fn invocation_caller(&self, peer: &InvocationPeer) -> InvocationCaller {
        for (terminal_id, runtime) in self.terminal_runtimes.iter() {
            let Some(root_pid) = runtime.child_pid() else {
                continue;
            };
            // This explicit invocation is not a render path. Still avoid allocations and
            // PTY foreground queries for terminals outside the captured peer ancestry.
            if !peer.ancestry.iter().any(|entry| entry.pid == root_pid) {
                continue;
            }
            let Some(terminal) = self.state.terminals.get(terminal_id) else {
                continue;
            };
            if let Some(caller) = classify(
                peer,
                TerminalEvidence {
                    terminal_id: terminal_id.to_string(),
                    root_pid,
                    root_birth: runtime.invocation_root_birth(),
                    foreground: runtime.invocation_foreground_process_group(),
                    known_agent: terminal.effective_known_agent(),
                    has_agent_label: terminal.effective_agent_label().is_some(),
                    session: terminal.persisted_agent_session.as_ref(),
                },
            ) {
                return caller;
            }
        }
        InvocationCaller::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Agent;

    fn process(pid: u32, parent_pid: u32, birth: u64, name: &str) -> InvocationProcess {
        InvocationProcess {
            pid,
            parent_pid,
            birth,
            process: crate::platform::ForegroundProcess {
                pid,
                name: name.into(),
                argv0: Some(name.into()),
                argv: Some(vec![name.into()]),
                cmdline: Some(name.into()),
            },
        }
    }
    #[test]
    fn execution_observation_reuses_original_root_and_foreground_rules() {
        let peer = peer(vec![
            process(20, 10, 200, "pi"),
            process(10, 1, 100, "bash"),
        ]);
        assert_eq!(
            observed_execution(&peer, "term".into(), 10, Some(100), 20, Some(Agent::Pi)),
            Some("boot:20:200".into())
        );
        for (birth, foreground, agent) in [
            (None, 20, Some(Agent::Pi)),
            (Some(101), 20, Some(Agent::Pi)),
            (Some(100), 21, Some(Agent::Pi)),
            (Some(100), 20, None),
            (Some(100), 20, Some(Agent::Codex)),
        ] {
            assert_eq!(
                observed_execution(&peer, "term".into(), 10, birth, foreground, agent),
                None
            );
        }
        let shell = self::peer(vec![
            process(20, 10, 200, "sh"),
            process(10, 1, 100, "bash"),
        ]);
        assert_eq!(
            observed_execution(&shell, "term".into(), 10, Some(100), 20, Some(Agent::Pi)),
            None
        );
    }

    #[test]
    fn captured_selected_job_requires_both_incarnations_and_agrees_with_probe() {
        let mut peer = agent_peer();
        peer.account = InvocationAccount::Windows {
            sid: "S-1-5-21-1-2-3-1001".into(),
        };
        let proof = InvocationForeground {
            root_pid: 10,
            root_birth: 100,
            pid: 20,
            birth: 200,
        };
        peer.foreground = Some(vec![proof.clone()]);
        let mut action = terminal(20, Some(Agent::Pi));
        action.foreground = None; // Windows actor never queries native processes.
        assert!(matches!(
            classify(&peer, action),
            Some(InvocationCaller::Agent { .. })
        ));
        assert_eq!(
            observed_execution(&peer, "term".into(), 10, Some(100), 20, Some(Agent::Pi)),
            Some("boot:20:200".into())
        );
        assert_eq!(
            observed_execution(&peer, "term".into(), 10, Some(100), 21, Some(Agent::Pi)),
            None
        );
        for stale in [
            InvocationForeground {
                root_birth: 101,
                ..proof.clone()
            },
            InvocationForeground {
                birth: 201,
                ..proof.clone()
            },
            InvocationForeground {
                pid: 99,
                ..proof.clone()
            },
        ] {
            peer.foreground = Some(vec![stale]);
            assert_eq!(
                observed_execution(&peer, "term".into(), 10, Some(100), 20, Some(Agent::Pi)),
                None
            );
        }
        peer.foreground = Some(Vec::new());
        assert_eq!(
            observed_execution(&peer, "term".into(), 10, Some(100), 20, Some(Agent::Pi)),
            None
        );
        peer.foreground = Some(vec![proof]);
        peer.ancestry[0].process.name = "codex".into();
        peer.ancestry[0].process.argv0 = Some("codex".into());
        peer.ancestry[0].process.argv = Some(vec!["codex".into()]);
        assert_eq!(
            observed_execution(&peer, "term".into(), 10, Some(100), 20, Some(Agent::Pi)),
            None
        );
        let mut direct = self::peer(vec![process(10, 1, 100, "pi")]);
        direct.foreground = Some(vec![InvocationForeground {
            root_pid: 10,
            root_birth: 100,
            pid: 10,
            birth: 100,
        }]);
        assert_eq!(
            observed_execution(&direct, "term".into(), 10, Some(100), 10, Some(Agent::Pi)),
            Some("boot:10:100".into())
        );
    }

    #[test]
    fn windows_sid_shape_preserves_unix_json_and_requires_launcher_proof() {
        let unix = InvocationCaller::HumanUnverified {
            terminal_id: "term".into(),
            account: InvocationAccount::Unix { uid: 1000 },
        };
        assert_eq!(
            serde_json::to_value(unix).unwrap(),
            serde_json::json!({"class":"human_unverified", "terminal_id":"term", "uid":1000})
        );
        let account = InvocationAccount::Windows {
            sid: "S-1-5-21-1-2-3-1001".into(),
        };
        let windows = InvocationCaller::HumanUnverified {
            terminal_id: "term".into(),
            account: account.clone(),
        };
        assert_eq!(
            serde_json::to_value(windows).unwrap(),
            serde_json::json!({"class":"human_unverified", "terminal_id":"term", "sid":"S-1-5-21-1-2-3-1001"})
        );
        let mut peer = peer(vec![
            process(30, 20, 300, "python.exe"),
            process(20, 10, 200, "vqro.exe"),
            process(10, 1, 100, "pwsh.exe"),
        ]);
        peer.account = account;
        assert!(matches!(
            classify(&peer, terminal(10, None)),
            Some(InvocationCaller::Unknown)
        ));
        peer.foreground_cli = true;
        assert!(matches!(
            classify(&peer, terminal(10, None)),
            Some(InvocationCaller::HumanUnverified { .. })
        ));
        assert!(matches!(
            classify(&peer, terminal(20, Some(Agent::Pi))),
            Some(InvocationCaller::Unknown)
        ));
        peer.ancestry.insert(0, process(40, 30, 400, "helper.exe"));
        assert!(matches!(
            classify(&peer, terminal(10, None)),
            Some(InvocationCaller::Unknown)
        ));
        peer.ancestry.remove(0);
        let mut stale = terminal(10, None);
        stale.root_birth = Some(101);
        assert!(matches!(
            classify(&peer, stale),
            Some(InvocationCaller::Unknown)
        ));
    }

    fn peer(ancestry: Vec<InvocationProcess>) -> InvocationPeer {
        InvocationPeer {
            account: InvocationAccount::Unix { uid: 1000 },
            boot_id: "boot".into(),
            ancestry,
            foreground_cli: false,
            foreground: None,
        }
    }
    fn terminal(foreground: u32, known_agent: Option<Agent>) -> TerminalEvidence<'static> {
        TerminalEvidence {
            terminal_id: "term_test".into(),
            root_pid: 10,
            root_birth: Some(100),
            foreground: Some(foreground),
            known_agent,
            has_agent_label: known_agent.is_some(),
            session: None,
        }
    }
    fn agent_peer() -> InvocationPeer {
        peer(vec![
            process(30, 20, 300, "vqro"),
            process(20, 10, 200, "pi"),
            process(10, 1, 100, "bash"),
        ])
    }

    #[test]
    fn current_foreground_leader_in_ancestry_is_exact_owner() {
        let caller = classify(&agent_peer(), terminal(20, Some(Agent::Pi))).unwrap();
        let InvocationCaller::Agent {
            execution,
            terminal_id,
            ..
        } = caller
        else {
            panic!("expected agent")
        };
        assert_eq!(execution, "boot:20:200");
        assert_eq!(terminal_id, "term_test");
    }
    #[test]
    fn caller_requires_foreground_execution_not_agent_type() {
        assert!(matches!(
            classify(&agent_peer(), terminal(99, Some(Agent::Pi))),
            Some(InvocationCaller::Unknown)
        ));
        assert!(matches!(
            classify(&agent_peer(), terminal(20, Some(Agent::Codex))),
            Some(InvocationCaller::Unknown)
        ));
    }
    #[test]
    fn same_kind_descendant_cannot_inherit_foreground_principal() {
        let nested = peer(vec![
            process(40, 30, 400, "vqro"),
            process(30, 20, 300, "pi"),
            process(20, 10, 200, "pi"),
            process(10, 1, 100, "bash"),
        ]);
        assert!(matches!(
            classify(&nested, terminal(20, Some(Agent::Pi))),
            Some(InvocationCaller::Unknown)
        ));
    }
    #[test]
    fn foreground_leader_shell_wrapper_cannot_inherit_across_child_restart() {
        let mut wrapped = agent_peer();
        wrapped.ancestry[1].process = crate::platform::ForegroundProcess {
            pid: 20,
            name: "bash".into(),
            argv0: Some("bash".into()),
            argv: Some(vec!["bash".into(), "/usr/local/bin/pi".into()]),
            cmdline: Some("bash /usr/local/bin/pi".into()),
        };
        assert!(
            crate::detect::identify_agent_in_job(&crate::platform::ForegroundJob {
                process_group_id: 20,
                processes: vec![wrapped.ancestry[1].process.clone()]
            })
            .is_some()
        );
        assert!(matches!(
            classify(&wrapped, terminal(20, Some(Agent::Pi))),
            Some(InvocationCaller::Unknown)
        ));
    }
    #[test]
    fn all_detector_shell_families_cannot_bind_foreground_execution() {
        for (shell, args) in [
            ("pwsh", vec!["pwsh", "-File", "/tmp/pi"]),
            ("powershell", vec!["powershell", "-File", "/tmp/pi"]),
            ("cmd", vec!["cmd", "/C", "pi"]),
        ] {
            let mut wrapped = agent_peer();
            let process = &mut wrapped.ancestry[1].process;
            process.name = shell.into();
            process.argv0 = Some(shell.into());
            process.argv = Some(args.iter().map(|arg| arg.to_string()).collect());
            process.cmdline = Some(args.join(" "));
            assert!(
                crate::detect::identify_agent_in_job(&crate::platform::ForegroundJob {
                    process_group_id: 20,
                    processes: vec![process.clone()]
                })
                .is_some(),
                "detector shell {shell}"
            );
            assert!(
                matches!(
                    classify(&wrapped, terminal(20, Some(Agent::Pi))),
                    Some(InvocationCaller::Unknown)
                ),
                "shell {shell}"
            );
        }
        let mut shell_alias = process(20, 10, 200, "busybox").process;
        shell_alias.argv0 = Some("/bin/sh".into());
        assert!(is_shell_wrapper(&shell_alias));
    }

    #[test]
    fn reused_root_and_unanchored_handoff_are_unknown() {
        let mut stale = terminal(20, Some(Agent::Pi));
        stale.root_birth = Some(999);
        assert!(matches!(
            classify(&agent_peer(), stale),
            Some(InvocationCaller::Unknown)
        ));
        let mut imported = terminal(20, Some(Agent::Pi));
        imported.root_birth = None;
        assert!(matches!(
            classify(&agent_peer(), imported),
            Some(InvocationCaller::Unknown)
        ));
    }
    #[test]
    fn foreground_same_pid_new_birth_has_a_distinct_binding() {
        let first =
            serde_json::to_value(classify(&agent_peer(), terminal(20, Some(Agent::Pi))).unwrap())
                .unwrap();
        let mut replaced = agent_peer();
        replaced.ancestry[1].birth = 201;
        let second =
            serde_json::to_value(classify(&replaced, terminal(20, Some(Agent::Pi))).unwrap())
                .unwrap();
        assert_ne!(first["execution"], second["execution"]);
    }
    #[test]
    fn cooperative_human_requires_direct_owned_shell_child() {
        let direct = peer(vec![
            process(20, 10, 200, "vqro"),
            process(10, 1, 100, "bash"),
        ]);
        assert!(matches!(
            classify(&direct, terminal(20, None)),
            Some(InvocationCaller::HumanUnverified { .. })
        ));
        let unsupported = peer(vec![
            process(30, 20, 300, "vqro"),
            process(20, 10, 200, "unsupported-agent"),
            process(10, 1, 100, "bash"),
        ]);
        assert!(matches!(
            classify(&unsupported, terminal(20, None)),
            Some(InvocationCaller::Unknown)
        ));
    }
}
