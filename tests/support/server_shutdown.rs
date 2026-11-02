use super::*;

#[test]
fn server_survives_launching_terminal_hangup_then_saves_on_explicit_stop() {
    let _lock = test_lock();
    for api_stop in [false, true] {
        let base = unique_test_dir();
        let config_home = base.join("config");
        let runtime_dir = base.join("runtime");
        let api_socket = runtime_dir.join("herdr.sock");
        let client_socket = runtime_dir.join("herdr-client.sock");
        let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
        wait_for_socket(&api_socket, Duration::from_secs(10));
        let mut stream = UnixStream::connect(&api_socket).unwrap();
        writeln!(stream, "{}", serde_json::json!({"id":"create", "method":"workspace.create", "params":{"cwd":base,"label":"saved after hangup","focus":true}})).unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert!(!response.contains("\"error\""), "{response}");
        let pid = spawned.child.process_id().unwrap() as libc::pid_t;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGHUP) }, 0);
        spawned.close_master();
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until {
            assert!(
                spawned.child.try_wait().unwrap().is_none(),
                "hangup stopped detached server"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(ping_socket(&api_socket).contains("pong"));
        let mut reattached = UnixStream::connect(&client_socket).unwrap();
        let (_, error) = client_handshake(&mut reattached, CURRENT_PROTOCOL, 80, 24).unwrap();
        assert!(
            error.is_none(),
            "new client must reconnect after launching shell closes"
        );
        drop(reattached);
        if api_stop {
            let mut stream = UnixStream::connect(&api_socket).unwrap();
            writeln!(
                stream,
                "{}",
                serde_json::json!({"id":"stop", "method":"server.stop", "params":{}})
            )
            .unwrap();
            let mut response = String::new();
            BufReader::new(stream).read_line(&mut response).unwrap();
            assert!(response.contains("\"ok\""), "{response}");
        } else {
            assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        }
        let until = Instant::now() + Duration::from_secs(10);
        while spawned.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "explicit stop must exit");
            thread::sleep(Duration::from_millis(20));
        }
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(config_home.join("vqro-dev/session.json")).unwrap())
                .unwrap();
        assert_eq!(saved["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(saved["workspaces"][0]["custom_name"], "saved after hangup");
        cleanup_spawned_herdr(spawned, base);
    }
}
