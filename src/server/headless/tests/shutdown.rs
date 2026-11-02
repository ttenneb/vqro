use super::*;

#[test]
fn host_shutdown_cancels_plans_but_settles_real_publishing_checkpoint_before_save() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let _lock = crate::config::test_config_env_lock().lock().unwrap();
    let previous = std::env::var_os("XDG_CONFIG_HOME");
    let previous_session = std::env::var_os(crate::session::SESSION_ENV_VAR);
    let root = std::env::temp_dir().join(format!(
        "vqro-settlement-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    struct Restore(
        std::path::PathBuf,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
    );
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.1 {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match &self.2 {
                Some(value) => std::env::set_var(crate::session::SESSION_ENV_VAR, value),
                None => std::env::remove_var(crate::session::SESSION_ENV_VAR),
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _restore = Restore(root.clone(), previous, previous_session);
    std::env::set_var("XDG_CONFIG_HOME", &root);
    std::env::remove_var(crate::session::SESSION_ENV_VAR);
    for mode in [
        "action-plan",
        "create-plan",
        "publishing",
        "selected-completion",
        "timeout",
        "checkpoint-error",
        "selected-error",
    ] {
        let mut server = test_headless_server();
        server.app.policy.persist_session = true;
        server.app.state.workspaces =
            vec![crate::workspace::Workspace::test_new("unsaved-shutdown")];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.app.save_session_now();
        server.app.checkpoint_session_before_pane_exit();
        let pane = server.app.state.workspaces[0].tabs[0].root_pane;
        let terminal = server.app.state.workspaces[0]
            .terminal_id(pane)
            .unwrap()
            .clone();
        server.app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        server.app.state.ensure_test_terminals();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "vqro:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("unsaved-resume").unwrap(),
            });
        server.app.state.session_dirty = true;
        let (canceled, response) = if mode == "create-plan" {
            server.app.test_queue_shutdown_create()
        } else {
            server.app.test_queue_shutdown_action()
        };
        assert!(server.app.collections_mutations_blocked);
        let high_water = server
            .app
            .capture_session_snapshot()
            .collections_action_sequence;
        assert!(high_water > 0);
        let mut worker = None;
        let mut release = None;
        if [
            "publishing",
            "selected-completion",
            "timeout",
            "checkpoint-error",
            "selected-error",
        ]
        .contains(&mode)
        {
            let (published_tx, published_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            worker = Some(server.app.test_publish_shutdown_checkpoint(
                crate::persist::collections_checkpoint_paths(),
                published_tx,
                release_rx,
            ));
            published_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(server.app.collections_pending_actions.is_empty());
            assert!(
                crate::persist::collections_checkpoint_paths()
                    .journal
                    .exists(),
                "test must pause an actual partly-published two-image checkpoint"
            );
            assert!(
                server.app.collections_mutations_blocked,
                "empty planning map cannot release publishing fence"
            );
            release = Some(release_tx);
            // A neutral resume observation newer than the publishing image must
            // survive the ordinary capture after checkpoint reconciliation.
            server
                .app
                .state
                .terminals
                .get_mut(&terminal)
                .unwrap()
                .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                    source: "vqro:codex".into(),
                    agent: "codex".into(),
                    session_ref: crate::agent_resume::AgentSessionRef::id(
                        "newer-than-publishing-image",
                    )
                    .unwrap(),
                });
        }
        #[cfg(unix)]
        {
            let result =
                server.perform_live_handoff(serde_json::from_value(serde_json::json!({})).unwrap());
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            assert!(
                !server.handoff_in_progress,
                "handoff must not export across an admitted persistence fence"
            );
        }
        server
            .host_shutdown_requested
            .store(true, Ordering::Release);
        if mode == "checkpoint-error" || mode == "selected-error" {
            let paths = crate::persist::collections_checkpoint_paths();
            let before = std::fs::read(&paths.session).unwrap();
            drop(release.take());
            if mode == "selected-error" {
                let event = runtime.block_on(server.app.event_rx.recv()).unwrap();
                assert!(matches!(
                    &event,
                    AppEvent::CollectionsCheckpointPersisted(_)
                ));
                assert!(!server.handle_internal_event_with_forwarding(event));
                assert!(!server.app.collections_mutations_blocked);
            }
            let result = runtime.block_on(server.run());
            assert!(
                result.is_err(),
                "checkpoint error must surface a failed final-save outcome"
            );
            worker.take().unwrap().join().unwrap();
            assert!(
                paths.journal.exists(),
                "uncertain publication evidence must survive"
            );
            assert_eq!(std::fs::read(&paths.session).unwrap(), before);
            // Evidence survives until explicit startup-style roll-forward.
            crate::persist::recover_collections_checkpoint().unwrap();
            assert!(!paths.journal.exists());
            continue;
        }
        if mode == "timeout" {
            let path = crate::persist::collections_checkpoint_paths().session;
            let before = std::fs::read(&path).unwrap();
            assert!(runtime
                .block_on(
                    server
                        .app
                        .settle_persistence_before_shutdown(Duration::ZERO)
                )
                .is_err());
            assert!(server.app.collections_mutations_blocked);
            server.app.save_session_on_shutdown();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "uncertain checkpoint must not be overwritten"
            );
            release.take().unwrap().send(()).unwrap();
            worker.take().unwrap().join().unwrap();
            // Finish/recover the checkpoint before the next fixture; final save remains blocked.
            let event = runtime.block_on(server.app.event_rx.recv()).unwrap();
            server.handle_internal_event_with_forwarding(event);
            continue;
        }
        if mode == "selected-completion" {
            release.take().unwrap().send(()).unwrap();
            let event = runtime.block_on(server.app.event_rx.recv()).unwrap();
            assert!(matches!(
                &event,
                AppEvent::CollectionsCheckpointPersisted(_)
            ));
            assert!(!server.handle_internal_event_with_forwarding(event));
            assert!(!server.app.collections_mutations_blocked);
        } else if let Some(release) = release.take() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                release.send(()).unwrap();
            });
        }
        server
            .app
            .event_tx
            .try_send(AppEvent::PaneDied {
                pane_id: pane,
                exit_reason: crate::platform::ChildExitReason::Exited,
            })
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), server.run())
                .await
                .unwrap()
                .unwrap();
        });
        if let Some(worker) = worker {
            worker.join().unwrap();
        }
        assert!(!server.app.collections_mutations_blocked);
        assert!(server.app.collections_pending_actions.is_empty());
        assert!(server.app.collections_pending_creates.is_empty());
        assert_eq!(canceled.load(Ordering::Acquire), mode.ends_with("plan"));
        assert!(response.recv_timeout(Duration::from_secs(1)).is_ok());
        // Late provider completion after cancellation must not publish anything.
        assert!(!server
            .app
            .finish_collections_action_plan(77, Err("late plan".into())));
        assert!(!server
            .app
            .finish_collections_create_stage(88, Err("late create".into())));
        let saved = crate::persist::load().unwrap();
        assert_eq!(saved.workspaces[0].tabs[0].panes.len(), 2);
        assert_eq!(
            saved.workspaces[0].tabs[0].panes[&pane.raw()]
                .agent_session
                .as_ref()
                .unwrap()
                .value,
            if ["publishing", "selected-completion"].contains(&mode) {
                "newer-than-publishing-image"
            } else {
                "unsaved-resume"
            }
        );
        assert!(saved.collections_action_sequence >= high_water);
        assert!(!crate::persist::collections_checkpoint_paths()
            .journal
            .exists());
        server.app.state.assert_invariants_for_test();
    }
}

