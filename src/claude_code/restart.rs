//! Guarded one-child replacement. Configuration is private; public arguments contain no secrets.
use super::state::State;
use crate::provider_client::ProviderError;
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, time::Instant};

pub struct Config {
    pub helper: Vec<String>,
    pub native: Vec<String>,
    pub cwd: PathBuf,
    pub environment: HashMap<String, String>,
}
impl Config {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.helper.len() < 2
            || self.helper.len() > 128
            || self.native.is_empty()
            || !std::path::Path::new(&self.helper[0]).is_absolute()
            || !std::path::Path::new(&self.helper[1]).is_absolute()
            || self
                .helper
                .iter()
                .any(|s| s.is_empty() || s.len() > 8192 || s.contains('\0'))
            || self.helper.iter().map(String::len).sum::<usize>() > 65536
        {
            return Err("restart helper requires bounded private absolute interpreter/helper argv");
        }
        Ok(())
    }
    fn helper_args(&self, action: &str, request: &Restart) -> Vec<String> {
        let mut args = self.helper.clone();
        args.extend([
            action.into(),
            "--handle".into(),
            request.reference.clone(),
            "--provider".into(),
            "anthropic".into(),
            "--kind".into(),
            "claude_subscription".into(),
        ]);
        args
    }
    pub fn launch(&self, request: &Restart) -> Vec<String> {
        let mut args = self.helper_args("launch", request);
        args.push("--".into());
        args.extend(self.native.clone());
        args.extend(["--resume".into(), request.session.clone()]);
        args
    }
    pub async fn preflight(
        &self,
        request: &Restart,
        deadline: Instant,
    ) -> Result<(), ProviderError> {
        let deadline = deadline.min(Instant::now() + Duration::from_secs(35));
        if Instant::now() >= deadline {
            return Err(ProviderError::new(
                "unavailable",
                "private preflight deadline before dispatch; no owned stop dispatched",
            ));
        }
        let argv = self.helper_args("check", request);
        let mut command = crate::program::background_command(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::native_process::native_env(&mut command, &self.environment);
        let mut child = command.spawn().map_err(|_| {
            ProviderError::new("unavailable", "private restart preflight could not start")
        })?;
        let mut stdout = child
            .stdout
            .take()
            .expect("private preflight pipe")
            .take(1025);
        let result = tokio::time::timeout_at(deadline, async {
            let mut bytes = Vec::new();
            let (status, _) = tokio::try_join!(child.wait(), stdout.read_to_end(&mut bytes))?;
            Ok::<_, std::io::Error>((status, bytes))
        })
        .await;
        match result {
            Ok(Ok((status, bytes)))
                if status.success()
                    && bytes.len() <= 1024
                    && serde_json::from_slice::<Value>(&bytes).ok()
                        == Some(json!({"status":"ready"})) =>
            {
                Ok(())
            }
            _ => Err(ProviderError::new(
                "unavailable",
                "private restart preflight not confirmed ready; no owned stop dispatched",
            )),
        }
    }
}

