//! One backend-owned Claude child, native effects, and public provider SDK plumbing.
use super::{contract, state::State, wire::ClaudeCodec};
use crate::native_process::{Error as NativeError, Process};
use crate::provider_client::items::{ItemCursor, Next};
use crate::provider_client::{
    EffectRequest, Outcome, Provider, ProviderError, ProviderStream, Request,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

pub struct Shared {
    pub state: Mutex<State>,
    process: Mutex<Option<(u64, Process)>>,
    restart_config: Mutex<Option<Arc<super::restart::Config>>>,
    lifetime: Mutex<Option<tokio::sync::watch::Receiver<bool>>>,
    pub changed: Notify,
    outgoing: Notify,
    effects: tokio::sync::Mutex<()>,
    #[cfg(test)]
    spawn_window: Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
    generation: AtomicU64,
    restart_in_progress: std::sync::atomic::AtomicBool,
    viewers: AtomicUsize,
    viewers_changed: Notify,
    retirement_failed: std::sync::atomic::AtomicBool,
    stdout_done: std::sync::atomic::AtomicBool,
    background_probe: std::sync::atomic::AtomicBool,
    spawned: std::sync::atomic::AtomicBool,
    pub name: String,
    pub instance: String,
}
#[cfg(test)]
#[path = "runtime_restart_tests.rs"]
mod restart_tests;

struct RestartGuard<'a>(&'a Shared);
impl Drop for RestartGuard<'_> {
    fn drop(&mut self) {
        self.0.restart_in_progress.store(false, Ordering::SeqCst);
        self.0.wake();
    }
}

