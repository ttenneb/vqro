//! Generation reducer controls, not a native-process or account qualification.
use super::*;
use crate::native_process::{Event, Exit};
#[test]
fn old_process_observations_cannot_mutate_new_epoch() {
    let shared = Shared::new("boot".into(), "fixture".into(), "instance".into());
    {
        let mut s = shared.state.lock().unwrap();
        shared.generation.store(2, Ordering::SeqCst);
        s.replace_owned(json!({"id":"B"}), 2);
    }
    let epoch = shared.state.lock().unwrap().epoch().to_owned();
    shared.observe_native(
        1,
        &Event::Record(json!({"type":"system","subtype":"init","session_id":"OLD"})),
    );
    shared.observe_native(
        1,
        &Event::Exited(Exit {
            code: Some(0),
            success: true,
        }),
    );
    shared.observe_native(1, &Event::ProtocolError("old pipe".into()));
    {
        let s = shared.state.lock().unwrap();
        assert_eq!(s.epoch(), epoch);
        assert!(s.session.is_none());
        assert!(s.exit.is_none());
        assert!(!s.incomplete.contains("native_protocol"));
    }
    shared.observe_native(
        2,
        &Event::Record(json!({"type":"system","subtype":"init","session_id":"NEW"})),
    );
    assert_eq!(shared.state.lock().unwrap().session.as_deref(), Some("NEW"));
}
