//! The only writer of block facts and the only publisher (including effect replies).
//! Native I/O produces messages outside this owner. The async owner lock orders apply + enqueue.
use super::*;

/// Active + retired identities are bounded. At exhaustion close this registration rather than forget
/// retirement history and resurrect refs. Unknown native events never consume this budget.
pub(super) const MAX_REGISTRATION_REFS: usize = 65_536;
const MAX_REF_BYTES: usize = 256;
pub(super) const MAX_PENDING_REFS: usize = 1024;
pub(super) const MAX_ARGV_BYTES: usize = 64 * 1024;
pub(super) fn argv_size(argv: &[String]) -> usize {
    argv.iter().fold(0usize, |n, s| n.saturating_add(s.len()).saturating_add(std::mem::size_of::<String>()))
}

pub(super) struct Pending {
    token: u64,
    revision: u64,
    argv: Vec<String>,
}

/// Clears an admitted stamp/argv on ordinary failure or cancellation. The token protects a newer
/// replacement reservation from an older future's cleanup. Registration End also clears the whole set.
pub(super) struct PendingGuard {
    app: Arc<App>,
    bid: String,
    token: Option<u64>,
}

impl PendingGuard {
    pub(super) fn new(app: Arc<App>, bid: String, token: Option<u64>) -> Self { Self { app, bid, token } }
    pub(super) async fn clear(&mut self) {
        if let Some(token) = self.token {
            let _ = self.app.write_facts(Change::Cancel { bid:self.bid.clone(), token }).await;
            self.token = None;
        }
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let (Some(token), Ok(runtime)) = (self.token.take(), tokio::runtime::Handle::try_current()) {
            let (app, bid) = (self.app.clone(), self.bid.clone());
            runtime.spawn(async move { let _ = app.write_facts(Change::Cancel { bid, token }).await; });
        }
    }
}

pub(super) enum Change {
    Begin { bid: String },
    Created { bid: String, argv: Vec<String> },
    Cancel { bid: String, token: u64 },
    End,
    Refresh { bid: String, session: String, base: Option<u64>, token: Option<u64>, facts: Value },
    Status { bid: String, base: u64, status: Value },
    Event { bid: String, name: String, body: Value },
    Kill { bid: String, base: u64 },
    Remove { bid: String, incarnation: String, reason: Value },
    Snapshot { bid: String },
    Sync,
    Reply { request: EffectRequest, result: Result<Outcome, ProviderError> },
}

enum Publication {
    Upsert(Value),
    Gone(String, String, Value),
    Sync(Vec<Value>),
}

impl State {
    fn revision(&mut self) -> Result<u64, String> {
        self.next_revision = self.next_revision.checked_add(1).ok_or("fact revision exhausted; closing registration")?;
        Ok(self.next_revision)
    }

    fn admit(&self, bid: &str) -> Result<(), String> {
        if bid.is_empty() || bid.len() > MAX_REF_BYTES {
            return Err("invalid/oversized Rex ref; closing registration".into());
        }
        if !self.blocks.contains_key(bid) && !self.removed.contains(bid) && !self.pending.contains_key(bid)
            && self.blocks.len() + self.removed.len() + self.pending.len() >= MAX_REGISTRATION_REFS {
            return Err("fact identity ledger exhausted; closing registration without evicting retired refs".into());
        }
        Ok(())
    }

    fn pending_admit(&self, bid: &str) -> Result<(), String> {
        self.admit(bid)?;
        if !self.pending.contains_key(bid) && self.pending.len() >= MAX_PENDING_REFS {
            return Err("pending discovery budget exhausted; closing registration".into());
        }
        Ok(())
    }