fn rejected(kind: &str, message: &str) -> Result<Outcome, ProviderError> {
    Ok(Outcome::Rejected {
        kind: kind.into(),
        message: message.into(),
    })
}
fn native_failure(error: NativeError) -> Result<Outcome, ProviderError> {
    if error.may_have_acted {
        Err(ProviderError::new("backend", error.to_string()))
    } else {
        rejected(error.kind, &error.message)
    }
}
fn observed_exit(exit: Option<crate::native_process::Exit>) -> bool {
    exit.is_some_and(|exit| exit.observed)
}
fn uuid() -> Result<String, ProviderError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| ProviderError::new("backend", "cannot allocate native client identifier"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
impl Shared {
    pub fn new(boot: String, name: String, instance: String) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new(boot)),
            process: Mutex::new(None),
            restart_config: Mutex::new(None),
            lifetime: Mutex::new(None),
            changed: Notify::new(),
            outgoing: Notify::new(),
            effects: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            spawn_window: Mutex::new(None),
            generation: AtomicU64::new(1),
            restart_in_progress: std::sync::atomic::AtomicBool::new(false),
            viewers: AtomicUsize::new(0),
            viewers_changed: Notify::new(),
            retirement_failed: std::sync::atomic::AtomicBool::new(false),
            stdout_done: std::sync::atomic::AtomicBool::new(false),
            background_probe: std::sync::atomic::AtomicBool::new(false),
            spawned: std::sync::atomic::AtomicBool::new(false),
            name,
            instance,
        })
    }
    pub fn restarting(&self) -> bool {
        self.restart_in_progress.load(Ordering::SeqCst)
    }
    pub fn process(&self) -> Option<Process> {
        self.process
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(generation, _)| *generation == self.generation.load(Ordering::SeqCst))
            .map(|(_, process)| process.clone())
    }
    pub fn configure_restart(&self, config: super::restart::Config) -> Result<(), &'static str> {
        config.validate()?;
        *self.restart_config.lock().unwrap() = Some(Arc::new(config));
        let mut state = self.state.lock().unwrap();
        state.restart_configured = true;
        state.refresh();
        Ok(())
    }
    pub fn set_lifetime(&self, lifetime: tokio::sync::watch::Receiver<bool>) {
        *self.lifetime.lock().unwrap() = Some(lifetime);
    }
    fn publish_lifetime(&self, sender: &tokio::sync::watch::Sender<bool>) {
        sender.send_replace(true);
        if let Some(native) = self.process() {
            native.stop();
        }
        self.wake();
    }
    /// OS stdin thread only. Publication is ordered by the EXISTING native admission fence.
    /// If dispatch owns admission, it finishes bringing up the process first; publication then
    /// stops that current owned process. An observed lifetime winner forbids subsequent dispatch.
    pub fn end_lifetime_blocking(&self, sender: &tokio::sync::watch::Sender<bool>) {
        let _gate = self.effects.blocking_lock();
        self.publish_lifetime(sender);
    }
    /// Runtime signal task counterpart; never block a runtime worker with blocking_lock.
    pub async fn end_lifetime(&self, sender: &tokio::sync::watch::Sender<bool>) {
        let _gate = self.effects.lock().await;
        self.publish_lifetime(sender);
    }
    fn ending(&self) -> bool {
        self.lifetime
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|life| *life.borrow() || life.has_changed().is_err())
    }
    async fn lifetime_ending(&self) {
        let receiver = self.lifetime.lock().unwrap().clone();
        if let Some(mut receiver) = receiver {
            if !*receiver.borrow() {
                let _ = receiver.changed().await;
            }
        } else {
            std::future::pending::<()>().await;
        }
    }
    pub fn facts(&self) -> Value {
        self.state.lock().unwrap().facts(&self.name, &self.instance)
    }
    pub fn wake(&self) {
        self.changed.notify_waiters();
        self.outgoing.notify_one();
    }
    pub async fn spawn_native(
        self: &Arc<Self>,
        argv: &[String],
        cwd: &std::path::Path,
        env: &HashMap<String, String>,
    ) -> Result<Process, NativeError> {
        let _gate = self.effects.lock().await;
        self.spawn_native_admitted(argv, cwd, env).await
    }
    // Restart's invoke holds the SAME admission guard until replacement initialization finishes.
    async fn spawn_native_admitted(
        self: &Arc<Self>,
        argv: &[String],
        cwd: &std::path::Path,
        env: &HashMap<String, String>,
    ) -> Result<Process, NativeError> {
        if self.ending() {
            return Err(NativeError {
                kind: "unavailable",
                message: "bridge lifetime ended before dispatch".into(),
                may_have_acted: false,
            });
        }
        #[cfg(test)]
        if let Some((entered, release)) = self.spawn_window.lock().unwrap().take() {
            entered.wait();
            release.wait();
        }
        if self.spawned.swap(true, Ordering::SeqCst) {
            return Err(NativeError {
                kind: "conflict",
                message: "one owned native slot is already reserved".into(),
                may_have_acted: false,
            });
        }
        let weak = Arc::downgrade(self);
        let generation = self.generation.load(Ordering::SeqCst);
        let mut env = env.clone();
        env.remove("CLAUDE_CODE_RESUME_INTERRUPTED_TURN");
        env.remove("CLAUDE_CODE_RESUME_REASON");
        let process = Process::spawn(
            argv,
            cwd,
            &env,
            Arc::new(ClaudeCodec),
            Arc::new(move |event| {
                if let Some(shared) = weak.upgrade() {
                    shared.observe_native(generation, &event);
                }
            }),
        )
        .await?;
        *self.process.lock().unwrap() = Some((generation, process.clone()));
        self.wake();
        Ok(process)
    }
    fn observe_native(&self, generation: u64, event: &crate::native_process::Event) {
        let mut state = self.state.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        state.observe(event);
        drop(state);
        self.wake();
    }
    pub async fn initialize(self: Arc<Self>) {
        let generation = self.generation.load(Ordering::SeqCst);
        let Some(native) = self.process() else {
            return;
        };
        if !self.initialize_primary(&native, generation).await {
            return;
        }
        self.probe_background(&native, generation).await;
    }
    async fn initialize_primary(&self, native: &Process, generation: u64) -> bool {
        // No prompt, model request, transcript scan, custom system prompt or settings-source override.
        let result = native
            .call(
                "initialize",
                json!({"supportedDialogKinds":[]}),
                Duration::from_secs(15),
            )
            .await;
        let ready = {
            let mut state = self.state.lock().unwrap();
            if self.generation.load(Ordering::SeqCst) != generation {
                return false;
            }
            match result {
                Ok(reply) if reply.success => state.initialized(&reply.data),
                _ => {
                    state.incomplete.insert("initialize".into());
                    state.ready = false;
                    state.refresh();
                }
            }
            state.ready
        };
        self.wake();
        ready
    }
    async fn probe_background(&self, native: &Process, generation: u64) {
        let probe = {
            let state = self.state.lock().unwrap();
            self.generation.load(Ordering::SeqCst) == generation
                && state.literal_supported()
                && !self.background_probe.swap(true, Ordering::SeqCst)
        };
        if probe {
            // The pinned native protocol emits a COMPLETE background level behind repeated
            // initialize, even when empty. Until that FRAME arrives, background remains unknown.
            // This is metadata/control only, never a user prompt or input replay.
            let _ = native
                .call(
                    "initialize",
                    json!({"supportedDialogKinds":[]}),
                    Duration::from_secs(15),
                )
                .await;
            self.wake();
        }
    }
    /// Unsupported native questions are cancelled once where that protocol has a cancellation response.
    /// A failed/unknown write is reported, never retried on the next wake or reconnect.
    pub async fn drain_outgoing(self: Arc<Self>) {
        loop {
            let changed = self.outgoing.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let slot = self.process.lock().unwrap().clone();
            let Some((generation, native)) = slot else {
                changed.await;
                continue;
            };
            let response = {
                let mut state = self.state.lock().unwrap();
                if self.generation.load(Ordering::SeqCst) == generation {
                    state.outgoing.pop_front()
                } else {
                    None
                }
            };
            if let Some(response) = response {
                if native
                    .notify_guarded(
                        "control_response",
                        response.frame,
                        Instant::now() + Duration::from_secs(3),
                        response.token,
                    )
                    .await
                    .is_err()
                {
                    let mut state = self.state.lock().unwrap();
                    if self.generation.load(Ordering::SeqCst) != generation {
                        continue;
                    }
                    state.incomplete.insert("requests".into());
                    state.refresh();
                    drop(state);
                    self.changed.notify_waiters();
                }
            } else {
                changed.await;
            }
        }
    }
    pub async fn invoke(
        self: &Arc<Self>,
        provider: &Provider,
        effect: &EffectRequest,
    ) -> Result<Outcome, ProviderError> {
        {
            let state = self.state.lock().unwrap();
            if effect.r#ref.as_deref() != Some("agent") || effect.incarnation != state.incarnation()
            {
                return rejected("stale", "agent resource incarnation changed");
            }
            if let Err(e) = contract::common(&effect.args, &state) {
                return rejected(e.kind, e.message);
            }
        }
        if effect.op == "agent.receipt" {
            let delivery = match contract::id(&effect.args["delivery"]) {
                Ok(id) => id,
                Err(e) => return rejected(e.kind, e.message),
            };
            return Ok(Outcome::Observed(Some(
                self.state.lock().unwrap().receipt(delivery),
            )));
        }
        let _gate = match tokio::time::timeout_at(effect.deadline, self.effects.lock()).await {
            Ok(gate) => gate,
            Err(_) => return rejected("timeout", "deadline before native dispatch"),
        };
        if self.ending() {
            return rejected(
                "unavailable",
                "bridge lifetime ended before native admission",
            );
        }
        let Some(native) = self.process() else {
            return rejected("unavailable", "native process is not present");
        };
        // Revalidate after queueing behind another effect, before any native write or request claim.
        {
            let state = self.state.lock().unwrap();
            if let Err(e) = contract::common(&effect.args, &state) {
                return rejected(e.kind, e.message);
            }
        }
        match effect.op.as_str() {
            "claude.restart" => self.restart_owned(provider, effect, &native).await,
            "agent.send" => {
                let owner = provider
                    .call("workspace.get", json!({"key":"doc/principals/owner"}))
                    .await
                    .unwrap_or(Value::Null);
                let uuid = match uuid() {
                    Ok(id) => id,
                    Err(_) => return rejected("backend", "native client identifier unavailable"),
                };
                let prepared = {
                    let mut state = self.state.lock().unwrap();
                    let prepared = match contract::send(
                        &effect.args,
                        &state,
                        &effect.attribution,
                        &owner,
                        uuid,
                    ) {
                        Ok(p) => p,
                        Err(e) => return rejected(e.kind, e.message),
                    };
                    state.availability.begin_turn();
                    state.inputs.insert(
                        prepared.uuid.clone(),
                        (prepared.delivery.clone(), prepared.epoch.clone()),
                    );
                    state.save_receipt(&prepared.delivery, prepared.receipt.clone());
                    prepared
                };
                self.wake();
                let result = native
                    .notify("user", prepared.native, effect.deadline)
                    .await;
                let mut state = self.state.lock().unwrap();
                let mut receipt = state.receipt(&prepared.delivery);
                match &result {
                    Ok(()) => {
                        if receipt["stage"] == "unknown" {
                            receipt["stage"] = json!("dispatched");
                        }
                    }
                    Err(e) if !e.may_have_acted => {
                        receipt["stage"] = json!("rejected");
                        state.inputs.remove(&prepared.uuid);
                    }
                    Err(_) => {}
                }
                // Native echo/result can win the write-completion race. Never downgrade it or rewrite
                // the internally frozen origin/may_transform snapshot after ACK or an epoch change.
                state.save_receipt(&prepared.delivery, receipt.clone());
                drop(state);
                self.wake();
                match result {
                    Ok(()) => Ok(Outcome::Dispatched(Some(json!({"receipt":receipt})))),
                    Err(error) => native_failure(error),
                }
            }
            "agent.respond" => {
                if effect.args.get("epoch").is_none() {
                    return rejected("invalid", "request observation epoch is required");
                }
                let request = match contract::id(&effect.args["request"]) {
                    Ok(id) => id.to_owned(),
                    Err(e) => return rejected(e.kind, e.message),
                };
                let Some(revision) = effect.args["revision"].as_u64() else {
                    return rejected("invalid", "request revision is required");
                };
                let (response, token) = {
                    let mut state = self.state.lock().unwrap();
                    let response =
                        match state
                            .requests
                            .answer(&request, revision, &effect.args["response"])
                        {
                            Ok(r) => r,
                            Err(e) => return rejected(e.kind, e.message),
                        };
                    let token = state
                        .requests
                        .answer_token(&request)
                        .expect("claimed native request has token");
                    (response, token)
                };
                let result = native
                    .notify_guarded("control_response", response, effect.deadline, token)
                    .await;
                let mut state = self.state.lock().unwrap();
                match &result {
                    Err(e) if !e.may_have_acted => state.requests.not_written(&request),
                    result => {
                        if let Some(change) = state.requests.dispatched(&request, result.is_ok()) {
                            state.put("request", &change.id, change.value);
                        }
                    }
                }
                state.refresh();
                drop(state);
                self.wake();
                match result {
                    Ok(()) => Ok(Outcome::Dispatched(None)),
                    Err(error) => native_failure(error),
                }
            }
            "agent.interrupt" => {
                let remaining = effect.deadline.saturating_duration_since(Instant::now());
                let result = native.call("interrupt", json!({}), remaining).await;
                match result {
                    Ok(reply) if reply.success => {
                        let mut data = json!({"pending":"unknown"});
                        for field in ["still_queued", "cancelled"] {
                            if let Some(value) = reply.data.get(field) {
                                if crate::limits::encoded_bytes(value) <= 8192 {
                                    data[field] = value.clone();
                                }
                            }
                        }
                        Ok(Outcome::Accepted(Some(data)))
                    }
                    Ok(_) => Err(ProviderError::new(
                        "backend",
                        "native interrupt returned an error; outcome not upgraded to no-effect",
                    )),
                    Err(error) => native_failure(error),
                }
            }
            _ => rejected("unsupported", "agent operation is not declared"),
        }
    }
    async fn restart_owned(
        self: &Arc<Self>,
        provider: &Provider,
        effect: &EffectRequest,
        native: &Process,
    ) -> Result<Outcome, ProviderError> {
        let request = match super::restart::Restart::parse(&effect.args) {
            Ok(v) => v,
            Err(e) => return rejected("invalid", e),
        };
        let config = match self.restart_config.lock().unwrap().clone() {
            Some(v) => v,
            None => return rejected("unsupported", "private restart helper is not configured"),
        };
        let host = provider.registration()["host"].as_str(); // authenticated registration, same binding as snapshot.providers[name].host
        {
            let state = self.state.lock().unwrap();
            if let Err(e) = request.guard(&state, host) {
                return rejected("stale", e);
            }
        }
        self.restart_in_progress.store(true, Ordering::SeqCst);
        let _restarting = RestartGuard(self);
        self.wake();
        let preflight = tokio::select! {biased;_=self.lifetime_ending()=>return rejected("unavailable","bridge lifetime ended before owned stop"),result=config.preflight(&request,effect.deadline)=>result};
        if preflight.is_err() {
            return rejected(
                "unavailable",
                "private preflight not confirmed ready; no owned stop dispatched",
            );
        }
        {
            let mut state = self.state.lock().unwrap();
            if let Err(e) = request.guard(&state, host) {
                return rejected("stale", e);
            }
            if self.ending() {
                return rejected("unavailable", "bridge lifetime ended before owned stop");
            }
            if Instant::now() >= effect.deadline {
                return rejected("timeout", "deadline before owned stop");
            }
            // The admission mutex is held by invoke. Recheck-and-stop while holding the same state
            // lock as native observations; consume the episode before any later retry can reuse it.
            state.availability.invalidate_episode();
            state.ready = false;
            state.refresh();
            native.stop();
        }
        self.wake();
        if !observed_exit(
            tokio::time::timeout_at(
                effect.deadline.min(Instant::now() + Duration::from_secs(5)),
                native.exited(),
            )
            .await
            .ok(),
        ) {
            return Err(ProviderError::new(
                "backend",
                "restart unknown: old owned exit unconfirmed; no new exec and no retry",
            ));
        }
        if self.ending() {
            return Err(ProviderError::new(
                "backend",
                "old process stopped; bridge lifetime ended, no replacement",
            ));
        }
        let tail = tokio::time::timeout_at(
            effect.deadline.min(Instant::now() + Duration::from_secs(2)),
            native.drained(),
        )
        .await;
        let generation = {
            let mut state = self.state.lock().unwrap();
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            state.replace_owned(request.account_value(), generation);
            if !matches!(tail, Ok(crate::native_process::Drain::Eof)) {
                state.incomplete.insert("prior_native_tail".into());
            }
            generation
        };
        self.background_probe.store(false, Ordering::SeqCst);
        self.spawned.store(false, Ordering::SeqCst);
        self.wake();
        if self.ending() {
            return Err(ProviderError::new(
                "backend",
                "old process stopped; bridge lifetime ended, no replacement",
            ));
        }
        if Instant::now() >= effect.deadline {
            return Err(ProviderError::new(
                "backend",
                "old owned process stopped; deadline before replacement, no retry",
            ));
        }
        let launch = config.launch(&request);
        let next = self
            .spawn_native_admitted(&launch, &config.cwd, &config.environment)
            .await
            .map_err(|_| {
                ProviderError::new(
                    "backend",
                    "old process stopped; replacement launch not confirmed, no retry",
                )
            })?;
        let initialized = tokio::select! {result=tokio::time::timeout_at(effect.deadline,self.initialize_primary(&next,generation))=>result.unwrap_or(false),_=self.lifetime_ending()=>{next.stop();false}};
        if !initialized {
            return Err(ProviderError::new(
                "backend",
                "replacement control initialization not confirmed; no retry or input replay",
            ));
        }
        // Best-effort metadata level only, bounded by the SAME effect deadline. No extra task or
        // supervisor survives the request; unknown background still refuses future default restart.
        tokio::select! {_=tokio::time::timeout_at(effect.deadline,self.probe_background(&next,generation))=>{},_=self.lifetime_ending()=>{next.stop();return Err(ProviderError::new("backend","bridge lifetime ended after replacement start"));}};
        if self.ending() {
            next.stop();
            return Err(ProviderError::new(
                "backend",
                "bridge lifetime ended after replacement start",
            ));
        }
        let data = {
            let state = self.state.lock().unwrap();
            if !state.ready
                || !next.alive()
                || state
                    .session
                    .as_deref()
                    .is_some_and(|id| id != request.session)
            {
                return Err(ProviderError::new(
                    "backend",
                    "replacement refused initialization/resume or observed a different session",
                ));
            }
            let mut data = json!({"started":true,"requested_session":request.session,"resume_confirmed":state.session.as_deref()==Some(request.session.as_str()),"account":request.target,"epoch":state.epoch()});
            if let Some(session) = &state.session {
                data["session"] = json!(session);
            }
            data
        };
        self.wake();
        if data["resume_confirmed"] == true {
            Ok(Outcome::Observed(Some(data)))
        } else {
            Ok(Outcome::Accepted(Some(data)))
        }
    }
    pub fn open(self: &Arc<Self>, open: crate::provider_client::OpenRequest) {
        let valid = {
            let state = self.state.lock().unwrap();
            open.stream.r#ref == "agent"
                && open.stream.incarnation == state.incarnation()
                && open.items.is_some()
        };
        if !valid {
            open.respond(Err(ProviderError::new(
                "unsupported",
                "one agent item channel only",
            )));
            return;
        }
        let cursor = self
            .state
            .lock()
            .unwrap()
            .journal
            .open(open.items.as_ref().and_then(|i| i.get("after")));
        if let Some(stream) = open.respond_items(cursor.reset(), cursor.epoch()) {
            self.viewers.fetch_add(1, Ordering::SeqCst);
            let shared = self.clone();
            tokio::spawn(async move {
                shared.viewer(stream, cursor).await;
            });
        }
    }
    async fn viewer(self: Arc<Self>, mut stream: ProviderStream, mut cursor: ItemCursor) {
        struct Guard(Arc<Shared>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.viewers.fetch_sub(1, Ordering::SeqCst);
                self.0.viewers_changed.notify_waiters();
            }
        }
        let _guard = Guard(self.clone());
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let next = {
                let mut state = self.state.lock().unwrap();
                cursor.next(&mut state.journal, 8192)
            };
            match next {
                Next::Data(data) => {
                    if stream.send(&data).await.is_err() {
                        break;
                    }
                }
                Next::Gap(reason) => {
                    let acknowledged =
                        stream.gap(reason).await.is_ok() && Self::retired(&mut stream).await;
                    if !acknowledged && self.stdout_done.load(Ordering::SeqCst) {
                        self.retirement_failed.store(true, Ordering::SeqCst);
                    }
                    break;
                }
                Next::Idle => {
                    if self.stdout_done.load(Ordering::SeqCst) {
                        let kind = if self
                            .state
                            .lock()
                            .unwrap()
                            .incomplete
                            .contains("native_tail")
                        {
                            "native_tail_unconfirmed"
                        } else {
                            "native_reader_closed"
                        };
                        let acknowledged = if stream.close(json!({"reason":kind})).await.is_ok() {
                            Self::retired(&mut stream).await
                        } else {
                            false
                        };
                        if !acknowledged {
                            self.retirement_failed.store(true, Ordering::SeqCst);
                        }
                        break;
                    }
                    tokio::select! {_=changed=>{},command=stream.next()=>match command{
                        None|Some(crate::provider_client::StreamCommand::Close)=>break,
                        _=>{let _=stream.error(&ProviderError::new("unsupported","agent item stream is read-only")).await;break;}
                    }}
                }
            }
        }
    }
    async fn retired(stream: &mut ProviderStream) -> bool {
        loop {
            match stream.next().await {
                Some(crate::provider_client::StreamCommand::Close) => return true,
                None => return false,
                _ => {}
            }
        }
    }
    pub async fn publish(self: Arc<Self>, provider: Provider) {
        let mut published = 0;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (revision, facts) = {
                let state = self.state.lock().unwrap();
                (state.changed, state.facts(&self.name, &self.instance))
            };
            if revision != published {
                if provider.upsert(facts).await.is_err() {
                    break;
                }
                published = revision;
            }
            changed.await;
        }
    }
    pub async fn finish(self: &Arc<Self>, provider: &Provider) -> Result<(), ProviderError> {
        if let Some(native) = self.process() {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(2), native.drained()).await,
                Ok(crate::native_process::Drain::Eof)
            ) {
                self.state
                    .lock()
                    .unwrap()
                    .incomplete
                    .insert("native_tail".into());
            }
        }
        self.stdout_done.store(true, Ordering::SeqCst);
        self.wake();
        let wait = async {
            loop {
                let changed = self.viewers_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.viewers.load(Ordering::SeqCst) == 0 {
                    break;
                }
                changed.await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .is_err()
        {
            self.state
                .lock()
                .unwrap()
                .incomplete
                .insert("item_stream_tail".into());
            return Err(ProviderError::new(
                "timeout",
                "core item-stream retirement unconfirmed; no retry",
            ));
        }
        if self.retirement_failed.load(Ordering::SeqCst) {
            return Err(ProviderError::new(
                "unavailable",
                "core item-stream retirement unconfirmed; no retry",
            ));
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.exit.is_some() {
                state.end_ready = true;
            } else {
                state.incomplete.insert("native_exit".into());
            }
            state.refresh();
        }
        self.wake();
        // Only now may either publisher announce ended, after the native and item-stream tail cut.
        // provider.sync is the EXISTING acknowledged publication primitive (reply once applied).
        // Its ACK fences all preceding item data/closed frames on this connection before close().
        // A timeout/disconnect stays unconfirmed and is never replayed.
        match tokio::time::timeout(
            Duration::from_secs(5),
            provider.full_sync(vec![self.facts()]),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ProviderError::new(
                "timeout",
                "final publication deadline; unconfirmed, never resent",
            )),
        }
    }
    pub async fn serve_request(self: &Arc<Self>, provider: &Provider, request: Request) {
        match request {
            Request::Effect(effect) => {
                let shared = self.clone();
                let provider = provider.clone();
                tokio::spawn(async move {
                    let result = shared.invoke(&provider, &effect).await;
                    effect.respond(result);
                });
            }
            Request::Open(open) => self.open(open),
        }
    }
}
