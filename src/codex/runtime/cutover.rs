//! Native saved-login switching under an all-slot admission fence. No prompts or credential I/O.
use super::*;
use std::collections::BTreeSet;
use tokio::sync::OwnedMutexGuard;

#[cfg(test)]
#[path = "cutover_tests.rs"]
mod tests;

pub(super) const COMMANDS: [&str; 8] = [
    "_account.fence",
    "_account.status",
    "_account.switch",
    "_account.resume",
    "_account.restore",
    "_account.release",
    "_account.abort",
    "_account.list",
];

pub(super) struct Barrier {
    operation: String,
    threads: Vec<(usize, String, String)>,
    guards: Vec<OwnedMutexGuard<()>>,
    stage: &'static str,
    account: Option<Value>,
    auth_revision: Option<u64>,
    source_auth_revision: u64,
    login_ref: Option<(String, String)>,
}

fn bounded(v: &Value, max: usize) -> Option<&str> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
fn reject(message: &str) -> Fault {
    Fault::Reject(RpcError::new("stale", message))
}
fn unknown() -> Fault {
    Fault::Unknown(RpcError::new(
        "indeterminate",
        "Codex account transition unconfirmed; fenced, never replay",
    ))
}

impl Runtime {
    /// Nonsecret launch configuration, not discovered from native settings/credential files.
    pub fn set_cutover_setup(&self, setup: &str) -> Result<(), RpcError> {
        if setup.is_empty()
            || setup.len() > 64
            || !setup
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(RpcError::new(
                "invalid",
                "Codex setup id must be 1..64 safe ASCII characters",
            ));
        }
        let mut current = self.cutover_setup.lock().unwrap();
        if current.is_some() {
            return Err(RpcError::new(
                "conflict",
                "Codex setup is already configured",
            ));
        }
        *current = Some(setup.to_owned());
        drop(current);
        for (i, slot) in self.slots.iter().enumerate() {
            let mut e = slot.endpoint.lock().unwrap();
            e.caps_rev = e.caps_rev.saturating_add(1);
            self.emit(i, &e, vec![e.state_entity()], false);
        }
        Ok(())
    }

    async fn sync_account_notifications(&self, deadline: Instant) -> Result<(), Fault> {
        let (tx, rx) = oneshot::channel();
        tokio::time::timeout(remaining(deadline), async {
            self.cutover_sync.send(tx).await.map_err(|_| unknown())?;
            rx.await.map_err(|_| unknown())
        })
        .await
        .map_err(|_| unknown())?
    }

    async fn read_active_login(
        &self,
        session: &str,
        workspace: &str,
        deadline: Instant,
    ) -> Result<u64, Fault> {
        let (login, revision) = self.read_current_login(deadline).await?;
        if login.0 != session || login.1 != workspace {
            return Err(unknown());
        }
        Ok(revision)
    }

    async fn read_current_login(
        &self,
        deadline: Instant,
    ) -> Result<((String, String), u64), Fault> {
        // One fenced metadata implementation for confirmation and lifecycle cleanup.
        // Bound read-only settling; never repeat a credential switch or resume.
        for _ in 0..2 {
            self.sync_account_notifications(deadline).await?;
            let revision = self.auth_revision.load(Ordering::Acquire);
            let value = self
                .native
                .call_with_timeout(
                    "accountSession/list",
                    json!({"refreshWorkspaceMetadata":false}),
                    remaining(deadline),
                )
                .await
                .map_err(|_| unknown())?;
            self.sync_account_notifications(deadline).await?;
            let projected = project_logins(&value)?;
            let active: Vec<_> = projected["logins"]
                .as_array()
                .ok_or_else(unknown)?
                .iter()
                .filter(|r| r["active"] == true)
                .collect();
            if active.len() != 1 {
                return Err(unknown());
            }
            let session = bounded(&active[0]["sessionId"], 256)
                .ok_or_else(unknown)?
                .to_owned();
            let workspace = bounded(&active[0]["accountId"], 256)
                .ok_or_else(unknown)?
                .to_owned();
            require_login(&value, &session, &workspace, true)?;
            if self.auth_revision.load(Ordering::Acquire) == revision {
                return Ok(((session, workspace), revision));
            }
        }
        Err(unknown())
    }

    async fn refresh_account_confirmation(
        &self,
        operation: &str,
        deadline: Instant,
    ) -> Result<u64, Fault> {
        let (previous, login, account) = {
            let barrier = self.cutover.lock().unwrap();
            let b = barrier
                .as_ref()
                .filter(|b| b.operation == operation)
                .ok_or_else(unknown)?;
            (
                b.auth_revision,
                b.login_ref.clone().ok_or_else(unknown)?,
                b.account.clone(),
            )
        };
        let current = self.auth_revision.load(Ordering::Acquire);
        if previous == Some(current) {
            return Ok(current);
        }
        let revision = self.read_active_login(&login.0, &login.1, deadline).await?;
        {
            let mut projection = self.launcher_account.lock().unwrap();
            if self.auth_revision.load(Ordering::Acquire) != revision {
                return Err(unknown());
            }
            self.account_projection_revision
                .fetch_add(1, Ordering::AcqRel);
            *projection = account;
        }
        self.cutover
            .lock()
            .unwrap()
            .as_mut()
            .filter(|b| b.operation == operation)
            .ok_or_else(unknown)?
            .auth_revision = Some(revision);
        for (i, slot) in self.slots.iter().enumerate() {
            let mut e = slot.endpoint.lock().unwrap();
            e.reset_availability();
            self.emit(i, &e, vec![e.state_entity()], false);
        }
        Ok(revision)
    }

    fn quiescent(&self) -> bool {
        self.slots.iter().all(|slot| {
            let e = slot.endpoint.lock().unwrap();
            e.lifecycle == "live" && e.state == "idle" && e.run.is_none() && e.pending.is_empty()
        })
    }

    pub(super) async fn account_command(
        self: &Arc<Self>,
        args: &Value,
        deadline: Instant,
    ) -> Result<Outcome, Fault> {
        let name = args["name"]
            .as_str()
            .ok_or_else(|| Fault::invalid("native account command name required"))?;
        if !COMMANDS.contains(&name) || self.cutover_setup.lock().unwrap().is_none() {
            return Err(Fault::unsupported(
                "native account switching is not configured",
            ));
        }
        let p = &args["args"];
        let _native_permit = if matches!(
            name,
            "_account.list" | "_account.switch" | "_account.resume" | "_account.restore"
        ) {
            Some(
                self.native_admissions
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| reject("native admission capacity is full"))?,
            )
        } else {
            None
        };
        if name == "_account.list" {
            let response = self
                .native
                .call_with_timeout(
                    "accountSession/list",
                    json!({"refreshWorkspaceMetadata":false}),
                    remaining(deadline),
                )
                .await
                .map_err(|_| unknown())?;
            return Ok(Outcome::Observed(Some(project_logins(&response)?)));
        }
        let operation = bounded(&p["operation"], 256)
            .ok_or_else(|| Fault::invalid("bounded cutover operation required"))?;
        let setup = self.cutover_setup.lock().unwrap().clone();
        if p["setup_id"].as_str() != setup.as_deref() {
            return Err(reject("shared Codex setup changed"));
        }
        if name == "_account.fence" {
            let mut barrier = self.cutover.lock().unwrap();
            let mut seen = self.cutover_seen.lock().unwrap();
            if barrier.is_some() || seen.contains(operation) || seen.len() >= 64 {
                return Err(reject(
                    "Codex host already fenced or intent consumed; never replay",
                ));
            }
            let members = p["members"]
                .as_array()
                .ok_or_else(|| Fault::invalid("all host thread witnesses required"))?;
            if members.len() != self.slots.len() {
                return Err(reject("Codex host cardinality changed"));
            }
            let mut guards = Vec::new();
            let mut threads = Vec::new();
            let mut matched = BTreeSet::new();
            for (i, slot) in self.slots.iter().enumerate() {
                let gate = slot
                    .admission
                    .clone()
                    .try_lock_owned()
                    .map_err(|_| reject("native admission still unresolved; no switch"))?;
                let e = slot.endpoint.lock().unwrap();
                let thread = e
                    .session
                    .as_ref()
                    .ok_or_else(|| reject("affected native thread is unknown"))?;
                let found = members
                    .iter()
                    .position(|m| m["session"] == *thread && m["epoch"] == e.epoch)
                    .ok_or_else(|| reject("native host membership changed"))?;
                if !matched.insert(found) || e.lifecycle != "live" {
                    return Err(reject("native host membership is not live/unique"));
                }
                threads.push((i, thread.clone(), e.epoch.clone()));
                guards.push(gate);
            }
            seen.insert(operation.to_owned());
            *barrier = Some(Barrier {
                operation: operation.to_owned(),
                threads,
                guards,
                stage: "fenced",
                account: None,
                auth_revision: None,
                source_auth_revision: self.auth_revision.load(Ordering::Acquire),
                login_ref: None,
            });
            return Ok(Outcome::Observed(Some(
                json!({"operation":operation,"quiescent":self.quiescent()}),
            )));
        }
        let (stage, threads, source_revision) = {
            let barrier = self.cutover.lock().unwrap();
            let b = barrier
                .as_ref()
                .filter(|b| b.operation == operation)
                .ok_or_else(|| reject("Codex fence witness missing"))?;
            (b.stage, b.threads.clone(), b.source_auth_revision)
        };
        match name {
            "_account.status" => Ok(Outcome::Observed(Some(
                json!({"operation":operation,"quiescent":self.quiescent(),"stage":stage}),
            ))),
            "_account.abort" => {
                if !matches!(stage, "fenced" | "checking") {
                    return Err(reject("cannot abort after native switch was attempted"));
                }
                let mut barrier = self.cutover.lock().unwrap();
                if barrier.as_ref().is_none_or(|b| {
                    b.operation != operation || !matches!(b.stage, "fenced" | "checking")
                }) {
                    return Err(reject("fence phase changed before abort"));
                }
                barrier.take(); // Only local admission guards: no native effect/input to replay.
                Ok(Outcome::Observed(Some(
                    json!({"operation":operation,"aborted":true}),
                )))
            }
            "_account.switch" => {
                self.sync_account_notifications(deadline).await?;
                if stage != "fenced" || !self.quiescent() {
                    return Err(reject(
                        "Codex turns/requests are not quiescent or switch already attempted",
                    ));
                }
                let session = bounded(&p["login_ref"]["sessionId"], 256)
                    .ok_or_else(|| Fault::invalid("native saved login id required"))?;
                let workspace = bounded(&p["login_ref"]["accountId"], 256)
                    .ok_or_else(|| Fault::invalid("native workspace id required"))?;
                let account = account_fact(&p["account"])?;
                // Consume phase before ANY irreversible call. Unknown outcomes stay fenced.
                {
                    let mut barrier = self.cutover.lock().unwrap();
                    let b = barrier
                        .as_mut()
                        .filter(|b| b.operation == operation && b.stage == "fenced")
                        .ok_or_else(|| reject("switch phase already consumed"))?;
                    b.stage = "checking";
                }
                let before = self
                    .native
                    .call_with_timeout(
                        "accountSession/list",
                        json!({"refreshWorkspaceMetadata":false}),
                        remaining(deadline),
                    )
                    .await
                    .map_err(|_| unknown())?;
                require_login(&before, session, workspace, false)?;
                self.sync_account_notifications(deadline).await?;
                let mut validated_revision = self.auth_revision.load(Ordering::Acquire);
                if validated_revision != source_revision {
                    // A prior managed host may have already moved this shared login to B.
                    // Accept that ONLY with independent native readback of the exact target.
                    validated_revision = self.read_active_login(session, workspace, deadline).await.map_err(|_| reject("native source changed and target is not confirmed before switching"))?;
                }
                {
                    let mut barrier = self.cutover.lock().unwrap();
                    let b = barrier
                        .as_mut()
                        .filter(|b| b.operation == operation && b.stage == "checking")
                        .ok_or_else(|| reject("native preflight was aborted"))?;
                    if validated_revision != self.auth_revision.load(Ordering::Acquire)
                        || !self.quiescent()
                    {
                        return Err(reject("native source changed during saved-login preflight"));
                    }
                    b.stage = "switching"; // Credential effect is consumed before dispatch.
                }
                // Never relabel B notifications as the previous configured account A.
                *self.launcher_account.lock().unwrap() = None;
                for (i, slot) in self.slots.iter().enumerate() {
                    let mut e = slot.endpoint.lock().unwrap();
                    e.reset_availability();
                    self.emit(i, &e, vec![e.state_entity()], false);
                }
                let response = self.native.call_with_timeout("accountSession/switch", json!({"sessionId":session,"accountId":workspace,"allowBrowserLogin":false}), remaining(deadline)).await.map_err(|_| unknown())?;
                // Browser/SSO results never count as account confirmation; no token/URL publication.
                require_login(&response, session, workspace, true)?;
                // Fence the response itself, not just the eventual projection. A B
                // snapshot followed by a queued C notification cannot certify C's revision.
                let auth_revision = self.read_active_login(session, workspace, deadline).await?;
                self.account_projection_revision
                    .fetch_add(1, Ordering::AcqRel);
                {
                    let mut projection = self.launcher_account.lock().unwrap();
                    if self.auth_revision.load(Ordering::Acquire) != auth_revision {
                        return Err(unknown());
                    }
                    *projection = Some(account.clone());
                }
                {
                    let mut barrier = self.cutover.lock().unwrap();
                    let b = barrier.as_mut().ok_or_else(unknown)?;
                    b.account = Some(account);
                    b.auth_revision = Some(auth_revision);
                    b.login_ref = Some((session.to_owned(), workspace.to_owned()));
                    b.stage = "switched";
                }
                for (i, slot) in self.slots.iter().enumerate() {
                    let mut e = slot.endpoint.lock().unwrap();
                    e.reset_availability();
                    self.emit(i, &e, vec![e.state_entity()], false);
                }
                Ok(Outcome::Observed(Some(
                    json!({"operation":operation,"account_confirmed":true}),
                )))
            }
            "_account.resume" | "_account.restore" => {
                if name == "_account.restore" {
                    // Lifecycle teardown only: never select or roll credentials back. Match the
                    // current native login against the pre-effect source or confirmed target.
                    if !matches!(stage, "fenced" | "checking" | "switched") {
                        return Err(reject("restore cannot replay an uncertain switch/resume"));
                    }
                    let allowed = p["accounts"]
                        .as_array()
                        .filter(|v| !v.is_empty() && v.len() <= 2)
                        .ok_or_else(|| {
                            Fault::invalid("bounded source/target restore references required")
                        })?;
                    let mut candidates = Vec::new();
                    for candidate in allowed {
                        let session = bounded(&candidate["login_ref"]["sessionId"], 256)
                            .ok_or_else(|| Fault::invalid("restore login reference required"))?
                            .to_owned();
                        let workspace = bounded(&candidate["login_ref"]["accountId"], 256)
                            .ok_or_else(|| Fault::invalid("restore workspace reference required"))?
                            .to_owned();
                        let account = if candidate["account"].is_null() {
                            None
                        } else {
                            Some(account_fact(&candidate["account"])?)
                        };
                        candidates.push(((session, workspace), account));
                    }
                    self.cutover
                        .lock()
                        .unwrap()
                        .as_mut()
                        .filter(|b| {
                            b.operation == operation
                                && matches!(b.stage, "fenced" | "checking" | "switched")
                        })
                        .ok_or_else(unknown)?
                        .stage = "restoring";
                    let (login, revision) = self.read_current_login(deadline).await?;
                    let account = candidates
                        .into_iter()
                        .find(|(expected, _)| expected == &login)
                        .ok_or_else(unknown)?
                        .1;
                    {
                        let mut projection = self.launcher_account.lock().unwrap();
                        if self.auth_revision.load(Ordering::Acquire) != revision {
                            return Err(unknown());
                        }
                        self.account_projection_revision
                            .fetch_add(1, Ordering::AcqRel);
                        *projection = account.clone();
                    }
                    {
                        let mut barrier = self.cutover.lock().unwrap();
                        let b = barrier
                            .as_mut()
                            .filter(|b| b.operation == operation && b.stage == "restoring")
                            .ok_or_else(unknown)?;
                        b.account = account;
                        b.login_ref = Some(login);
                        b.auth_revision = Some(revision);
                        b.stage = "switched";
                    }
                    for (i, slot) in self.slots.iter().enumerate() {
                        let mut e = slot.endpoint.lock().unwrap();
                        e.reset_availability();
                        self.emit(i, &e, vec![e.state_entity()], false);
                    }
                } else if stage != "switched" {
                    return Err(reject(
                        "native account not confirmed or resume already attempted",
                    ));
                }
                self.refresh_account_confirmation(operation, deadline)
                    .await?;
                {
                    let mut barrier = self.cutover.lock().unwrap();
                    let b = barrier
                        .as_mut()
                        .filter(|b| b.stage == "switched")
                        .ok_or_else(|| reject("resume phase consumed"))?;
                    b.stage = "resuming";
                }
                for (index, thread, _) in threads {
                    let response = self
                        .native
                        .call_with_timeout(
                            "thread/resume",
                            json!({"threadId":thread}),
                            remaining(deadline),
                        )
                        .await
                        .map_err(|_| unknown())?;
                    self.sync_account_notifications(deadline).await?;
                    self.refresh_account_confirmation(operation, deadline)
                        .await?;
                    if response["thread"]["id"] != thread {
                        return Err(unknown());
                    }
                    let mut e = self.slots[index].endpoint.lock().unwrap();
                    let changes = e
                        .set_session_response_from(
                            &response,
                            unique_epoch(&self.incarnation),
                            "rpc:thread/resume",
                        )
                        .map_err(|_| unknown())?;
                    self.emit(index, &e, changes, true);
                    drop(e);
                    self.start_startup_reads(index);
                }
                self.cutover
                    .lock()
                    .unwrap()
                    .as_mut()
                    .ok_or_else(unknown)?
                    .stage = "resumed";
                Ok(Outcome::Observed(Some(
                    json!({"operation":operation,"resumed":true,"account_confirmed":true,"account":self.launcher_account.lock().unwrap().clone()}),
                )))
            }
            "_account.release" => {
                if stage != "resumed" {
                    return Err(reject(
                        "all same-thread resumes must be observed before admission release",
                    ));
                }
                self.refresh_account_confirmation(operation, deadline)
                    .await?;
                let mut b = self.cutover.lock().unwrap().take().ok_or_else(unknown)?;
                let account_name = b
                    .account
                    .as_ref()
                    .and_then(|a| a["label"].as_str())
                    .map(|label| format!("account {label}"))
                    .unwrap_or_else(|| "the previously observed native account".to_owned());
                let notice = if p["revoked"] == true {
                    let reason = if p["revocation_kind"] == "unconfirmed" {
                        "switch authorization could no longer be confirmed"
                    } else {
                        "automatic switching was turned off during the switch"
                    };
                    format!("{reason}: resumed on {account_name}; no continuation sent")
                } else {
                    format!("continued on {account_name}")
                };
                for (i, slot) in self.slots.iter().enumerate() {
                    let mut e = slot.endpoint.lock().unwrap();
                    e.next_request += 1;
                    self.emit(i, &e, vec![Change::new("message", format!("_account_{}", e.next_request), json!({"role":"system","content":[{"type":"text","format":"plain","text":notice}],"final":true,"notice":{"kind":"account_switch","source":"vqro"}}))], false);
                }
                b.guards.clear();
                Ok(Outcome::Observed(Some(
                    json!({"operation":operation,"released":true}),
                )))
            }
            _ => Err(Fault::unsupported("unknown native account command")),
        }
    }
}

