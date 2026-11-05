//! Explicit bounded known-publisher observation; never a global absence proof.

use std::time::Instant;

use super::registry::{RegistryDirectory, RegistryEntry};
use super::{validate_record, OwnershipProof, OwnershipStatus, TtyIdentity};

pub(crate) struct ObservedOwnership {
    pub proof: OwnershipProof,
    pub owned_ttys: Vec<TtyIdentity>,
}

pub(crate) fn observe(deadline: Instant) -> Result<ObservedOwnership, String> {
    let local = super::publisher::current_status()
        .filter(|status| status.complete)
        .ok_or("local ownership publication is unavailable; actions are read-only")?;
    let base = crate::platform::ownership_runtime_base()
        .ok_or("fixed runtime namespace is unavailable")?;
    let directory = RegistryDirectory::existing(&base).map_err(|error| error.to_string())?;
    let entries = directory
        .entries_with_liveness(|key| {
            if Instant::now() >= deadline {
                return super::ServerLiveness::Unknown;
            }
            crate::platform::ownership_server_liveness(key, &local.server.boot_id)
        })
        .map_err(|error| error.to_string())?;
    validate_set(&entries, &local, |entry| {
        if Instant::now() >= deadline {
            return Err("ownership observation deadline expired".into());
        }
        if entry.ownership.server == local.server {
            // A pure cached self read, never a recursive call to this App actor.
            super::publisher::current_status()
                .ok_or_else(|| "local ownership state is unavailable".into())
        } else {
            crate::platform::ownership_status_at(
                std::path::Path::new(&entry.endpoint),
                &entry.ownership.server,
                deadline,
            )
        }
    })
}

fn validate_set(
    entries: &[RegistryEntry],
    local: &OwnershipStatus,
    mut probe: impl FnMut(&RegistryEntry) -> Result<OwnershipStatus, String>,
) -> Result<ObservedOwnership, String> {
    if !entries
        .iter()
        .any(|entry| entry.ownership.server == local.server)
    {
        return Err("local ownership record is missing".into());
    }
    let mut statuses = Vec::with_capacity(entries.len());
    let mut owned_ttys = Vec::new();
    for entry in entries {
        let before = probe(entry)?;
        validate_record(&entry.ownership, &before, &entry.ownership.server)?;
        let after = probe(entry)?;
        validate_record(&entry.ownership, &after, &entry.ownership.server)?;
        if before != after {
            return Err("ownership epoch changed during observation".into());
        }
        statuses.push(after);
        owned_ttys.extend(entry.ownership.owned.iter().cloned());
    }
    let proof = OwnershipProof::capture(&statuses)?;
    Ok(ObservedOwnership { proof, owned_ttys })
}

impl ObservedOwnership {
    /// Called after async peer proof AND the checked audit write. A different
    /// record set or live epoch invalidates this proof, including publisher loss.
    pub(crate) fn recheck_after_audit(&self, deadline: Instant) -> Result<(), String> {
        let current = observe(deadline)?;
        if self.proof.same_epochs(&current.proof) {
            Ok(())
        } else {
            Err("ownership publisher epochs changed after proof/audit".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty_ownership::{OwnershipRecord, ServerKey};

    fn entry(pid: u32) -> RegistryEntry {
        RegistryEntry {
            endpoint: "/private/api.sock".into(),
            ownership: OwnershipRecord {
                version: 1,
                server: ServerKey {
                    boot_id: "boot".into(),
                    pid,
                    birth: 10,
                },
                epoch: 1,
                complete: true,
                owned: vec![TtyIdentity {
                    device: 1,
                    inode: u64::from(pid),
                    rdev: u64::from(pid),
                }],
            },
        }
    }
    fn status(entry: &RegistryEntry) -> OwnershipStatus {
        OwnershipStatus {
            server: entry.ownership.server.clone(),
            epoch: entry.ownership.epoch,
            complete: true,
        }
    }

    #[test]
    fn observed_publishers_are_not_silently_dropped_or_replaced_by_old_complete_files() {
        let first = entry(1);
        let second = entry(2);
        let local = status(&first);
        let entries = [first.clone(), second.clone()];
        let observed =
            validate_set(&entries, &local, |entry| Ok(status(entry))).expect("live complete set");
        assert_eq!(observed.owned_ttys.len(), 2);
        assert!(observed.proof.matches_local(&local));
        assert!(
            validate_set(&[second], &local, |entry| Ok(status(entry))).is_err(),
            "own record is mandatory"
        );
        assert!(
            validate_set(&entries, &local, |entry| {
                let mut live = status(entry);
                if entry.ownership.server.pid == 2 {
                    live.complete = false;
                    live.epoch += 1;
                }
                Ok(live)
            })
            .is_err(),
            "R1 file cannot hide failed R2 publication"
        );
        assert!(validate_set(&entries, &local, |_| Err(
            "old provider has no status method".into()
        ))
        .is_err());
    }

    #[test]
    fn epoch_drift_on_the_second_probe_invalidates_even_a_new_complete_publication() {
        let first = entry(1);
        let local = status(&first);
        let mut count = 0;
        assert!(validate_set(&[first], &local, |entry| {
            count += 1;
            let mut status = status(entry);
            if count == 2 {
                status.epoch += 1;
            }
            Ok(status)
        })
        .is_err());
    }
}