    /// Pure application: no native I/O or outbound publication, and no caller mutates facts elsewhere.
    fn apply(&mut self, change: Change, run: &str) -> Result<(Option<Value>, Vec<Publication>), String> {
        let mut publications = Vec::new();
        let mut result = None;
        if self.ending { return Ok((None, publications)); }
        match change {
            Change::Begin { bid } => {
                if self.removed.contains(&bid) { return Ok((None, publications)); }
                if let Some(block) = self.blocks.get(&bid) {
                    result = Some(json!({"revision":block.revision,"facts":block.facts,"argv":[]}));
                } else {
                    self.pending_admit(&bid)?;
                    let revision = self.revision()?;
                    let argv = self.pending.remove(&bid).map(|p| p.argv).unwrap_or_default();
                    // Transfer any creation metadata into this observation's unique token. Cleanup from
                    // an older observer or creation future cannot erase this newer in-flight witness.
                    self.pending.insert(bid.clone(), Pending { token:revision, revision, argv });
                    let pending = &self.pending[&bid];
                    result = Some(json!({"revision":pending.revision,"token":pending.token,"facts":null,"argv":pending.argv}));
                }
            }
            Change::Created { bid, argv } => {
                if argv_size(&argv) > MAX_ARGV_BYTES {
                    return Err("pending argv exceeds byte budget; closing registration".into());
                }
                if self.removed.contains(&bid) { return Ok((None, publications)); }
                if self.blocks.contains_key(&bid) {
                    let revision = self.revision()?;
                    let block = self.blocks.get_mut(&bid).unwrap();
                    block.revision = revision;
                    block.facts["argv"] = json!(argv);
                    publications.push(Publication::Upsert(block.facts.clone()));
                } else {
                    self.pending_admit(&bid)?;
                    let revision = self.revision()?;
                    // A delayed creation acknowledgement replaces an older discovery token. Cancellation
                    // from the older observation cannot erase this reservation or its argv.
                    self.pending.insert(bid, Pending { token:revision, revision, argv });
                    result = Some(json!({"token":revision}));
                }
            }
            Change::Cancel { bid, token } => {
                if self.pending.get(&bid).is_some_and(|p| p.token == token) { self.pending.remove(&bid); }
            }
            Change::End => { self.pending.clear(); self.ending = true; }
            Change::Refresh { bid, session, base, token, facts } => {
                let current = self.blocks.get(&bid).map(|b| b.revision);
                let pending = self.pending.get(&bid);
                let valid = if current.is_some() { token.is_none() && current == base }
                    else { pending.is_some_and(|p| Some(p.token) == token && Some(p.revision) == base) };
                if self.removed.contains(&bid) || !valid {
                    return Ok((None, publications));
                }
                if self.blocks.get(&bid).is_some_and(|b| b.facts == facts) {
                    return Ok((Some(facts), publications));
                }
                self.admit(&bid)?;
                let revision = self.revision()?;
                self.pending.remove(&bid);
                self.blocks.insert(bid, Block { session, facts: facts.clone(), revision });
                result = Some(facts.clone());
                publications.push(Publication::Upsert(facts));
            }
            Change::Status { bid, base, status } => {
                if !self.blocks.get(&bid).is_some_and(|b| b.revision == base) { return Ok((None, publications)); }
                if self.blocks[&bid].facts["status"] == status { return Ok((None, publications)); }
                let revision = self.revision()?;
                let block = self.blocks.get_mut(&bid).unwrap();
                block.revision = revision;
                block.facts["status"] = status;
                publications.push(Publication::Upsert(block.facts.clone()));
            }
            Change::Event { bid, name, body } => {
                // Only explicitly admitted discovery refs get an in-flight witness. Other unknown/gone
                // ids allocate nothing. A pending event invalidates the candidate without creating a Block.
                if !self.blocks.contains_key(&bid) {
                    if self.pending.contains_key(&bid) {
                        let revision = self.revision()?;
                        self.pending.get_mut(&bid).unwrap().revision = revision;
                    }
                    return Ok((None, publications));
                }
                let revision = self.revision()?;
                let block = self.blocks.get_mut(&bid).unwrap();
                block.revision = revision;
                let f = &mut block.facts;
                let before = f.clone();
                match name.as_str() {
                    "title_changed" if body.get("title").is_some_and(|v| v.is_string() || v.is_null()) =>
                        f["title"] = json!(body["title"].as_str().filter(|s| !s.is_empty())),
                    "pwd_changed" => if let Some(cwd) = body["pwd"].as_str().and_then(cwd_from_pwd) { f["cwd"] = json!(cwd); },
                    "child_exited" if integer(&body["exit_code"]) => {
                        f["state"] = json!("finished");
                        f["exit"] = json!({"reason":"exited","exit_code":body["exit_code"]});
                    }
                    "process_changed" => if let Some(name) = body["foreground"]["name"].as_str() {
                        f["data"]["foreground"] = json!(name);
                    },
                    // The event invalidates older observations; its stored table is queried outside the owner.
                    "program_status_changed" | "program_status_removed" => {}
                    _ => {}
                }
                if *f != before { publications.push(Publication::Upsert(f.clone())); }
            }
            Change::Kill { bid, base } => {
                if !self.blocks.get(&bid).is_some_and(|b| b.revision == base) { return Ok((None, publications)); }
                let revision = self.revision()?;
                let block = self.blocks.get_mut(&bid).unwrap();
                block.revision = revision;
                block.facts["state"] = json!("finished");
                block.facts["exit"] = json!({"reason":"killed"});
                self.killed.insert(bid);
                result = Some(block.facts.clone());
                publications.push(Publication::Upsert(block.facts.clone()));
            }
            Change::Remove { bid, incarnation, mut reason } => {
                if incarnation == run {
                    self.admit(&bid)?;
                    self.blocks.remove(&bid);
                    self.removed.insert(bid.clone());
                    self.pending.remove(&bid);
                    if self.killed.remove(&bid) && reason["reason"] == "exited" { reason["reason"] = json!("killed"); }
                    for stream in self.streams.remove(&bid).unwrap_or_default() { stream.notify_one(); }
                }
                publications.push(Publication::Gone(bid, incarnation, reason));
            }
            Change::Sync => {
                self.facts_ready = true;
                publications.push(Publication::Sync(self.blocks.values().map(|b| b.facts.clone()).collect()));
            }
            Change::Snapshot { bid } => result = self.blocks.get(&bid).map(|b| b.facts.clone()),
            Change::Reply { .. } => unreachable!("effect replies are enqueued by the owner, not stored"),
        }
        Ok((result, publications))
    }
}

