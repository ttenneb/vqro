use super::*;
use crate::action_invocation::{InvocationAccount, InvocationProcess};

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
