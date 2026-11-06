use super::*;

fn server() -> ServerKey {
    ServerKey {
        boot_id: "boot".into(),
        pid: 42,
        birth: 100,
    }
}

fn tty(n: u64) -> TtyIdentity {
    TtyIdentity {
        device: 1,
        inode: n + 1,
        rdev: n + 100,
    }
}

fn ready() -> OwnershipState {
    let mut state = OwnershipState::new(server());
    let change = state.begin_change();
    state.finish_add(change, Some(tty(1)));
    let record = state.record();
    assert!(record.complete);
    assert!(
        !state.status().complete,
        "unwritten projection is not live authority"
    );
    state.publication_succeeded(record.epoch);
    assert!(state.status().complete);
    state
}

#[test]
fn failed_update_cannot_hide_behind_an_old_complete_record() {
    let mut state = ready();
    let old = state.record();
    let change = state.begin_change();
    assert!(
        !state.status().complete,
        "dirty BEFORE creating/exposing the PTY"
    );
    state.finish_add(change, Some(tty(2)));
    // Core ownership continues even when the registry write never succeeds.
    assert_eq!(state.record().owned.len(), 2);
    assert!(!state.status().complete);
    assert!(validate_record(&old, &state.status(), &server()).is_err());
    // A late acknowledgement of R1 is not acknowledgement of the new set.
    state.publication_succeeded(old.epoch);
    assert!(!state.status().complete);
    let current = state.record();
    state.publication_succeeded(current.epoch);
    assert!(validate_record(&current, &state.status(), &server()).is_ok());
}

#[test]
fn changing_or_unproved_imports_never_publish_complete_authority() {
    let mut state = ready();
    let first = state.begin_change();
    let second = state.begin_change();
    state.finish_add(first, Some(tty(2)));
    assert!(
        !state.record().complete,
        "another ownership mutation is still pending"
    );
    state.finish_add(second, None);
    let record = state.record();
    assert!(
        !record.complete,
        "unknown imported slave identity stays fail closed"
    );
    state.publication_succeeded(record.epoch);
    assert!(!state.status().complete);
}

#[test]
fn aborted_creation_and_actual_release_can_restore_a_complete_snapshot() {
    let mut state = OwnershipState::new(server());
    let aborted = state.begin_change();
    state.finish_abort(aborted);
    let added = state.begin_change();
    state.finish_add(added, Some(tty(1)));
    let removed = state.begin_change();
    state.finish_remove(removed, added);
    let record = state.record();
    assert!(record.complete);
    assert!(record.owned.is_empty());
    state.publication_succeeded(record.epoch);
    assert!(state.status().complete);
}

#[test]
fn proof_rejects_epoch_drift_during_async_proof_audit_or_final_admission() {
    let mut state = ready();
    let status = state.status();
    let proof =
        OwnershipProof::capture(std::slice::from_ref(&status)).expect("complete live observation");
    assert!(proof.matches(std::slice::from_ref(&status)));
    assert!(proof.matches_local(&status));
    let change = state.begin_change();
    assert!(!proof.matches_local(&state.status()));
    state.finish_add(change, Some(tty(2)));
    let record = state.record();
    state.publication_succeeded(record.epoch);
    assert!(
        !proof.matches(&[state.status()]),
        "a new complete epoch still invalidates old proof"
    );
    assert!(
        !proof.matches(&[]),
        "do not drop a publisher from final observations"
    );
}

#[test]
fn incomplete_or_duplicate_live_publishers_cannot_form_a_proof() {
    let state = ready();
    let status = state.status();
    assert!(OwnershipProof::capture(&[status.clone(), status]).is_err());
    assert!(OwnershipProof::capture(&[OwnershipState::new(server()).status()]).is_err());
    assert!(
        OwnershipProof::capture(&[]).is_err(),
        "must include at least the local server"
    );
}

#[test]
fn live_endpoint_must_match_record_incarnation_and_epoch() {
    let state = ready();
    let record = state.record();
    let mut wrong = server();
    wrong.birth += 1;
    assert!(validate_record(&record, &state.status(), &wrong).is_err());
    let mut wrong = state.status();
    wrong.server.pid += 1;
    assert!(validate_record(&record, &wrong, &server()).is_err());
    let mut wrong = record.clone();
    wrong.complete = false;
    assert!(validate_record(&wrong, &state.status(), &server()).is_err());
}

#[test]
fn only_positive_death_or_reuse_is_ignored_not_unprovable_liveness() {
    let state = ready();
    assert_eq!(check_liveness(ServerLiveness::Dead), Ok(false));
    assert_eq!(check_liveness(ServerLiveness::Reused), Ok(false));
    assert!(check_liveness(ServerLiveness::Unknown).is_err());
    assert_eq!(
        check_liveness(ServerLiveness::Live(state.status().server)),
        Ok(true)
    );
}

#[test]
fn serialized_records_are_bounded_strict_and_device_identities_are_not_paths() {
    let state = ready();
    let bytes = serde_json::to_vec(&state.record()).expect("record JSON");
    assert!(parse_record(&bytes).is_ok());
    assert!(parse_record(&vec![b' '; MAX_RECORD_BYTES + 1]).is_err());
    let mut value = serde_json::to_value(state.record()).expect("record value");
    value["unexpected_authority"] = serde_json::json!(true);
    assert!(parse_record(&serde_json::to_vec(&value).expect("JSON")).is_err());
    let mut record = state.record();
    record.owned.push(record.owned[0].clone());
    assert!(parse_record(&serde_json::to_vec(&record).expect("JSON")).is_err());
    let mut record = state.record();
    record.version += 1;
    assert!(parse_record(&serde_json::to_vec(&record).expect("JSON")).is_err());
}

#[test]
fn tty_alias_uncertainty_is_conservative_not_a_false_nonownership_proof() {
    let owned = tty(1);
    let mut alias = owned.clone();
    alias.inode += 100;
    assert!(
        owned.may_match(&alias),
        "same controlling device via a different alias still denies"
    );
    assert!(!owned.may_match(&tty(2)));
}