pub struct Restart {
    pub epoch: String,
    pub session: String,
    pub account: String,
    pub episode: String,
    pub target: String,
    pub reference: String,
    pub host: String,
    pub revision: u64,
    pub allow_background_loss: bool,
}
fn text(value: &Value, max: usize) -> Result<String, &'static str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.contains('\0'))
        .map(str::to_owned)
        .ok_or("restart identity must be bounded text")
}
impl Restart {
    pub fn parse(args: &Value) -> Result<Self, &'static str> {
        let row = args
            .as_object()
            .ok_or("restart arguments must be an object")?;
        if args["v"] != 0
            || row.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "v" | "epoch"
                        | "session"
                        | "account"
                        | "episode"
                        | "target"
                        | "allow_background_loss"
                )
            })
        {
            return Err("invalid restart version/field");
        }
        let target = args["target"]
            .as_object()
            .ok_or("restart target required")?;
        if target
            .keys()
            .any(|k| !matches!(k.as_str(), "account" | "ref" | "host" | "revision"))
        {
            return Err("undeclared restart target field");
        }
        let reference = text(&args["target"]["ref"], 32)?;
        if reference.len() != 32 || !reference.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("restart target requires opaque32hex reference");
        }
        let allow_background_loss = match row.get("allow_background_loss") {
            None => false,
            Some(Value::Bool(v)) => *v,
            _ => return Err("allow_background_loss must be boolean"),
        };
        let result = Self {
            epoch: text(&args["epoch"], 256)?,
            session: text(&args["session"], 128)?,
            account: text(&args["account"], 64)?,
            episode: text(&args["episode"], 128)?,
            target: text(&args["target"]["account"], 64)?,
            reference,
            host: text(&args["target"]["host"], 4096)?,
            revision: args["target"]["revision"]
                .as_u64()
                .ok_or("target registry revision required")?,
            allow_background_loss,
        };
        if [&result.account, &result.target].iter().any(|id| {
            !id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        }) {
            return Err("configured account identifier is invalid");
        }
        if result.session.starts_with('-') || result.session.chars().any(char::is_control) {
            return Err("native session cannot be represented faithfully as a resume argument");
        }
        if result.account == result.target {
            return Err("restart target must differ from source account");
        }
        Ok(result)
    }
    pub fn guard(
        &self,
        state: &State,
        authenticated_host: Option<&str>,
    ) -> Result<(), &'static str> {
        if state.epoch() != self.epoch
            || state.session.as_deref() != Some(self.session.as_str())
            || state.account.as_ref().and_then(|v| v["id"].as_str()) != Some(self.account.as_str())
        {
            return Err("restart source epoch/session/account changed");
        }
        if authenticated_host != Some(self.host.as_str()) {
            return Err("restart execution host differs from authenticated provider binding");
        }
        if let Some(reason) = state.work.foreground_loss_reason() {
            return Err(match reason {
                "task list full" => {
                    "restart blocked: foreground work may be running (task list full); automatic restart refused until the process is replaced another way"
                }
                "foreground identity unavailable" => {
                    "restart blocked: foreground work may be running (foreground identity unavailable); automatic restart refused until the process is replaced another way"
                }
                "foreground generation unknown" => {
                    "restart blocked: foreground work may be running (foreground generation unknown); automatic restart refused until the process is replaced another way"
                }
                _ => {
                    "restart blocked: foreground work may be running (foreground tracking incomplete); automatic restart refused until the process is replaced another way"
                }
            });
        }
        if state.requests.scope_history_incomplete() {
            return Err(
                "restart blocked: request scope evidence was lost at request bounds; automatic restart refused until the process is replaced another way",
            );
        }
        if !state.ready
            || !state.literal_supported()
            || state.exit.is_some()
            || state.status() != "idle"
            || !state.requests.is_empty()
            || state.requests.has_unconfirmed()
            || !state.active_tools.is_empty()
            || state.work.root_foreground_active()
        {
            return Err("restart requires initialized idle owned main process");
        }
        let Some(episode) = &state.availability.limit_episode else {
            return Err("restart requires current native subscription terminal episode");
        };
        if episode["episode"] != self.episode
            || episode["session_id"] != self.session
            || episode["subscription_limit"] != true
            || episode["terminal_reason"] != "blocking_limit"
        {
            return Err("restart native subscription episode changed");
        }
        let ledger = state.input_ledger();
        if ledger["epoch"] != self.epoch || ledger["complete"] != true || ledger["pending"] != 0 {
            return Err(
                "restart requires a complete empty vqro input ledger, not a guessed native queue",
            );
        }
        if !self.allow_background_loss && !state.background_known_empty() {
            return Err(
                "native background is unknown/incomplete/running; background work may be lost",
            );
        }
        Ok(())
    }
    pub fn account_value(&self) -> Value {
        json!({"id":self.target,"provider":"anthropic","kind":"subscription","sources":{"id":"vqro:restart","provider":"vqro:restart","kind":"vqro:restart"}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (State, Value) {
        let mut state = State::new("boot".into());
        state.observe(&crate::native_process::Event::Record(
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        ));
        state.ready = true;
        state.version = Some("2.1.329".into());
        state.foreground = "idle";
        state.session = Some("native-session".into());
        state.account = Some(json!({"id":"A"}));
        state.availability.limit_episode = Some(
            json!({"episode":"native-result","session_id":"native-session","subscription_limit":true,"terminal_reason":"blocking_limit"}),
        );
        let args = json!({"v":0,"epoch":state.epoch(),"session":"native-session","account":"A","episode":"native-result","target":{"account":"B","ref":"0123456789abcdef0123456789abcdef","host":"r/host","revision":3}});
        (state, args)
    }
    #[test]
    fn positive_background_edge_revokes_stale_empty_restart_witness() {
        let (mut state, args) = fixture();
        let request = Restart::parse(&args).unwrap();
        assert!(request.guard(&state, Some("r/host")).is_ok());
        let episode = state.availability.limit_episode.clone();
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"new-bg","run_id":"1","task_type":"local_agent","subagent_type":"worker","is_backgrounded":true})));
        assert!(
            request.guard(&state, Some("r/host")).is_err(),
            "positive new background task MUST revoke stale known-empty restart authorization until a fresh full level"
        );
        assert!(state.background.is_none());
        assert_eq!(
            state.availability.limit_episode, episode,
            "child activity must not rewrite root rate/turn evidence"
        );
        state.observe(&crate::native_process::Event::Record(
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        ));
        assert!(
            request.guard(&state, Some("r/host")).is_ok(),
            "only fresh native full membership can restore known empty"
        );
    }
    #[test]
    fn covered_level_and_late_terminal_edges_do_not_fabricate_new_background_work() {
        let (mut state, args) = fixture();
        let request = Restart::parse(&args).unwrap();
        let start = json!({"type":"system","subtype":"task_started","task_id":"a","run_id":"1","task_type":"local_agent","is_backgrounded":true});
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a","run_id":"1","task_type":"local_agent"}]})));
        state.observe(&crate::native_process::Event::Record(start.clone()));
        assert!(
            state.background.is_some(),
            "level-before-start is covered, not a contradiction"
        );
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":"a","run_id":"1","status":"completed"})));
        state.observe(&crate::native_process::Event::Record(
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        ));
        state.observe(&crate::native_process::Event::Record(start.clone()));
        assert!(
            request.guard(&state, Some("r/host")).is_ok(),
            "same observed terminal run cannot reopen work"
        );
        let mut newer = start;
        newer["run_id"] = json!("2");
        state.observe(&crate::native_process::Event::Record(newer));
        assert!(request.guard(&state, Some("r/host")).is_err());
    }
    #[test]
    fn slice3_known_direct_root_foreground_work_refuses_with_both_loss_values() {
        let (mut state, mut args) = fixture();
        let episode = state.availability.limit_episode.clone();
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"root-foreground","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"is_backgrounded":false})));
        for loss in [false, true] {
            args["allow_background_loss"] = json!(loss);
            assert!(
                Restart::parse(&args)
                    .unwrap()
                    .guard(&state, Some("r/host"))
                    .is_err(),
                "known DIRECT ROOT foreground work MUST refuse before stop; background loss cannot bypass it"
            );
        }
        assert_eq!(state.status(), "idle");
        assert_eq!(state.background, Some(vec![]));
        assert_eq!(state.availability.limit_episode, episode);
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":"root-foreground","run_id":"1","status":"completed"})));
        assert!(
            Restart::parse(&args)
                .unwrap()
                .guard(&state, Some("r/host"))
                .is_ok()
        );
    }
    #[test]
    fn nested_foreground_under_known_background_is_not_inferred_as_root_work() {
        let (mut state, mut args) = fixture();
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"bg-parent","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"is_backgrounded":true})));
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"bg-parent","run_id":"1","task_type":"local_agent","subagent_type":"worker"}]})));
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"nested-foreground","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":2,"parent_task_id":"bg-parent","is_backgrounded":false})));
        assert!(!state.work.root_foreground_active());
        args["allow_background_loss"] = json!(true);
        assert!(
            Restart::parse(&args)
                .unwrap()
                .guard(&state, Some("r/host"))
                .is_ok()
        );
    }
    #[test]
    fn foreground_overflow_keeps_restart_fenced_for_both_loss_values() {
        let (mut state, mut args) = fixture();
        for n in 0..super::super::work::TASKS {
            state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":format!("history-{n}"),"run_id":"1","status":"completed"})));
        }
        state.observe(&crate::native_process::Event::Record(
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        ));
        for loss in [false, true] {
            args["allow_background_loss"] = json!(loss);
            assert!(
                Restart::parse(&args)
                    .unwrap()
                    .guard(&state, Some("r/host"))
                    .is_ok(),
                "historical terminal rows alone are not live work"
            );
        }
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"new-fg-129","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"is_backgrounded":false})));
        for loss in [false, true] {
            args["allow_background_loss"] = json!(loss);
            assert!(
                Restart::parse(&args)
                    .unwrap()
                    .guard(&state, Some("r/host"))
                    .is_err(),
                "unrepresented known ROOT foreground work MUST retain the stop fence despite terminal history capacity"
            );
        }
        assert_eq!(state.background, Some(vec![]));
        assert_eq!(state.status(), "idle");
        let notice = state
            .journal
            .entity("notice", "foreground-work-loss")
            .unwrap()
            .1;
        let text = notice["text"].as_str().unwrap();
        assert!(text.contains("restart blocked: foreground work may be running (task list full)"));
        assert!(text.contains("replaced another way"));
        assert!(text.len() < 512);
        state.observe(&crate::native_process::Event::Record(
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        ));
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":"new-fg-129","run_id":"1","status":"completed"})));
        assert!(
            state.work.root_foreground_active(),
            "a BG level or one unmatched terminal cannot clear an uncounted foreground loss witness"
        );
        state.observe(&crate::native_process::Event::Record(
            json!({"type":"conversation_reset"}),
        ));
        assert!(
            state.work.root_foreground_active(),
            "same-process conversation reset is not process quiescence"
        );
        state.replace_owned(json!({"id":"B"}), 2);
        assert!(
            !state.work.root_foreground_active(),
            "new owned native process clears the bounded witness"
        );
    }
    #[test]
    fn terminal_history_capacity_alone_and_background_overflow_do_not_invent_root_foreground() {
        let (mut state, mut args) = fixture();
        for n in 0..super::super::work::TASKS {
            state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":format!("old-{n}"),"run_id":"1","status":"completed"})));
        }
        state.observe(&crate::native_process::Event::Record(
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
        ));
        assert!(!state.work.root_foreground_active());
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"new-bg-129","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"is_backgrounded":true})));
        assert!(!state.work.root_foreground_active());
        args["allow_background_loss"] = json!(true);
        assert!(
            Restart::parse(&args)
                .unwrap()
                .guard(&state, Some("r/host"))
                .is_ok()
        );
    }
    #[test]
    fn missing_run_foreground_evidence_cannot_reuse_a_terminal_row_as_quiescence() {
        for run in [None, Some("x".repeat(129))] {
            let (mut state, mut args) = fixture();
            state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":"same-task","run_id":"1","status":"completed"})));
            let mut frame = json!({"type":"system","subtype":"task_started","task_id":"same-task","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"is_backgrounded":false});
            if let Some(run) = run {
                frame["run_id"] = json!(run);
            }
            state.observe(&crate::native_process::Event::Record(frame));
            for loss in [false, true] {
                args["allow_background_loss"] = json!(loss);
                assert!(
                    Restart::parse(&args)
                        .unwrap()
                        .guard(&state, Some("r/host"))
                        .is_err(),
                    "missing/unrepresentable run cannot prove a positively known foreground start is a late terminal run"
                );
            }
            assert_eq!(
                state.work.foreground_loss_reason(),
                Some("foreground generation unknown")
            );
        }
    }
    #[test]
    fn unrepresentable_foreground_identity_has_specific_recovery_refusal() {
        let (mut state, mut args) = fixture();
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"x".repeat(129),"run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"is_backgrounded":false})));
        for loss in [false, true] {
            args["allow_background_loss"] = json!(loss);
            let reason = Restart::parse(&args)
                .unwrap()
                .guard(&state, Some("r/host"))
                .unwrap_err();
            assert!(
                reason.contains("foreground identity unavailable")
                    && reason.contains("process is replaced another way")
            );
        }
    }
    #[test]
    fn background_override_defaults_false_and_bypasses_only_background() {
        let (mut state, mut args) = fixture();
        let ordinary = Restart::parse(&args).unwrap();
        assert!(!ordinary.allow_background_loss);
        assert!(ordinary.guard(&state, Some("r/host")).is_ok());
        state.background = None;
        assert!(ordinary.guard(&state, Some("r/host")).is_err());
        args["allow_background_loss"] = json!(true);
        let allowed = Restart::parse(&args).unwrap();
        assert!(allowed.guard(&state, Some("r/host")).is_ok());
        state.observe(&crate::native_process::Event::Record(json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"id":"native-running"}]})));
        assert!(ordinary.guard(&state, Some("r/host")).is_err());
        assert!(allowed.guard(&state, Some("r/host")).is_ok());
        let epoch = state.epoch().to_owned();
        state
            .inputs
            .insert("unknown-input".into(), ("delivery".into(), epoch));
        state.save_receipt("delivery", json!({"stage":"unknown"}));
        assert!(
            allowed.guard(&state, Some("r/host")).is_err(),
            "loss flag does not bypass own input ledger"
        );
    }
    #[test]
    fn all_source_and_host_guards_are_independent_of_background_override() {
        for field in ["epoch", "session", "account", "episode"] {
            let (state, mut args) = fixture();
            args["allow_background_loss"] = json!(true);
            args[field] = json!("changed");
            assert!(
                Restart::parse(&args)
                    .unwrap()
                    .guard(&state, Some("r/host"))
                    .is_err()
            );
        }
        let (mut state, mut args) = fixture();
        args["allow_background_loss"] = json!(true);
        let request = Restart::parse(&args).unwrap();
        assert!(request.guard(&state, Some("process-display-host")).is_err());
        assert!(request.guard(&state, None).is_err());
        state.foreground = "running";
        assert!(request.guard(&state, Some("r/host")).is_err());
    }
    #[test]
    fn public_arguments_cannot_supply_helper_environment_or_credentials() {
        let (_, args) = fixture();
        for (field, value) in [
            ("helper", json!(["arbitrary"])),
            ("env", json!({})),
            ("claim", json!("policy")),
            ("allow_background_loss", json!("true")),
        ] {
            let mut bad = args.clone();
            bad[field] = value;
            assert!(Restart::parse(&bad).is_err());
        }
        let mut bad = args.clone();
        bad["target"]["ref"] = json!("not-a-handle");
        assert!(Restart::parse(&bad).is_err());
        for id in ["bad account".to_owned(), "x".repeat(65)] {
            let mut bad = args.clone();
            bad["target"]["account"] = json!(id);
            assert!(Restart::parse(&bad).is_err());
        }
        let mut bad = args.clone();
        bad["session"] = json!("--not-a-session-value");
        assert!(Restart::parse(&bad).is_err());
        let account = Restart::parse(&args).unwrap().account_value();
        assert!(account.get("label").is_none());
        assert_eq!(account["sources"]["id"], "vqro:restart");
    }
    #[tokio::test]
    async fn expired_preflight_refuses_before_helper_dispatch() {
        let (_, args) = fixture();
        let request = Restart::parse(&args).unwrap();
        let absent = std::env::temp_dir()
            .join("absent-synthetic-restart-interpreter")
            .to_string_lossy()
            .into_owned();
        let config = Config {
            helper: vec![absent.clone(), absent],
            native: vec!["synthetic".into()],
            cwd: PathBuf::new(),
            environment: HashMap::new(),
        };
        let error = config
            .preflight(&request, Instant::now() - Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.message.contains("deadline before dispatch"));
    }
    #[test]
    fn helper_wiring_keeps_original_native_settings_without_new_launch_limits() {
        let (_, args) = fixture();
        let request = Restart::parse(&args).unwrap();
        let native = vec![
            "claude".into(),
            "--append-system-prompt".into(),
            "x".repeat(32768),
        ];
        let config = Config {
            helper: vec![
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ],
            native: native.clone(),
            cwd: PathBuf::new(),
            environment: HashMap::new(),
        };
        assert!(config.validate().is_ok());
        let argv = config.launch(&request);
        let at = argv.iter().position(|s| s == "--").unwrap();
        assert_eq!(&argv[at + 1..argv.len() - 2], native.as_slice());
        assert_eq!(&argv[argv.len() - 2..], ["--resume", "native-session"]);
        assert!(!argv.iter().any(|s| s == "--bare" || s == "--fork-session"));
    }
    #[test]
    fn owned_replacement_advances_epoch_and_does_not_fake_requested_session() {
        let (mut state, _) = fixture();
        let epoch = state.epoch().to_owned();
        state.restart_configured = true;
        state.replace_owned(json!({"id":"B"}), 2);
        assert_ne!(state.epoch(), epoch);
        assert!(state.session.is_none());
        assert!(!state.ready);
        assert!(!state.background_known_empty());
        assert_eq!(state.generation, 2);
        assert_eq!(state.account.as_ref().unwrap()["id"], "B");
    }
}
