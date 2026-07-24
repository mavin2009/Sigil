//! Bounded degradation/soak driver. It has no timing pass threshold: safety
//! assertions are deterministic, while the emitted distribution is evidence.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug)]
struct Message {
    sent: Instant,
}

fn percentile(sorted: &[u128], numerator: usize, denominator: usize) -> u128 {
    let index = sorted.len().saturating_sub(1).saturating_mul(numerator) / denominator;
    sorted[index]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "bounded soak evidence; run explicitly in the scheduled CI job"]
async fn bounded_overload_publishes_distributions() {
    const PRODUCERS: usize = 4;
    const PER_PRODUCER: usize = 5_000;
    const CAPACITY: usize = 64;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(CAPACITY);
    let queue_peak = Arc::new(AtomicUsize::new(0));
    let retries = Arc::new(AtomicU64::new(0));
    let consumer = tokio::spawn(async move {
        let mut latencies = Vec::with_capacity(PRODUCERS * PER_PRODUCER);
        while let Some(message) = rx.recv().await {
            latencies.push(message.sent.elapsed().as_micros());
            if latencies.len() % 64 == 0 {
                tokio::task::yield_now().await;
            }
        }
        latencies
    });

    let mut producers = Vec::new();
    for _ in 0..PRODUCERS {
        let tx = tx.clone();
        let peak = queue_peak.clone();
        let retries = retries.clone();
        producers.push(tokio::spawn(async move {
            for _ in 0..PER_PRODUCER {
                let mut message = Message {
                    sent: Instant::now(),
                };
                loop {
                    match tx.try_send(message) {
                        Ok(()) => {
                            let now = tx.max_capacity().saturating_sub(tx.capacity());
                            peak.fetch_max(now, Ordering::Relaxed);
                            break;
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                            retries.fetch_add(1, Ordering::Relaxed);
                            message = returned;
                            tokio::task::yield_now().await;
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            panic!("consumer disappeared before bounded soak completed")
                        }
                    }
                }
            }
        }));
    }
    drop(tx);
    for producer in producers {
        producer.await.expect("producer must not panic");
    }
    let shutdown_started = Instant::now();
    let mut latencies = consumer.await.expect("consumer must not panic");
    let shutdown_us = shutdown_started.elapsed().as_micros();
    latencies.sort_unstable();

    assert_eq!(latencies.len(), PRODUCERS * PER_PRODUCER);
    assert!(queue_peak.load(Ordering::Relaxed) <= CAPACITY);
    println!(
        "{{\"messages\":{},\"capacity\":{},\"estimated_queue_bytes\":{},\
         \"queue_peak\":{},\"retries\":{},\"latency_us\":{{\"p50\":{},\
         \"p95\":{},\"p99\":{},\"max\":{}}},\"shutdown_us\":{}}}",
        latencies.len(),
        CAPACITY,
        CAPACITY * std::mem::size_of::<Message>(),
        queue_peak.load(Ordering::Relaxed),
        retries.load(Ordering::Relaxed),
        percentile(&latencies, 50, 100),
        percentile(&latencies, 95, 100),
        percentile(&latencies, 99, 100),
        latencies.last().copied().unwrap_or_default(),
        shutdown_us,
    );
}
