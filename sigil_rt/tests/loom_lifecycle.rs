//! Exhaustive small-state models for the lifecycle/accounting contracts used
//! by `ActorSender` and `Supervisor`.

use loom::sync::atomic::{AtomicUsize, Ordering};
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
