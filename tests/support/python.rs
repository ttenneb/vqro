//! Shared portable test-only selection. No shell is required on native Windows.
pub fn python() -> &'static str {
    static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PYTHON.get_or_init(|| {
        use std::{fs, process::{Command, Stdio}, time::{Duration, Instant}};
        let candidates = if let Some(value) = std::env::var_os("VQRO_PYTHON") {
            vec![std::path::PathBuf::from(value)]
        } else {
            let names: &[&str] = if cfg!(windows) { &["python3.exe", "python.exe"] } else { &["python3"] };
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
                .collect()
        };
        let root = tempfile::tempdir().expect("private Python bootstrap output");
        let out = root.path().join("out");
        let err = root.path().join("err");
        let mut errors = String::new();
        for candidate in candidates {
            let child = Command::new(candidate)
                .args(["-I", "-X", "utf8", concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/test_python.py")])
                .stdin(Stdio::null())
                .stdout(fs::File::create(&out).unwrap())
                .stderr(fs::File::create(&err).unwrap())
                .spawn();
            let Ok(mut child) = child else { continue };
            let deadline = Instant::now() + Duration::from_secs(15);
            let status = loop {
                if let Some(status) = child.try_wait().expect("poll owned Python bootstrap") { break Some(status); }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    child.wait().expect("reap timed-out Python bootstrap");
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            if status.is_some_and(|status| status.success()) {
                let selected = fs::read_to_string(&out).expect("Python path is UTF-8");
                let path = selected.strip_suffix('\n').unwrap_or(&selected);
                let path = if cfg!(windows) { path.strip_suffix('\r').unwrap_or(path) } else { path };
                assert!(!path.is_empty(), "empty Python resolver output");
                return path.to_owned();
            }
            errors.push_str(&fs::read_to_string(&err).unwrap_or_default());
        }
        panic!("test Python resolution failed (15s bootstrap timeout): {errors}")
    })
}
