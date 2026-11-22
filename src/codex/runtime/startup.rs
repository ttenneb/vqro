//! Optional, one-shot native catalogs. Never hold session/admission locks while waiting.
use super::{Runtime, StartupRead};
use crate::rpc::Peer;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const FIELDS: [&str; 4] = ["hooks", "skills", "plugins", "mcp"];

impl Runtime {
    pub fn initialize_startup(&self, response: &Value) {
        self.startup_initialized.store(true, Ordering::Release);
        for (index, slot) in self.slots.iter().enumerate() {
            let mut endpoint = slot.endpoint.lock().unwrap();
            let changes = endpoint.initialize_startup(response);
            if !changes.is_empty() {
                self.emit(index, &endpoint, changes, false);
            }
        }
    }

    pub(super) fn start_startup_reads(self: &Arc<Self>, index: usize) {
        // Catalog capture is armed only by the observed initialize reply, not Runtime construction.
        if !self.startup_initialized.load(Ordering::Acquire) {
            return;
        }
        let (thread, epoch, cwd) = {
            let endpoint = self.slots[index].endpoint.lock().unwrap();
            let Some(thread) = endpoint.session.clone() else {
                return;
            };
            (thread, endpoint.epoch.clone(), endpoint.cwd.clone())
        };
        let Ok(permit) = self.startup_reads.clone().try_acquire_owned() else {
            let mut endpoint = self.slots[index].endpoint.lock().unwrap();
            for field in FIELDS {
                let changes = endpoint.finish_startup_inventory(&thread, &epoch, field, None);
                self.emit(index, &endpoint, changes, false);
            }
            return;
        };
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let _task_permit = permit;
            for field in FIELDS {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                {
                    let endpoint = this.slots[index].endpoint.lock().unwrap();
                    if endpoint.lifecycle == "ended"
                        || endpoint.epoch != epoch
                        || endpoint.session.as_deref() != Some(&thread)
                    {
                        return;
                    }
                }
                let native = this.native.clone();
                let completed = this.startup_completed.clone();
                let admission = this.native_admissions.clone().try_acquire_owned();
                drop(this);
                let response = if let Ok(admission) = admission {
                    let response = catalog(&native, field, &thread, cwd.as_deref()).await;
                    drop(admission);
                    response
                } else {
                    None
                };
                // Application is serialized by production drain_native, with epoch/lifecycle fences.
                if completed
                    .send(StartupRead {
                        index,
                        thread: thread.clone(),
                        epoch: epoch.clone(),
                        field,
                        response,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

async fn read(native: &Peer, method: &str, params: Value) -> Option<Value> {
    native
        .call_with_timeout(method, params, Duration::from_secs(3))
        .await
        .ok()
}

async fn catalog(native: &Peer, field: &str, thread: &str, cwd: Option<&str>) -> Option<Value> {
    match field {
        "hooks" => read(native, "hooks/list", json!({"threadId":thread})).await,
        "skills" => {
            read(
                native,
                "skills/list",
                json!({"cwds":[cwd?],"forceReload":false}),
            )
            .await
        }
        "plugins" => read(native, "plugin/installed", json!({})).await,
        "mcp" => {
            let mut cursor = Value::Null;
            let mut seen = BTreeSet::new();
            let mut rows = Vec::new();
            for _ in 0..4 {
                let page = read(
                    native,
                    "mcpServerStatus/list",
                    json!({"threadId":thread,"detail":"full","limit":64,"cursor":cursor}),
                )
                .await?;
                let entries = page["data"].as_array()?;
                let room = 256usize.saturating_sub(rows.len());
                // Never retain opaque tool/config metadata or credentials in a startup result.
                rows.extend(
                    entries
                        .iter()
                        .take(room)
                        .map(|r| json!({"name":r["name"],"runtimeStatus":r["runtimeStatus"]})),
                );
                let next = page.get("nextCursor")?;
                if entries.len() > room {
                    return Some(json!({"data":rows,"_startup_partial":true}));
                }
                if next.is_null() {
                    return Some(json!({"data":rows}));
                }
                let next = next.as_str().filter(|s| !s.is_empty() && s.len() <= 256)?;
                if !seen.insert(next.to_owned()) {
                    return None;
                }
                cursor = json!(next);
            }
            Some(json!({"data":rows,"_startup_partial":true}))
        }
        _ => None,
    }
}