fn account_fact(value: &Value) -> Result<Value, Fault> {
    let id = bounded(&value["id"], 64)
        .ok_or_else(|| Fault::invalid("configured account id required"))?;
    let label = bounded(&value["label"], 512)
        .ok_or_else(|| Fault::invalid("bounded configured account label required"))?;
    if value["kind"] != "subscription" {
        return Err(Fault::unsupported(
            "native saved login is subscription-only",
        ));
    }
    let mut fact = json!({"id":id,"label":label,"kind":"subscription","sources":{"id":"vqro:cutover","label":"vqro:selection","kind":"vqro:selection"}});
    if let Some(provider) = bounded(&value["provider"], 64) {
        fact["provider"] = json!(provider);
        fact["sources"]["provider"] = json!("vqro:selection");
    }
    Ok(fact)
}

fn require_login(value: &Value, session: &str, workspace: &str, active: bool) -> Result<(), Fault> {
    let rows = value["sessions"]
        .as_array()
        .filter(|r| r.len() <= 64)
        .ok_or_else(unknown)?;
    let row = rows
        .iter()
        .find(|r| r["sessionId"] == session)
        .ok_or_else(unknown)?;
    if !row["workspaces"]
        .as_array()
        .filter(|r| r.len() <= 128)
        .is_some_and(|r| r.iter().any(|w| w["accountId"] == workspace))
    {
        return Err(unknown());
    }
    if active
        && (value["activeSessionId"] != session
            || row["isActive"] != true
            || row["selectedWorkspaceAccountId"] != workspace)
    {
        return Err(unknown());
    }
    Ok(())
}

fn project_logins(value: &Value) -> Result<Value, Fault> {
    let rows = value["sessions"]
        .as_array()
        .filter(|r| r.len() <= 64)
        .ok_or_else(unknown)?;
    let mut projected = Vec::new();
    for row in rows {
        let session = bounded(&row["sessionId"], 256).ok_or_else(unknown)?;
        let workspaces = row["workspaces"]
            .as_array()
            .filter(|r| r.len() <= 128)
            .ok_or_else(unknown)?;
        for workspace in workspaces {
            let account = bounded(&workspace["accountId"], 256).ok_or_else(unknown)?;
            projected.push(json!({"sessionId":session,"accountId":account,"active":value["activeSessionId"] == session && row["selectedWorkspaceAccountId"] == account}));
            if projected.len() > 128 {
                return Err(unknown());
            }
        }
    }
    Ok(json!({"logins":projected}))
}
