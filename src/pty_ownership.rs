//! Neutral, volatile ownership observations for cooperative local client calls.
//! This is not session persistence, a caller credential or a task authority.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub(crate) const MAX_RECORD_BYTES: usize = 128 * 1024;
pub(crate) const MAX_SERVERS: usize = 64;
pub(crate) const MAX_OWNED_TTYS: usize = 4096;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ServerKey {
    pub boot_id: String,
    pub pid: u32,
    pub birth: u64,
}

impl ServerKey {
    fn valid(&self) -> bool {
        self.pid > 0
            && self.birth > 0
            && !self.boot_id.is_empty()
            && self.boot_id.len() <= 128
            && self
                .boot_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_:".contains(&c))
    }
}

/// Corroborated character-device facts, never a client-supplied path or ptmx.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TtyIdentity {
    pub device: u64,
    pub inode: u64,
    pub rdev: u64,
}

impl TtyIdentity {
    pub(crate) fn may_match(&self, other: &Self) -> bool {
        // Aliases and distinct devpts mounts can make inode/dev differ. A
        // possible match must deny authority, not manufacture nonownership.
        self.rdev == other.rdev
    }

    fn valid(&self) -> bool {
        self.inode != 0 && self.rdev != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnershipRecord {
    pub version: u32,
    pub server: ServerKey,
    pub epoch: u64,
    pub complete: bool,
    pub owned: Vec<TtyIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnershipStatus {
    pub server: ServerKey,
    pub epoch: u64,
    pub complete: bool,
}

/// The live authoritative epoch is distinct from its fallible file projection.
/// Lifecycle callers mark dirty before ownership changes; they never need a
/// successful publication in order to create, close, import or recover a PTY.
#[derive(Debug)]
pub(crate) struct OwnershipState {
    server: ServerKey,
    epoch: u64,
    pending: BTreeSet<u64>,
    members: BTreeMap<u64, Option<TtyIdentity>>,
    published: Option<u64>,
    unprovable: bool,
}

impl OwnershipState {
    pub(crate) fn new(server: ServerKey) -> Self {
        let unprovable = !server.valid();
        Self {
            server,
            epoch: 1,
            pending: BTreeSet::new(),
            members: BTreeMap::new(),
            published: None,
            unprovable,
        }
    }

    pub(crate) fn begin_change(&mut self) -> u64 {
        match self.epoch.checked_add(1) {
            Some(epoch) => self.epoch = epoch,
            None => self.unprovable = true,
        }
        self.published = None;
        if !self.pending.insert(self.epoch) {
            self.unprovable = true;
        }
        self.epoch
    }

    pub(crate) fn finish_add(&mut self, change: u64, tty: Option<TtyIdentity>) {
        if !self.pending.remove(&change) || self.members.insert(change, tty).is_some() {
            self.unprovable = true;
        }
    }

    pub(crate) fn finish_remove(&mut self, change: u64, member: u64) {
        if !self.pending.remove(&change) || self.members.remove(&member).is_none() {
            self.unprovable = true;
        }
    }

    pub(crate) fn finish_abort(&mut self, change: u64) {
        if !self.pending.remove(&change) {
            self.unprovable = true;
        }
    }

    fn proved(&self) -> bool {
        !self.unprovable
            && self.pending.is_empty()
            && self.members.len() <= MAX_OWNED_TTYS
            && self
                .members
                .values()
                .all(|tty| tty.as_ref().is_some_and(TtyIdentity::valid))
    }

    pub(crate) fn record(&self) -> OwnershipRecord {
        let owned: BTreeSet<_> = self.members.values().filter_map(Clone::clone).collect();
        OwnershipRecord {
            version: 1,
            server: self.server.clone(),
            epoch: self.epoch,
            complete: self.proved(),
            owned: owned.into_iter().collect(),
        }
    }

    pub(crate) fn status(&self) -> OwnershipStatus {
        OwnershipStatus {
            server: self.server.clone(),
            epoch: self.epoch,
            complete: self.proved() && self.published == Some(self.epoch),
        }
    }

    pub(crate) fn publication_succeeded(&mut self, epoch: u64) {
        // The file write can finish after a new ownership mutation began.
        // Never acknowledge the new set using an old asynchronous write.
        if self.epoch == epoch && self.proved() {
            self.published = Some(epoch);
        }
    }
}

pub(crate) fn parse_record(bytes: &[u8]) -> Result<OwnershipRecord, String> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err("ownership record exceeds byte bound".into());
    }
    let record: OwnershipRecord = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid ownership record: {error}"))?;
    if record.version != 1
        || record.epoch == 0
        || !record.server.valid()
        || record.owned.len() > MAX_OWNED_TTYS
        || record.owned.iter().any(|tty| !tty.valid())
        || record.owned.iter().collect::<BTreeSet<_>>().len() != record.owned.len()
    {
        return Err("unsupported or unprovable ownership record".into());
    }
    Ok(record)
}

pub(crate) fn validate_record(
    record: &OwnershipRecord,
    live: &OwnershipStatus,
    peer: &ServerKey,
) -> Result<(), String> {
    if !record.complete
        || !live.complete
        || &record.server != peer
        || &live.server != peer
        || record.epoch != live.epoch
    {
        return Err(
            "ownership publication is missing, incomplete, stale or belongs to another incarnation"
                .into(),
        );
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ServerLiveness {
    Live(ServerKey),
    Dead,
    Reused,
    Unknown,
}

pub(crate) fn check_liveness(liveness: ServerLiveness) -> Result<bool, String> {
    match liveness {
        ServerLiveness::Live(key) if key.valid() => Ok(true),
        ServerLiveness::Dead | ServerLiveness::Reused => Ok(false),
        _ => Err("server incarnation liveness is unprovable".into()),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OwnershipProof(BTreeMap<ServerKey, u64>);

impl OwnershipProof {
    pub(crate) fn capture(statuses: &[OwnershipStatus]) -> Result<Self, String> {
        if statuses.is_empty() || statuses.len() > MAX_SERVERS {
            return Err("ownership publisher set is empty or exceeds bound".into());
        }
        let mut epochs = BTreeMap::new();
        for status in statuses {
            if !status.complete
                || status.epoch == 0
                || !status.server.valid()
                || epochs.insert(status.server.clone(), status.epoch).is_some()
            {
                return Err("ownership observation is incomplete or duplicated".into());
            }
        }
        Ok(Self(epochs))
    }

    pub(crate) fn same_epochs(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    pub(crate) fn matches(&self, after_audit: &[OwnershipStatus]) -> bool {
        Self::capture(after_audit).is_ok_and(|proof| proof.0 == self.0)
    }

    pub(crate) fn matches_local(&self, at_admission: &OwnershipStatus) -> bool {
        at_admission.complete && self.0.get(&at_admission.server) == Some(&at_admission.epoch)
    }
}

pub(crate) mod observation;
pub(crate) mod publisher;
pub(crate) mod registry;

#[cfg(all(test, unix))]
mod registry_tests;
#[cfg(test)]
mod tests;
