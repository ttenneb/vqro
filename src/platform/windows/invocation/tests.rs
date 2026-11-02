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
        image_status: [0; 3],
        declaration: declaration.clone(),
    };
    (peer, proof, declaration)
}

#[test]
fn transparent_launcher_requires_images_full_tail_generation_and_incarnations() {
    let (peer, proof, declaration) = fixture();
    assert!(launcher_evidence_matches(&peer, &proof, &declaration));
    for field in 0..14 {
        let (peer, mut proof, mut fresh) = fixture();
        match field {
            0 => proof.image_status[1] = -1073741790, // STATUS_ACCESS_DENIED
            1 => proof.image_status[0] = 1, // Positive status is NOT exact STATUS_SUCCESS.
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
            12 => proof.image_status[2] = -1073741823, // STATUS_UNSUCCESSFUL: different mapped file.
            13 => proof.image_status[0] = -1073741821, // STATUS_INVALID_INFO_CLASS: no fallback.
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
fn foreground_freshness_rejects_new_removed_missing_and_capped_descendants() {
    let snapshot = |children: &[u32]| {
        let mut entries = vec![WindowsProcessEntry {
            pid: 10,
            parent_pid: 1,
            name: "pwsh.exe".into(),
            command: OnceLock::new(),
        }];
        entries.extend(children.iter().map(|&pid| WindowsProcessEntry {
            pid,
            parent_pid: 10,
            name: "pi.exe".into(),
            command: OnceLock::new(),
        }));
        ProcessSnapshot::new(entries)
    };
    let roots = [(10, 100)];
    let before = foreground_process_set(&roots, &snapshot(&[20])).unwrap();
    assert_eq!(
        Some(before.clone()),
        foreground_process_set(&roots, &snapshot(&[20]))
    );
    assert_ne!(
        Some(before.clone()),
        foreground_process_set(&roots, &snapshot(&[20, 30]))
    );
    assert_ne!(Some(before), foreground_process_set(&roots, &snapshot(&[])));
    assert!(foreground_process_set(&[(99, 100)], &snapshot(&[20])).is_none());
    let oversized: Vec<_> = (20..20 + FOREGROUND_QUERY_LIMIT as u32).collect();
    assert!(foreground_process_set(&roots, &snapshot(&oversized)).is_none());
    assert!(foreground_topology_is_current(
        &roots,
        &snapshot(&[20, 30]),
        &snapshot(&[20, 30])
    ));
    assert!(!foreground_topology_is_current(
        &roots,
        &snapshot(&[20]),
        &snapshot(&[20, 30])
    ));
    assert!(!foreground_topology_is_current(
        &roots,
        &snapshot(&[20, 30]),
        &snapshot(&[20])
    ));
    let moved = snapshot(&[20, 30]);
    let mut moved_entries = moved.entries;
    moved_entries
        .iter_mut()
        .find(|entry| entry.pid == 30)
        .unwrap()
        .parent_pid = 20;
    assert!(!foreground_topology_is_current(
        &roots,
        &snapshot(&[20, 30]),
        &ProcessSnapshot::new(moved_entries)
    ));
    assert!(!foreground_topology_is_current(
        &roots,
        &snapshot(&[20]),
        &snapshot(&oversized)
    ));
}

#[test]
fn loaded_mapping_rejects_a_rebound_same_name_file() {
    let executable = std::env::current_exe().unwrap();
    let file = open_image_file(&executable).expect("held executable");
    assert_eq!(
        0,
        loaded_image_status(unsafe { GetCurrentProcess() }, &file)
    );
    let directory = std::env::temp_dir().join(format!(
        "vqro-image-rebind-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let old = directory.join("old-file");
    let rebound = directory.join(executable.file_name().unwrap());
    std::fs::write(&old, b"not the running executable").unwrap();
    std::fs::rename(&old, &rebound).unwrap();
    let candidate = open_image_file(&rebound).expect("same name, unrelated file");
    assert_ne!(
        0,
        loaded_image_status(unsafe { GetCurrentProcess() }, &candidate)
    );
    drop(candidate);
    std::fs::remove_file(rebound).unwrap();
    std::fs::remove_dir(directory).unwrap();
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
