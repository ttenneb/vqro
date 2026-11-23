//! Shared test-only interpreter selection; product resolution is deliberately unchanged.
pub fn python() -> &'static str {
    static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PYTHON.get_or_init(|| {
        let output = std::process::Command::new("/bin/sh")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/test-python.sh"))
            .output().expect("run test Python resolver");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).expect("Python path is UTF-8").trim().to_owned()
    })
}
