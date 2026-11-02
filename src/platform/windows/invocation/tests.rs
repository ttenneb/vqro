use super::*;
use crate::extension_cli::ForegroundDeclaration;

fn process(pid: u32, parent_pid: u32, birth: u64, argv: &[&str]) -> InvocationProcess {
    InvocationProcess {
        pid,
        parent_pid,
        birth,
        process: crate::platform::ForegroundProcess {
            pid,
            name: argv[0].into(),
            argv0: Some(argv[0].into()),
            cmdline: Some(argv.join(" ")),
            argv: Some(argv.iter().map(|arg| (*arg).into()).collect()),
        },
    }
}

fn fixture() -> (InvocationPeer, LauncherEvidence, ForegroundDeclaration) {
    let child = process(
        30,
        20,
        300,
        &[
            r"C:\Python\python.exe",
            r"C:\Tasks\cli.py",
            "task",
            "create",
            "literal ; title",
        ],
    );
    let launcher = process(
        20,
        10,
        200,
        &[r"C:\Vqro\vqro.exe", "task", "create", "literal ; title"],
    );
    let sid = "S-1-5-21-1-2-3-1001".to_owned();
    let declaration = ForegroundDeclaration {
        argv: child.process.argv.clone().unwrap(),
        root: r"C:\Tasks".into(),
        manifest: r"C:\Tasks\vqro-extension.toml".into(),
        digest: [7; 32],
    };
    let peer = InvocationPeer {
        account: InvocationAccount::Windows { sid: sid.clone() },
        boot_id: "boot".into(),
        ancestry: vec![
            child.clone(),
            launcher.clone(),
            process(10, 1, 100, &["pwsh.exe"]),
        ],
        foreground_cli: false,
        foreground: None,
    };
    let proof = LauncherEvidence {
        child,
        launcher,
        child_sid: sid.clone(),
        launcher_sid: sid,
        images: [
            ImageIdentity {
                volume: 1,
                index: 1,
            },
            ImageIdentity {
                volume: 1,
                index: 1,
            },
            ImageIdentity {
                volume: 1,
                index: 2,
            },
            ImageIdentity {
                volume: 1,
                index: 2,
            },
        ],
        declaration: declaration.clone(),
    };
    (peer, proof, declaration)
}

#[test]
fn transparent_launcher_requires_images_full_tail_generation_and_incarnations() {
    let (peer, proof, declaration) = fixture();
    assert!(launcher_evidence_matches(&peer, &proof, &declaration));
    for field in 0..12 {
        let (peer, mut proof, mut fresh) = fixture();
        match field {
            0 => proof.images[1].index += 1,
            1 => proof.images[3].volume += 1,
            2 => {
                proof.declaration.argv.pop();
            }
            3 => proof.declaration.argv[4] = "different forwarded title".into(),
            4 => fresh.digest[0] ^= 1,
            5 => fresh.root = r"C:\DifferentGeneration".into(),
            6 => proof.child.parent_pid += 1,
            7 => proof.launcher.birth += 1,
            8 => proof.child_sid = "S-1-5-18".into(),
            9 => proof.launcher_sid = "S-1-5-18".into(),
            10 => proof.child.birth += 1,
            11 => proof.launcher.parent_pid += 1,
            _ => unreachable!(),
        }
        assert!(
            !launcher_evidence_matches(&peer, &proof, &fresh),
            "field {field}"
        );
    }
    let (mut peer, proof, declaration) = fixture();
    peer.ancestry
        .insert(0, process(40, 30, 400, &["wrapper.exe"]));
    assert!(!launcher_evidence_matches(&peer, &proof, &declaration));
    peer.ancestry.clear();
    assert!(!launcher_evidence_matches(&peer, &proof, &declaration));
}

#[test]
fn reused_parent_ordering_never_proves_the_launcher() {
    let (mut peer, mut proof, declaration) = fixture();
    peer.ancestry[1].birth = 400;
    proof.launcher.birth = 400;
    assert!(!launcher_evidence_matches(&peer, &proof, &declaration));
}

#[test]
fn live_windows_identity_is_nonzero_and_sid_not_uid() {
    let pid = std::process::id();
    let birth = invocation_process_birth(pid).expect("own process birth");
    assert_ne!(0, birth);
    assert_eq!(Some(birth), invocation_process_birth(pid));
    assert!(current_account_sid_text()
        .expect("own token SID")
        .starts_with("S-1-"));
    assert!(boot_id().is_some_and(|id| id.len() == 32));
    assert!(invocation_process_birth(0).is_none());
}
