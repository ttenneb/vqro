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

type Step = (&'static str, Option<serde_json::Value>);

fn scripted_start(created: bool, steps: Vec<Step>) -> std::process::Output {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        for (method, response) in steps {
            let (mut stream, line) = accept_fake_cli_operation(&listener);
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], method);
            if method == "pane.close_if_pristine_terminal" {
                assert_eq!(
                    request["params"],
                    serde_json::json!({"pane_id": "w1:p2", "expected_terminal_id": "term_created"})
                );
            }
            let Some(mut response) = response else {
                drop(stream);
                break;
            };
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
    assert!(!result.status.success(), "{result:?}");
    let listener = server.join().unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "unexpected extra operation (no unconditional close fallback)"
    );
    cleanup_test_base(&base);
    result
}

fn creation_steps(terminal: Option<&str>) -> Vec<Step> {
    vec![
        (
            "agent.get",
            Some(serde_json::json!({"error": {"code": "agent_not_found"}})),
        ),
        (
            "pane.split",
            Some(
                serde_json::json!({"result": {"pane": {"pane_id": "w1:p2", "terminal_id": terminal}}}),
            ),
        ),
    ]
}

fn rejected_start(
    created: bool,
    error: &str,
    cleanup_error: Option<&str>,
    creation_identity: bool,
) {
    let mut steps = if created {
        creation_steps(creation_identity.then_some("term_created"))
    } else {
        vec![(
            "pane.get",
            Some(serde_json::json!({"result": {"pane": {"terminal_id": "term_created"}}})),
        )]
    };
    steps.push((
        "agent.start",
        Some(serde_json::json!({"error": {"code": error, "message": "rejected"}})),
    ));
    if error == "agent_pane_busy" && (!created || creation_identity) {
        steps.push((
            "pane.get",
            Some(serde_json::json!({"result": {"pane": {"terminal_id": "term_created"}}})),
        ));
        steps.push((
            "pane.process_info",
            Some(serde_json::json!({"result": {"process_info": {}}})),
        ));
    }
    if created && creation_identity && error != "agent_start_input_failed" {
        let result = if let Some(error) = cleanup_error {
            serde_json::json!({"error": {"code": error, "message": "retained"}})
        } else {
            serde_json::json!({"result": {"type": "ok"}})
        };
        steps.push(("pane.close_if_pristine_terminal", Some(result)));
    }
    let result = scripted_start(created, steps);
    assert_eq!(result.status.code(), Some(1));
    let response: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(response["error"]["code"], error);
    if cleanup_error.is_some() {
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("retained"));
    }
}

#[test]
fn agent_start_cleans_only_creation_bound_pre_execution_rejections() {
    rejected_start(true, "agent_pane_busy", None, true);
    rejected_start(false, "agent_pane_busy", None, true);
    rejected_start(true, "invalid_agent_argument", None, true);
    rejected_start(false, "invalid_agent_argument", None, true);
    rejected_start(true, "agent_start_input_failed", None, true);
    // Host replacement after any observation is rejected atomically; the CLI
    // never obtains ownership from a later pane.get or issues pane.close.
    rejected_start(
        true,
        "invalid_agent_argument",
        Some("pristine_close_rejected"),
        true,
    );
    rejected_start(
        true,
        "invalid_agent_argument",
        Some("method_not_found"),
        true,
    );
    rejected_start(true, "invalid_agent_argument", None, false);
}

#[test]
fn agent_start_retains_new_pane_after_execution_or_transport_failure() {
    for transport_failure in [false, true] {
        let mut steps = creation_steps(Some("term_created"));
        steps.push((
            "agent.start",
            (!transport_failure)
                .then(|| serde_json::json!({"result": {"agent": {"terminal_id": "term_created"}}})),
        ));
        if !transport_failure {
            steps.push((
                "agent.get",
                Some(serde_json::json!({"result": {"agent": {
                    "pane_id": "w1:p2", "terminal_id": "term_created", "name": "reviewer",
                    "agent": "opencode", "agent_status": "blocked", "interactive_ready": true
                }}})),
            ));
        }
        scripted_start(true, steps);
    }
}
