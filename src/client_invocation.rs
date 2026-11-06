//! Explicit outer-client evidence. A descriptor alone is not action authority:
//! dispatch must also bind live ownership epochs, audit and provider/token facts.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::action_invocation::InvocationPeer;
use crate::pty_ownership::TtyIdentity;

#[derive(Debug, Clone)]
pub(crate) struct ClientAttachEvidence {
    pub peer: InvocationPeer,
    pub complete_ancestry: bool,
    pub tty: Option<TtyIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientTtyDescriptor {
    pub kind: String,
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientIdentity {
    pub boot_id: String,
    pub pid: u32,
    pub birth: u64,
    pub tty: ClientTtyDescriptor,
}

/// Capture on the connection worker before reading handshake claims. All native
/// observations are explicit/off actor, never render or per-frame fanout work.
pub(crate) fn capture(stream: &crate::ipc::LocalStream) -> Option<ClientAttachEvidence> {
    let initial = crate::platform::socket_peer_birth(stream)?;
    let peer = crate::platform::invocation_peer(stream)?;
    let first = peer.ancestry.first()?;
    if (first.pid, first.birth) != initial {
        return None;
    }
    let complete_ancestry = crate::platform::client_ancestry_complete(&peer);
    let tty = crate::platform::client_controlling_tty(first.pid)
        .filter(|(_, birth)| *birth == first.birth)
        .map(|(tty, _)| tty);
    if !crate::platform::invocation_peer_is_current(&peer)
        || crate::platform::socket_peer_birth(stream) != Some(initial)
    {
        return None;
    }
    Some(ClientAttachEvidence {
        peer,
        complete_ancestry,
        tty,
    })
}

fn server_ancestor(process: &crate::platform::ForegroundProcess) -> bool {
    // Exact observed server incarnations are checked separately. For absent
    // publishers, conservatively deny ANY executable's `server` argument; do
    // not assume a filename, fixed argv position, or run the mutating CLI parser.
    process
        .argv
        .as_ref()
        .is_some_and(|argv| argv.iter().skip(1).any(|arg| arg == "server"))
}

fn coherent(peer: &InvocationPeer) -> bool {
    if peer.ancestry.is_empty()
        || peer.ancestry.len() > 64
        || peer.boot_id.is_empty()
        || peer.boot_id.len() > 128
        || peer.ancestry.last().is_none_or(|last| last.parent_pid != 0)
    {
        return false;
    }
    let mut pids = BTreeSet::new();
    peer.ancestry.iter().all(|entry| {
        entry.pid > 0 && entry.birth > 0 && entry.pid == entry.process.pid && pids.insert(entry.pid)
    }) && peer
        .ancestry
        .windows(2)
        .all(|pair| pair[0].parent_pid == pair[1].pid)
}

/// This produces the host-derived receipt descriptor, not a synthetic terminal.
/// Caller admission additionally requires the independently validated complete
/// publisher proof and its post-audit/final-admission epoch fences.
pub(crate) fn classify_external(
    current: &ClientAttachEvidence,
    attached: &ClientAttachEvidence,
    owned_pane: bool,
    ownership: &crate::pty_ownership::observation::ObservedOwnership,
) -> Result<ClientIdentity, String> {
    let rejected = || {
        "client provenance is unproved or belongs to a Vqro-owned pane/TTY; actions remain read-only".to_owned()
    };
    if owned_pane
        || !current.complete_ancestry
        || !attached.complete_ancestry
        || !coherent(&current.peer)
        || !coherent(&attached.peer)
        || current.peer.account != attached.peer.account
        || current.peer.boot_id != attached.peer.boot_id
        || current
            .peer
            .ancestry
            .iter()
            .map(|entry| (entry.pid, entry.birth, entry.parent_pid))
            .ne(attached
                .peer
                .ancestry
                .iter()
                .map(|entry| (entry.pid, entry.birth, entry.parent_pid)))
    {
        return Err(rejected());
    }
    let tty = current.tty.as_ref().ok_or_else(rejected)?;
    if current.tty != attached.tty
        || tty.inode == 0
        || tty.rdev == 0
        || ownership
            .owned_ttys
            .iter()
            .any(|owned| owned.may_match(tty))
    {
        return Err(rejected());
    }
    for entry in &current.peer.ancestry {
        if ownership
            .proof
            .contains_process(&current.peer.boot_id, entry.pid, entry.birth)
            || server_ancestor(&entry.process)
        {
            return Err(rejected());
        }
        let job = crate::platform::ForegroundJob {
            process_group_id: entry.pid,
            processes: vec![entry.process.clone()],
        };
        if crate::detect::identify_agent_in_job(&job).is_some() {
            return Err(rejected());
        }
    }
    let first = current.peer.ancestry.first().ok_or_else(rejected)?;
    Ok(ClientIdentity {
        boot_id: current.peer.boot_id.clone(),
        pid: first.pid,
        birth: first.birth,
        tty: ClientTtyDescriptor {
            kind: "unix".into(),
            device: format!("{}:{}:{}", tty.device, tty.inode, tty.rdev),
        },
    })
}

#[cfg(test)]
mod tests;
