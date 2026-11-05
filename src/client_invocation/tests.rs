use super::*;
use crate::action_invocation::{InvocationAccount, InvocationProcess};

fn observation(
    servers: &[crate::pty_ownership::ServerKey],
    ttys: &[TtyIdentity],
) -> crate::pty_ownership::observation::ObservedOwnership {
    let mut statuses = vec![crate::pty_ownership::OwnershipStatus {
        server: crate::pty_ownership::ServerKey {
            boot_id: "boot".into(),
            pid: 999,
            birth: 777,
        },
        epoch: 1,
        complete: true,
    }];
    statuses.extend(
        servers
            .iter()
            .cloned()
            .map(|server| crate::pty_ownership::OwnershipStatus {
                server,
                epoch: 1,
                complete: true,
            }),
    );
    crate::pty_ownership::observation::ObservedOwnership {
        proof: crate::pty_ownership::OwnershipProof::capture(&statuses)
            .expect("validated test publishers"),
        owned_ttys: ttys.to_vec(),
    }
}

fn classify_external(
    current: &ClientAttachEvidence,
    attached: &ClientAttachEvidence,
    owned: bool,
    ttys: &[TtyIdentity],
) -> Result<ClientIdentity, String> {
    super::classify_external(current, attached, owned, &observation(&[], ttys))
}

fn evidence() -> ClientAttachEvidence {
    ClientAttachEvidence {
        peer: InvocationPeer {
            account: InvocationAccount::Unix { uid: 1000 },
            boot_id: "boot".into(),
            ancestry: vec![InvocationProcess {
                pid: 10,
                parent_pid: 0,
                birth: 20,
                process: crate::platform::ForegroundProcess {
                    pid: 10,
                    name: "vqro".into(),
                    argv0: Some("vqro".into()),
                    cmdline: Some("vqro attach".into()),
                    argv: Some(vec!["vqro".into(), "attach".into()]),
                },
            }],
            foreground_cli: false,
            foreground: None,
        },
        complete_ancestry: true,
        tty: Some(TtyIdentity {
            device: 1,
            inode: 2,
            rdev: 100,
        }),
    }
}

#[test]
fn external_caller_requires_complete_current_peer_tty_and_no_owned_pane() {
    let original = evidence();
    let caller = classify_external(&original, &original, false, &[]).expect("outer client");
    assert_eq!(caller.pid, 10);
    assert_eq!(caller.birth, 20);
    assert_eq!(caller.tty.kind, "unix");
    assert!(classify_external(&original, &original, true, &[]).is_err());
    let mut current = original.clone();
    current.complete_ancestry = false;
    assert!(
        classify_external(&current, &original, false, &[]).is_err(),
        "Some truncated ancestry is not proof"
    );
    let mut current = original.clone();
    current.tty = None;
    assert!(classify_external(&current, &original, false, &[]).is_err());
    let mut current = original.clone();
    current.peer.ancestry[0].birth += 1;
    assert!(classify_external(&current, &original, false, &[]).is_err());
    let mut current = original.clone();
    current.peer.account = InvocationAccount::Unix { uid: 1001 };
    assert!(classify_external(&current, &original, false, &[]).is_err());
    let mut current = original.clone();
    current.tty.as_mut().expect("tty").rdev += 1;
    assert!(classify_external(&current, &original, false, &[]).is_err());
}

#[test]
fn any_known_owned_tty_or_other_vqro_server_ancestry_stays_read_only() {
    let mut current = evidence();
    assert!(classify_external(
        &current,
        &current,
        false,
        &[current.tty.clone().expect("tty")]
    )
    .is_err());
    let mut server = current.peer.ancestry[0].clone();
    server.pid = 9;
    server.process.pid = 9;
    server.process.argv = Some(vec!["/other/root/vqro-dev".into(), "server".into()]);
    server.process.name = "vqro-dev".into();
    server.process.argv0 = Some("/other/root/vqro-dev".into());
    current.peer.ancestry[0].parent_pid = 9;
    current.peer.ancestry.push(server);
    assert!(
        classify_external(&current, &current, false, &[]).is_err(),
        "no per-config-root ancestry waiver"
    );
}

#[test]
fn a_foreign_account_boundary_is_not_proof_that_no_server_exists_above_it() {
    let mut stopped = evidence();
    stopped.peer.ancestry[0].parent_pid = 777;
    stopped.complete_ancestry = true; // Even a mistaken platform completion flag cannot grant.
    assert!(classify_external(&stopped, &stopped, false, &[]).is_err());
}

#[test]
fn server_exclusion_is_conservative_across_global_session_argument_forms() {
    for args in [
        vec!["vqro", "server"],
        vec!["vqro", "--session", "private", "server"],
        vec!["vqro", "--session=private", "server"],
    ] {
        let mut server = evidence().peer.ancestry.remove(0).process;
        server.argv = Some(args.into_iter().map(str::to_owned).collect());
        assert!(
            server_ancestor(&server),
            "do not run the mutating CLI parser to recognize a server"
        );
    }
    assert!(!server_ancestor(&evidence().peer.ancestry[0].process));
}

#[test]
fn a_known_server_incarnation_is_excluded_even_if_renamed_and_argv_is_not_a_cli_command() {
    let mut client = evidence();
    let mut server = client.peer.ancestry[0].clone();
    server.pid = 9;
    server.birth = 30;
    server.process.pid = 9;
    server.process.name = "renamed-daemon".into();
    server.process.argv0 = Some("/private/custom-binary".into());
    server.process.argv = Some(vec!["custom-binary".into(), "--daemonized".into()]);
    client.peer.ancestry[0].parent_pid = 9;
    client.peer.ancestry.push(server);
    let key = crate::pty_ownership::ServerKey {
        boot_id: "boot".into(),
        pid: 9,
        birth: 30,
    };
    assert!(super::classify_external(&client, &client, false, &observation(&[key], &[])).is_err());
}

#[test]
fn client_receipt_identity_never_fabricates_a_terminal_id() {
    let evidence = evidence();
    let identity = classify_external(&evidence, &evidence, false, &[]).expect("outer client");
    let caller = serde_json::to_value(
        crate::action_invocation::InvocationCaller::ClientHumanUnverified {
            account: InvocationAccount::Unix { uid: 1000 },
            client: identity.clone(),
        },
    )
    .expect("negotiated caller");
    assert_eq!(caller["class"], "human_unverified");
    assert_eq!(caller["uid"], 1000);
    assert!(caller.get("terminal_id").is_none());
    assert_eq!(caller["client"]["pid"], 10);
    let value = serde_json::to_value(identity).expect("client identity");
    assert!(value.get("terminal_id").is_none());
    assert_eq!(value["pid"], 10);
    assert_eq!(value["birth"], 20);
    assert!(value["tty"]["device"].as_str().is_some());
}
