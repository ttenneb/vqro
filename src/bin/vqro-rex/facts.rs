//! The only writer of block facts and the only publisher (including effect replies).
//! Native I/O produces messages outside this owner. The async owner lock orders apply + enqueue.
use super::*;

/// Active + retired identities are bounded. At exhaustion close this registration rather than forget
/// retirement history and resurrect refs. Unknown native events never consume this budget.
pub(super) const MAX_REGISTRATION_REFS: usize = 65_536;
const MAX_REF_BYTES: usize = 256;

pub(super) enum Change {
    Refresh { bid: String, session: String, base: Option<u64>, facts: Value },
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
        if !self.blocks.contains_key(bid) && !self.removed.contains(bid)
            && self.blocks.len() + self.removed.len() >= MAX_REGISTRATION_REFS {
            return Err("fact identity ledger exhausted; closing registration without evicting retired refs".into());
        }
        Ok(())
    }

    /// Pure application: no native I/O or outbound publication, and no caller mutates facts elsewhere.
    fn apply(&mut self, change: Change, run: &str) -> Result<(Option<Value>, Vec<Publication>), String> {
        let mut publications = Vec::new();
        let mut result = None;
        match change {
            Change::Refresh { bid, session, base, facts } => {
                if self.removed.contains(&bid) || self.blocks.get(&bid).map(|b| b.revision) != base {
                    return Ok((None, publications));
                }
                if self.blocks.get(&bid).is_some_and(|b| b.facts == facts) {
                    return Ok((Some(facts), publications));
                }
                self.admit(&bid)?;
                let revision = self.revision()?;
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
                // Unknown/gone ids allocate nothing: no revision entry and no tombstone.
                if !self.blocks.contains_key(&bid) { return Ok((None, publications)); }
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
                    self.argv.remove(&bid);
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
