//! The Rust fixture path uses the same resolver as the Python harness and gate.
#[path = "support/python.rs"]
mod test_python;

#[test]
fn resolved_python_meets_the_product_minimum() {
    let mut child = std::process::Command::new(test_python::python())
        .args(["-I", "-c", "import sys; assert sys.version_info >= (3, 11)"])
        .spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() { assert!(status.success()); break; }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill(); child.wait().unwrap();
            panic!("minimum-version probe exceeded 5s");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
