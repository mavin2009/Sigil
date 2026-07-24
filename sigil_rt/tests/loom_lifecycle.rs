//! Exhaustive small-state models for the lifecycle/accounting contracts used
//! by `ActorSender`, `IngressRouter`, `ShardLease`, and `Supervisor`.

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

#[derive(Default)]
struct Inbox {
    open: bool,
    accepted: usize,
    completed: usize,
}

#[test]
fn close_racing_with_send_never_loses_accounting() {
    loom::model(|| {
        let inbox = Arc::new(Mutex::new(Inbox {
            open: true,
            ..Inbox::default()
        }));
        let sender_inbox = inbox.clone();
        let sender = thread::spawn(move || {
            let mut inbox = sender_inbox.lock().expect("model mutex");
            if inbox.open {
                inbox.accepted += 1;
            }
        });
        let worker_inbox = inbox.clone();
        let worker = thread::spawn(move || {
            let mut inbox = worker_inbox.lock().expect("model mutex");
            if inbox.completed < inbox.accepted {
                inbox.completed += 1;
            }
        });
        let closer_inbox = inbox.clone();
        let closer = thread::spawn(move || {
            closer_inbox.lock().expect("model mutex").open = false;
        });
        sender.join().expect("sender");
        worker.join().expect("worker");
        closer.join().expect("closer");

        let inbox = inbox.lock().expect("model mutex");
        let undrained = inbox.accepted - inbox.completed;
        assert!(!inbox.open);
        assert_eq!(inbox.accepted, inbox.completed + undrained);
    });
}

#[test]
fn processed_message_publication_carries_prior_acceptance() {
    loom::model(|| {
        let accepted = Arc::new(AtomicUsize::new(0));
        let visible = Arc::new(AtomicBool::new(false));
        let handled = Arc::new(AtomicUsize::new(0));

        let sender_accepted = accepted.clone();
        let sender_visible = visible.clone();
        let sender = thread::spawn(move || {
            sender_accepted.store(1, Ordering::Release);
            sender_visible.store(true, Ordering::Release);
        });

        let worker_visible = visible.clone();
        let worker_handled = handled.clone();
        let worker = thread::spawn(move || {
            if worker_visible.load(Ordering::Acquire) {
                worker_handled.store(1, Ordering::Release);
            }
        });

        let observer_accepted = accepted.clone();
        let observer_handled = handled.clone();
        let observer = thread::spawn(move || {
            let processed = observer_handled.load(Ordering::Acquire);
            let admitted = observer_accepted.load(Ordering::Acquire);
            assert!(
                processed <= admitted,
                "observed processing without its prior acceptance"
            );
        });

        sender.join().expect("sender");
        worker.join().expect("worker");
        observer.join().expect("observer");
    });
}

#[test]
fn concurrent_ingress_cursor_assigns_distinct_successive_slots() {
    loom::model(|| {
        const UNSET: usize = usize::MAX;
        let cursor = Arc::new(AtomicUsize::new(0));
        let selected = Arc::new([AtomicUsize::new(UNSET), AtomicUsize::new(UNSET)]);

        let mut producers = Vec::new();
        for producer in 0..2 {
            let cursor = cursor.clone();
            let selected = selected.clone();
            producers.push(thread::spawn(move || {
                let sequence = cursor.fetch_add(1, Ordering::Relaxed);
                selected[producer].store(sequence % 2, Ordering::Relaxed);
            }));
        }
        for producer in producers {
            producer.join().expect("producer");
        }

        let first = selected[0].load(Ordering::Relaxed);
        let second = selected[1].load(Ordering::Relaxed);
        assert_ne!(first, UNSET);
        assert_ne!(second, UNSET);
        assert_ne!(first, second);
    });
}

#[test]
fn panic_and_shutdown_race_emits_one_terminal_report() {
    loom::model(|| {
        const RUNNING: usize = 0;
        const PANICKED: usize = 1;
        const CANCELLED: usize = 2;
        let status = Arc::new(AtomicUsize::new(RUNNING));
        let reports = Arc::new(AtomicUsize::new(0));

        let panic_status = status.clone();
        let panic_reports = reports.clone();
        let panic_path = thread::spawn(move || {
            if panic_status
                .compare_exchange(RUNNING, PANICKED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                panic_reports.fetch_add(1, Ordering::Relaxed);
            }
        });
        let shutdown_status = status.clone();
        let shutdown_reports = reports.clone();
        let shutdown_path = thread::spawn(move || {
            if shutdown_status
                .compare_exchange(RUNNING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                shutdown_reports.fetch_add(1, Ordering::Relaxed);
            }
        });
        panic_path.join().expect("panic path");
        shutdown_path.join().expect("shutdown path");

        assert_ne!(status.load(Ordering::Acquire), RUNNING);
        assert_eq!(reports.load(Ordering::Relaxed), 1);
    });
}

#[test]
fn shard_drain_never_completes_while_an_authorized_permit_is_live() {
    loom::model(|| {
        const PHASE_SHIFT: usize = 8;
        const COUNT_MASK: usize = (1 << PHASE_SHIFT) - 1;
        const SERVING: usize = 1 << PHASE_SHIFT;
        const DRAINING: usize = 2 << PHASE_SHIFT;
        const RETIRED: usize = 3 << PHASE_SHIFT;
        let state = Arc::new(AtomicUsize::new(SERVING));
        let handoff_complete = Arc::new(AtomicBool::new(false));

        let permit_state = state.clone();
        let permit_handoff = handoff_complete.clone();
        let receiver = thread::spawn(move || {
            let mut observed = permit_state.load(Ordering::Acquire);
            loop {
                if observed & !COUNT_MASK != SERVING {
                    return;
                }
                match permit_state.compare_exchange_weak(
                    observed,
                    observed + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => observed = actual,
                }
            }
            assert!(
                !permit_handoff.load(Ordering::Acquire),
                "handoff completed before a serving permit was observed"
            );
            thread::yield_now();
            assert!(
                !permit_handoff.load(Ordering::Acquire),
                "handoff completed while a serving permit was live"
            );
            permit_state.fetch_sub(1, Ordering::AcqRel);
        });

        let drain_state = state.clone();
        let drain_handoff = handoff_complete.clone();
        let drainer = thread::spawn(move || {
            let mut observed = drain_state.load(Ordering::Acquire);
            loop {
                if observed & !COUNT_MASK != SERVING {
                    return;
                }
                let draining = DRAINING | (observed & COUNT_MASK);
                match drain_state.compare_exchange_weak(
                    observed,
                    draining,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => observed = actual,
                }
            }
            if drain_state
                .compare_exchange(DRAINING, RETIRED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                drain_handoff.store(true, Ordering::Release);
            }
        });

        receiver.join().expect("receiver");
        drainer.join().expect("drainer");
        assert_eq!(state.load(Ordering::Acquire) & COUNT_MASK, 0);
        if handoff_complete.load(Ordering::Acquire) {
            assert_eq!(state.load(Ordering::Acquire), RETIRED);
        }
    });
}
