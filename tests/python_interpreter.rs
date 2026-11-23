//! The Rust fixture path uses the same resolver as the Python harness and gate.
#[path = "support/python.rs"]
mod test_python;

#[test]
fn resolved_python_meets_the_product_minimum() {
    let status = std::process::Command::new(test_python::python())
        .args(["-I", "-c", "import sys; assert sys.version_info >= (3, 11)"])
        .status()
        .unwrap();
    assert!(status.success());
}
