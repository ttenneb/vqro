//! Ported P1 real-process crash/fault/native-ledger oracle on the product operational APIs.
#[test]
#[ignore="owned core/supervised fixture processes and explicit durability fault gates; full gate"]
fn operational_native_ledger_crash_edit_and_retirement_oracles() {
 let repo=std::path::Path::new(env!("CARGO_MANIFEST_DIR"));let root=tempfile::Builder::new().prefix("op-oracle-").tempdir().unwrap();
 let target=std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(||repo.join("target"));
 let output=std::process::Command::new("python3").env_clear().env("HOME",root.path()).env("PATH","/usr/bin:/bin").arg(repo.join("scripts/probes/operational_oracle.py")).arg("check").arg("--core").arg(target.join("debug/examples/operational-test-core")).arg("--out").arg(root.path()).output().unwrap();
 assert!(output.status.success(),"{}\n{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
}
