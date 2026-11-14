use super::harness::*;

#[test]
fn agent_start_preflight_rejects_invalid_arguments_without_connecting() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    for args in [
        vec!["agent", "start", "reviewer", "--kind", "pi", "--kind", "pi"],
        vec![
            "agent", "start", "reviewer", "--pane", "w1:p1", "--pane", "w1:p2",
        ],
        vec![
            "agent",
            "start",
            "reviewer",
            "--timeout",
            "30000",
            "--timeout",
            "30000",
        ],
        vec!["agent", "start", "Bad-Name"],
        vec!["agent", "start", "reviewer", "--timeout", "3000"],
        vec!["agent", "start", "reviewer", "--", "bad\nargument"],
    ] {
        let result = run_cli(&socket, &args);
        let expected = if String::from_utf8_lossy(&result.stderr).contains("duplicate option") {
            2
        } else {
            1
        };
        assert_eq!(result.status.code(), Some(expected), "{args:?}: {result:?}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
    cleanup_test_base(&base);
}

fn rejected_start(created: bool, error: &str, changed_terminal: bool) {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let error = error.to_owned();
    let server = thread::spawn(move || {
        let mut steps = Vec::new();
        if created {
            steps.push((
                "agent.get",
                serde_json::json!({"error": {"code": "agent_not_found"}}),
            ));
            steps.push((
                "pane.split",
                serde_json::json!({"result": {"pane": {"pane_id": "w1:p2"}}}),
            ));
        }
        steps.push((
            "pane.get",
            serde_json::json!({"result": {"pane": {"terminal_id": "term_new"}}}),
        ));
        steps.push((
            "agent.start",
            serde_json::json!({"error": {"code": error, "message": "rejected"}}),
        ));
        if error == "agent_pane_busy" {
            steps.push((
                "pane.get",
                serde_json::json!({"result": {"pane": {"terminal_id": "term_new"}}}),
            ));
            steps.push((
                "pane.process_info",
                serde_json::json!({"result": {"process_info": {}}}),
            ));
        }
        let cleanup = created && error != "agent_start_input_failed";
        if cleanup {
            steps.push(("pane.get", serde_json::json!({"result": {"pane": {"terminal_id": if changed_terminal { "term_replacement" } else { "term_new" }}}})));
            if !changed_terminal {
                steps.push(("pane.close", serde_json::json!({"result": {"type": "ok"}})));
            }
        }
        for (method, mut response) in steps {
            let (mut stream, line) = accept_fake_cli_operation(&listener);
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], method);
            if method == "pane.close" {
                assert_eq!(request["params"]["pane_id"], "w1:p2");
            }
            response["id"] = request["id"].clone();
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
        }
        listener.set_nonblocking(true).unwrap();
        listener
    });
    let mut args = vec!["agent", "start", "reviewer", "--kind", "opencode"];
    if !created {
        args.extend(["--pane", "w1:p2"]);
    }
    let result = run_cli(&socket, &args);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    let listener = server.join().unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    cleanup_test_base(&base);
}

#[test]
fn agent_start_retains_new_pane_after_execution_or_transport_failure() {
    for transport_failure in [false, true] {
        let base = unique_test_dir();
        fs::create_dir_all(&base).unwrap();
        let socket = base.join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server =
            thread::spawn(move || {
                let mut steps = vec![
                    (
                        "agent.get",
                        serde_json::json!({"error": {"code": "agent_not_found"}}),
                    ),
                    (
                        "pane.split",
                        serde_json::json!({"result": {"pane": {"pane_id": "w1:p2"}}}),
                    ),
                    (
                        "pane.get",
                        serde_json::json!({"result": {"pane": {"terminal_id": "term_new"}}}),
                    ),
                    (
                        "agent.start",
                        serde_json::json!({"result": {"agent": {"terminal_id": "term_new"}}}),
                    ),
                ];
                if !transport_failure {
                    steps.push(("agent.get", serde_json::json!({"result": {"agent": {
                    "pane_id": "w1:p2", "terminal_id": "term_new", "name": "reviewer",
                    "agent": "opencode", "agent_status": "blocked", "interactive_ready": true
                }}})));
                }
                for (method, mut response) in steps {
                    let (mut stream, line) = accept_fake_cli_operation(&listener);
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["method"], method);
                    if transport_failure && method == "agent.start" {
                        drop(stream);
                        break;
                    }
                    response["id"] = request["id"].clone();
                    writeln!(stream, "{response}").unwrap();
                    stream.flush().unwrap();
                }
                listener.set_nonblocking(true).unwrap();
                listener
            });
        let result = run_cli(
            &socket,
            &["agent", "start", "reviewer", "--kind", "opencode"],
        );
        assert!(!result.status.success());
        let listener = server.join().unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        cleanup_test_base(&base);
    }
}

#[test]
fn agent_start_cleans_only_owned_pre_execution_rejections() {
    rejected_start(true, "agent_pane_busy", false);
    rejected_start(false, "agent_pane_busy", false);
    rejected_start(true, "invalid_agent_argument", false);
    rejected_start(false, "invalid_agent_argument", false);
    rejected_start(true, "agent_start_input_failed", false);
    rejected_start(true, "invalid_agent_argument", true);
}