#[test]
fn host_shutdown_guards_death_resume_autosave_and_api_before_final_save() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _runtime = runtime.enter();
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    struct RestoreEnv([(&'static str, Option<std::ffi::OsString>); 2]);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    let _env = RestoreEnv(
        ["XDG_CONFIG_HOME", crate::session::SESSION_ENV_VAR]
            .map(|key| (key, std::env::var_os(key))),
    );
    let root = std::env::temp_dir().join(format!(
        "vqro-shutdown-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::env::set_var("XDG_CONFIG_HOME", &root);
    std::env::remove_var(crate::session::SESSION_ENV_VAR);
    thread_local! { static PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    fn shutdown_during_api_drain() -> bool {
        PROBES.with(|probes| {
            let n = probes.get();
            probes.set(n + 1);
            n > 0
        })
    }
    for entry in [
        "queued-death",
        "selected-death",
        "agent-exit",
        "autosave",
        "api-drain",
        "monitor-flag",
    ] {
        let mut server = test_headless_server();
        server.app.policy.persist_session = true;
        let workspace = crate::workspace::Workspace::test_new("shutdown");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().clone();
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.app.save_session_now();
        let other =
            server.app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        server.app.state.ensure_test_terminals();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "vqro:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("shutdown-session").unwrap(),
            });
        let death = || AppEvent::PaneDied {
            pane_id: pane,
            exit_reason: crate::platform::ChildExitReason::Exited,
        };
        let agent_exit = || AppEvent::StateChanged {
            pane_id: pane,
            agent: Some(crate::detect::Agent::Codex),
            state: crate::detect::AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at: Instant::now(),
        };
        server.host_shutdown_probe = || true;
        match entry {
            "queued-death" => {
                server.app.event_tx.try_send(death()).unwrap();
                assert_eq!(
                    server.drain_internal_events_with_forwarding_up_to(16),
                    (false, false)
                );
                assert!(
                    server.app.event_rx.try_recv().is_ok(),
                    "do not apply queued deaths before save"
                );
            }
            "selected-death" => assert!(!server.handle_internal_event_with_forwarding(death())),
            "agent-exit" => assert!(!server.handle_internal_event_with_forwarding(agent_exit())),
            "autosave" => {
                server.app.session_save_deadline = Some(Instant::now());
                assert!(!server.handle_scheduled_tasks_headless(Instant::now(), false));
            }
            "api-drain" => {
                PROBES.with(|probes| probes.set(0));
                server.host_shutdown_probe = shutdown_during_api_drain;
                server.app.event_tx.try_send(death()).unwrap();
                let (respond_to, rx) = std::sync::mpsc::channel();
                server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                    origin: crate::api::ApiOrigin::LocalSocket,
                    request: api::schema::Request {
                        id: "shutdown-race".into(),
                        method: api::schema::Method::WorkspaceRename(
                            api::schema::WorkspaceRenameParams {
                                workspace_id: server.app.public_workspace_id(0),
                                label: "must-not-be-saved".into(),
                            },
                        ),
                    },
                    respond_to,
                    response_write_complete: None,
                    stream_active: None,
                });
                let response: api::schema::ErrorResponse =
                    serde_json::from_str(&rx.recv().unwrap()).unwrap();
                assert_eq!(response.error.code, "server_unavailable");
            }
            "monitor-flag" => {
                server.host_shutdown_probe = || false;
                server
                    .host_shutdown_requested
                    .store(true, Ordering::Release);
                assert!(!server.handle_internal_event_with_forwarding(death()));
            }
            _ => unreachable!(),
        }
        // A transient native flag is latched: later deaths and due saves still
        // cannot erase either the unsaved extra leaf or its resume identity.
        server.host_shutdown_probe = || false;
        assert!(!server.handle_internal_event_with_forwarding(agent_exit()));
        assert!(!server.handle_internal_event_with_forwarding(death()));
        assert!(
            !server.handle_internal_event_with_forwarding(AppEvent::PaneDied {
                pane_id: other,
                exit_reason: crate::platform::ChildExitReason::WaitFailed,
            })
        );
        assert!(server.should_quit.load(Ordering::Acquire));
        assert_eq!(server.app.terminal_runtimes.len(), 0);
        server.app.session_save_deadline = Some(Instant::now());
        assert!(
            !server.handle_scheduled_tasks_headless(Instant::now() + Duration::from_secs(6), false)
        );
        assert!(server.app.session_save_thread.is_none());
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), server.run())
                .await
                .unwrap()
                .unwrap();
        });
        let saved = crate::persist::load().unwrap();
        assert_eq!(saved.workspaces[0].tabs[0].panes.len(), 2);
        assert_eq!(saved.workspaces[0].custom_name.as_deref(), Some("shutdown"));
        assert_eq!(
            saved.workspaces[0].tabs[0].panes[&pane.raw()]
                .agent_session
                .as_ref()
                .unwrap()
                .value,
            "shutdown-session"
        );
        server.app.state.assert_invariants_for_test();
        shutdown_test_runtimes(&mut server);
    }
    // Without a host warning, ordinary child death still removes only that leaf.
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("ordinary-exit");
    let pane = workspace.tabs[0].root_pane;
    let other = workspace.test_split(ratatui::layout::Direction::Horizontal);
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    assert!(
        server.handle_internal_event_with_forwarding(AppEvent::PaneDied {
            pane_id: pane,
            exit_reason: crate::platform::ChildExitReason::Exited,
        })
    );
    assert!(server.app.find_pane(pane).is_none());
    assert!(server.app.find_pane(other).is_some());
    assert!(!server.should_quit.load(Ordering::Acquire));
    shutdown_test_runtimes(&mut server);
    std::fs::remove_dir_all(root).unwrap();
}
