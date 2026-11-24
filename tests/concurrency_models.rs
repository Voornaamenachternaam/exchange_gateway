use exchange_gateway::jmap::MailboxDestroyOutcome;
use loom::sync::{Arc, Mutex};
use loom::thread;

#[derive(Debug)]
struct DeleteModel {
    backend_present: bool,
    mapping_present: bool,
    prune_intent: bool,
}

impl DeleteModel {
    fn live() -> Self {
        Self {
            backend_present: true,
            mapping_present: true,
            prune_intent: false,
        }
    }
}

/// Model the atomic storage boundaries in `delete_folder`: intent is recorded
/// before the backend call; a known-gone outcome prunes mapping and intent in
/// one transaction. `None` models a transport/method error whose outcome is
/// unknowable. Loom explores the interleavings; the real backend/SQLite path is
/// covered separately by eas_folders' integration tests.
fn delete_attempt(state: &Arc<Mutex<DeleteModel>>, outcome: Option<MailboxDestroyOutcome>) {
    if !state.lock().unwrap().mapping_present {
        return;
    }

    thread::yield_now();
    state.lock().unwrap().prune_intent = true;
    thread::yield_now();

    if outcome.is_some_and(MailboxDestroyOutcome::is_gone) {
        state.lock().unwrap().backend_present = false;
        thread::yield_now();
        let mut state = state.lock().unwrap();
        state.mapping_present = false;
        state.prune_intent = false;
    }
}

/// A successful destroy races a failed one. The WINNER is varied: with a
/// confirmed destroy the folder is gone, with a refusal it stays live — so the
/// live-folder branch is genuinely reachable and the invariant is checked in
/// both final states.
#[test]
fn loom_success_failure_delete_interleavings_never_drop_a_live_folder() {
    for winner in [
        MailboxDestroyOutcome::Destroyed,
        MailboxDestroyOutcome::Refused,
    ] {
        for failure in [Some(MailboxDestroyOutcome::Refused), None] {
            loom::model(move || {
                let state = Arc::new(Mutex::new(DeleteModel::live()));
                let destroyer = {
                    let state = Arc::clone(&state);
                    thread::spawn(move || {
                        delete_attempt(&state, Some(winner));
                    })
                };
                let failed = {
                    let state = Arc::clone(&state);
                    thread::spawn(move || delete_attempt(&state, failure))
                };
                destroyer.join().unwrap();
                failed.join().unwrap();

                let final_state = state.lock().unwrap();
                if final_state.backend_present {
                    assert!(
                        final_state.mapping_present,
                        "a live backend folder lost its mirror"
                    );
                    assert!(
                        final_state.prune_intent,
                        "a failed delete dropped the live folder's replay intent"
                    );
                } else {
                    assert!(
                        !final_state.mapping_present,
                        "a confirmed delete left the mirror advertised"
                    );
                    // A late failed attempt may record an orphan intent after the
                    // winner's prune. Reconciliation drops that harmless residue;
                    // it is not the unsafe loss of a live folder's intent.
                }
            });
        }
    }
}

#[test]
fn loom_failed_deletes_always_retain_live_mapping_and_intent() {
    loom::model(|| {
        let state = Arc::new(Mutex::new(DeleteModel::live()));
        let refused = {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                delete_attempt(&state, Some(MailboxDestroyOutcome::Refused));
            })
        };
        let unknown = {
            let state = Arc::clone(&state);
            thread::spawn(move || delete_attempt(&state, None))
        };
        refused.join().unwrap();
        unknown.join().unwrap();

        let final_state = state.lock().unwrap();
        assert!(final_state.backend_present);
        assert!(final_state.mapping_present);
        assert!(final_state.prune_intent);
    });
}

#[test]
fn already_gone_uses_the_same_confirmed_delete_transition() {
    loom::model(|| {
        let state = Arc::new(Mutex::new(DeleteModel::live()));
        delete_attempt(&state, Some(MailboxDestroyOutcome::AlreadyGone));
        let final_state = state.lock().unwrap();
        assert!(!final_state.backend_present);
        assert!(!final_state.mapping_present);
        assert!(!final_state.prune_intent);
    });
}