impl App {
    /// One writer for all fact mutations and publication paths. The caller carries its observation's base
    /// revision; events apply to current blocks, removals are final, refresh/kill results are conditional.
    pub(super) async fn write_facts(&self, change: Change) -> Result<Option<Value>, String> {
        let _owner = self.publishing.lock().await;
        if let Change::Reply { request, mut result } = change {
            // An effect result is also a publication. Never reply with a snapshot cloned before a newer
            // event/removal: resolve the current facts and enqueue the reply under the same owner lock.
            if let Ok(Outcome::Observed(Some(value))) = &mut result {
                if let Some(bid) = value["facts"]["ref"].as_str().map(String::from) {
                    let current = self.state.lock().unwrap().blocks.get(&bid).map(|b| b.facts.clone());
                    result = match current {
                        Some(facts) => Ok(Outcome::Observed(Some(json!({"facts":facts})))),
                        None if request.op == "terminal.spawn" => Err(ProviderError::new("backend",
                            format!("terminal created: ref {bid}; no current lifecycle facts; do not retry spawn"))),
                        None => Ok(Outcome::Observed(None)),
                    };
                }
            }
            request.respond(result);
            return Ok(None);
        }
        let applied = self.state.lock().unwrap().apply(change, &self.run);
        let (result, publications) = match applied {
            Ok(value) => value,
            Err(error) => { self.provider.close(); return Err(error); }
        };
        for publication in publications {
            let sent = match publication {
                Publication::Upsert(facts) => {
                    if !self.state.lock().unwrap().facts_ready { continue; }
                    self.provider.upsert(facts).await
                }
                Publication::Gone(bid, incarnation, reason) => self.provider.gone(&bid, &incarnation, reason).await,
                Publication::Sync(facts) => self.provider.full_sync(facts).await,
            };
            if let Err(error) = sent { self.provider.close(); return Err(error.to_string()); }
        }
        Ok(result)
    }
}
