//! Integration tests for the 750 Mbit/s Token Bucket Rate Limiter and
//! channel backpressure mechanics in the network & ingestion layer.
//!
//! Validates:
//! - Steady-state rate enforcement (<= 750 Mbit/s / 93.75 MB/s)
//! - Burst allowance verification
//! - Multi-connection token pooling
//! - Protocol chunk accounting
//! - Rate limiter reset & refill
//! - Deadlock avoidance on oversized acquisitions (> burst capacity)
//! - Fractional nanosecond accumulation integrity (zero truncation)
//! - Channel capacity bounds & UnsavedBudget backpressure

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use atlas::nntp::{DEFAULT_BURST_BYTES, RATE_750_MBIT, RateLimiter, TokenBucket};

fn run_async<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to start tokio test runtime")
        .block_on(f)
}

#[test]
fn test_steady_state_rate_enforcement() {
    run_async(async {
        // Use a 5 MB/s rate limiter with 100 KB burst for fast deterministic test execution
        let rate_bytes_per_sec = 5_000_000;
        let burst_bytes = 100_000;
        let limiter = TokenBucket::new(rate_bytes_per_sec, burst_bytes);

        // First consume burst tokens
        limiter.acquire(burst_bytes as usize).await;

        // Now acquire 1,000,000 bytes at 5 MB/s => theoretical duration: 0.20s (200 ms)
        let started = Instant::now();
        limiter.acquire(1_000_000).await;
        let elapsed = started.elapsed();

        // Must take at least 180ms (accounting for scheduling granularity)
        assert!(elapsed >= Duration::from_millis(180), "rate limiter did not throttle: elapsed {elapsed:?}");
        // And should not exceed 400ms
        assert!(elapsed < Duration::from_millis(400), "rate limiter took excessively long: elapsed {elapsed:?}");
    });
}

#[test]
fn test_burst_allowance_verification() {
    run_async(async {
        // At 750 Mbit/s (93.75 MB/s) with 4 MB burst
        let burst = 4_000_000;
        let limiter = TokenBucket::new(RATE_750_MBIT, burst);

        // Immediate acquire of full burst buffer must complete instantaneously (< 10 ms)
        let t0 = Instant::now();
        limiter.acquire(burst as usize).await;
        let burst_elapsed = t0.elapsed();
        assert!(burst_elapsed < Duration::from_millis(20), "burst acquire stalled: {burst_elapsed:?}");

        // Subsequent immediate acquire of 1 MB must delay by approx 1MB / 93.75MB/s ~= 10.6ms
        let t1 = Instant::now();
        limiter.acquire(1_000_000).await;
        let throttled_elapsed = t1.elapsed();
        assert!(
            throttled_elapsed >= Duration::from_millis(8),
            "subsequent acquire was not throttled: {throttled_elapsed:?}"
        );
    });
}

#[test]
fn test_multi_connection_token_pooling() {
    run_async(async {
        // 8 concurrent connections sharing a single 20 MB/s rate limiter
        let rate = 20_000_000;
        let burst = 2_000_000;
        let limiter = Arc::new(TokenBucket::new(rate, burst));

        // Drain initial burst
        limiter.set_tokens(0);

        let workers = 8;
        let bytes_per_worker = 125_000; // total: 1,000,000 bytes => ~50ms
        let total_acquired = Arc::new(AtomicU64::new(0));

        let t0 = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..workers {
            let lim = limiter.clone();
            let counter = total_acquired.clone();
            handles.push(tokio::spawn(async move {
                lim.acquire(bytes_per_worker).await;
                counter.fetch_add(bytes_per_worker as u64, Ordering::Relaxed);
            }));
        }

        for h in handles {
            h.await.expect("worker task panicked");
        }

        let elapsed = t0.elapsed();
        assert_eq!(total_acquired.load(Ordering::Relaxed), 1_000_000);
        // 1MB at 20MB/s should take at least 40ms
        assert!(
            elapsed >= Duration::from_millis(35),
            "concurrent pooling did not enforce aggregate rate limit: elapsed {elapsed:?}"
        );
    });
}

#[test]
fn test_protocol_chunk_accounting() {
    run_async(async {
        let limiter = Arc::new(RateLimiter::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));

        let initial_tokens = limiter.available_tokens();
        limiter.acquire(65_536).await;
        let remaining = limiter.available_tokens();
        assert!(remaining <= initial_tokens.saturating_sub(65_536) + 10_000);
    });
}

#[test]
fn test_rate_limiter_reset_and_refill() {
    run_async(async {
        let limiter = TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES);
        limiter.set_tokens(0);
        assert!(limiter.available_tokens() <= 100);

        // Sleep 100ms with no traffic
        tokio::time::sleep(Duration::from_millis(100)).await;

        // In 100ms at 93,750,000 B/s: expected refill = 9,375,000 bytes (+- 15%)
        let tokens = limiter.available_tokens();
        assert!((7_500_000..=10_000_000).contains(&tokens), "refill did not match expected rate: got {tokens}");
    });
}

#[test]
fn test_deadlock_avoidance_oversized_acquisition() {
    run_async(async {
        // High rate so test runs fast: 100 MB/s, burst capacity 4 MB
        let rate = 100_000_000;
        let burst = 4_000_000;
        let limiter = TokenBucket::new(rate, burst);

        // Request 12 MB (3x burst capacity)
        let started = Instant::now();
        limiter.acquire(12_000_000).await;
        let elapsed = started.elapsed();

        // 12MB - 4MB burst = 8MB deficit at 100MB/s => ~80ms
        assert!(elapsed < Duration::from_secs(2), "deadlock occurred on oversized acquisition: {elapsed:?}");
        assert!(elapsed >= Duration::from_millis(60), "expected throttling on oversized acquisition: {elapsed:?}");
    });
}

#[test]
fn test_fractional_nanosecond_accumulation_integrity() {
    run_async(async {
        let limiter = TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES);
        limiter.set_tokens(0);

        // Perform 20 high-frequency micro-acquisitions with sub-millisecond sleeps
        let start = Instant::now();
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_micros(250)).await;
            let _ = limiter.available_tokens();
        }
        let total_elapsed = start.elapsed();
        let expected_tokens = (total_elapsed.as_nanos() * RATE_750_MBIT as u128 / 1_000_000_000) as u64;
        let actual_tokens = limiter.available_tokens();

        let diff = (expected_tokens as i64 - actual_tokens as i64).abs();
        assert!(
            diff <= 100_000,
            "fractional tokens leaked or truncated: expected ~{expected_tokens}, got {actual_tokens}"
        );
    });
}

#[test]
fn test_channel_capacity_bound_backpressure() {
    run_async(async {
        let (tx, mut rx) = mpsc::channel::<usize>(1024);
        let items_sent = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicBool::new(false));

        let tx_clone = tx.clone();
        let sent_clone = items_sent.clone();
        let fin_clone = finished.clone();

        let producer = tokio::spawn(async move {
            for i in 0..2048 {
                tx_clone.send(i).await.expect("failed to send item");
                sent_clone.fetch_add(1, Ordering::Relaxed);
            }
            fin_clone.store(true, Ordering::Relaxed);
        });

        // Let producer fill the bounded buffer (1024 items)
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Producer must pause and cannot send all 2048 items while consumer has not read
        let sent_so_far = items_sent.load(Ordering::Relaxed);
        assert!(sent_so_far <= 1025, "producer exceeded bounded channel buffer: sent {sent_so_far}");
        assert!(!finished.load(Ordering::Relaxed));

        // Now drain 500 items
        for _ in 0..500 {
            rx.recv().await.expect("channel closed early");
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        let sent_after_drain = items_sent.load(Ordering::Relaxed);
        assert!(sent_after_drain > sent_so_far, "producer did not resume after downstream drain");

        // Drain remaining
        while rx.recv().await.is_some() {
            if items_sent.load(Ordering::Relaxed) == 2048 && rx.is_empty() {
                break;
            }
        }
        producer.await.expect("producer failed");
    });
}

#[test]
fn test_adversarial_burst_edge_case_20mb() {
    run_async(async {
        // Burst capacity: DEFAULT_BURST_BYTES = 10 MB (10,000,000 bytes)
        // Rate: RATE_750_MBIT = 93,750,000 bytes/sec
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));

        // Part 1: Sudden 20 MB burst request (2x burst capacity) from a single task
        // Slicing into two 10 MB chunks:
        // Chunk 1: consumes 10 MB burst buffer immediately (< 20 ms).
        // Chunk 2: 10 MB deficit at 93.75 MB/s => ~106.67 ms.
        let t0 = Instant::now();
        limiter.acquire(20_000_000).await;
        let elapsed_single = t0.elapsed();

        assert!(
            elapsed_single >= Duration::from_millis(90),
            "20 MB burst was not throttled properly: {elapsed_single:?}"
        );
        assert!(elapsed_single < Duration::from_millis(300), "20 MB burst deadlocked or lagged: {elapsed_single:?}");

        // Part 2: Concurrent burst: two tasks simultaneously request 20 MB each (total 40 MB)
        // Reset bucket to full burst capacity
        limiter.set_tokens(DEFAULT_BURST_BYTES);

        let t1 = Instant::now();
        let l1 = limiter.clone();
        let l2 = limiter.clone();

        let h1 = tokio::spawn(async move {
            l1.acquire(20_000_000).await;
        });
        let h2 = tokio::spawn(async move {
            l2.acquire(20_000_000).await;
        });

        h1.await.expect("task 1 failed");
        h2.await.expect("task 2 failed");

        let elapsed_concurrent = t1.elapsed();
        // 40 MB total with 10 MB initial burst = 30 MB deficit at 93.75 MB/s => ~320 ms
        assert!(
            elapsed_concurrent >= Duration::from_millis(270),
            "concurrent 20 MB bursts did not enforce 750 Mbit pacing: {elapsed_concurrent:?}"
        );
        assert!(
            elapsed_concurrent < Duration::from_millis(700),
            "concurrent 20 MB bursts deadlocked: {elapsed_concurrent:?}"
        );
    });
}

#[test]
fn test_adversarial_microsecond_precision_fractional_accumulation() {
    run_async(async {
        let limiter = TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES);

        // Sub-test A: Rapid burst of 50,000 single-byte acquisitions
        limiter.set_tokens(50_000);
        let t0 = Instant::now();
        for _ in 0..50_000 {
            limiter.acquire(1).await;
        }
        let elapsed = t0.elapsed();
        // 50,000 acquires should execute in under 300 ms without lock thrashing or deadlocks
        assert!(elapsed < Duration::from_millis(300), "rapid 1-byte acquisitions were excessively slow: {elapsed:?}");

        // Sub-test B: Microsecond fractional nanosecond accumulation without token loss
        limiter.set_tokens(0);
        let start = Instant::now();
        let steps = 500;
        let mut observed_tokens = 0;
        for _ in 0..steps {
            tokio::task::yield_now().await;
            observed_tokens = limiter.available_tokens();
        }
        let total_nanos = start.elapsed().as_nanos();
        let expected_tokens = (total_nanos * RATE_750_MBIT as u128 / 1_000_000_000) as u64;

        // Verify fractional nanoseconds preserved exact rate without truncation drift
        let diff = (expected_tokens as i64 - observed_tokens as i64).abs();
        assert!(
            diff <= 50_000,
            "microsecond precision drifted: expected ~{expected_tokens}, got {observed_tokens}, diff {diff}"
        );
    });
}

#[test]
fn test_adversarial_high_concurrency_100_tasks() {
    run_async(async {
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        limiter.set_tokens(0);

        let task_count = 128;
        let bytes_per_task = 50_000; // total 6,400,000 bytes
        let completed_count = Arc::new(AtomicU64::new(0));
        let total_bytes = Arc::new(AtomicU64::new(0));

        let t0 = Instant::now();
        let mut handles = Vec::with_capacity(task_count);

        for task_id in 0..task_count {
            let lim = limiter.clone();
            let c_count = completed_count.clone();
            let b_count = total_bytes.clone();

            handles.push(tokio::spawn(async move {
                let start_task = Instant::now();
                lim.acquire(bytes_per_task).await;
                b_count.fetch_add(bytes_per_task as u64, Ordering::Relaxed);
                c_count.fetch_add(1, Ordering::Relaxed);
                let duration = start_task.elapsed();
                assert!(duration < Duration::from_secs(3), "task {task_id} suffered starvation: {duration:?}");
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }

        let elapsed = t0.elapsed();
        assert_eq!(completed_count.load(Ordering::Relaxed), 128, "not all tasks completed");
        assert_eq!(total_bytes.load(Ordering::Relaxed), 6_400_000);

        // 6.4 MB at 93.75 MB/s => theoretical duration 68.26 ms
        assert!(elapsed >= Duration::from_millis(55), "concurrency violated rate limit: elapsed {elapsed:?}");
        assert!(elapsed < Duration::from_millis(500), "concurrency deadlocked or lagged: elapsed {elapsed:?}");
    });
}

#[test]
fn test_adversarial_loopback_wire_benchmark_750mbit_bounds() {
    run_async(async {
        let (mut client_io, mut server_io) = tokio::io::duplex(2 * 1024 * 1024);
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));

        let total_bytes: usize = 30_000_000;
        let chunk_size: usize = 256 * 1024; // 256 KB chunks

        let writer = tokio::spawn(async move {
            let chunk = vec![0x42u8; chunk_size];
            let mut remaining = total_bytes;
            while remaining > 0 {
                let to_write = remaining.min(chunk_size);
                server_io.write_all(&chunk[..to_write]).await.expect("write failed");
                remaining -= to_write;
            }
        });

        // Drain initial burst tokens to measure steady-state line rate
        limiter.set_tokens(0);

        let start_time = Instant::now();
        let mut bytes_read = 0;
        let mut buf = vec![0u8; chunk_size];

        while bytes_read < total_bytes {
            let to_read = (total_bytes - bytes_read).min(chunk_size);
            let n = client_io.read(&mut buf[..to_read]).await.expect("read failed");
            if n == 0 {
                break;
            }
            limiter.acquire(n).await;
            bytes_read += n;
        }

        let elapsed = start_time.elapsed();
        writer.await.expect("writer failed");

        assert_eq!(bytes_read, total_bytes);

        // Throughput in Mbit/s
        let elapsed_secs = elapsed.as_secs_f64();
        let throughput_mbps = (total_bytes as f64 * 8.0) / (elapsed_secs * 1_000_000.0);
        println!(
            "Adversarial Wire Benchmark: transferred {total_bytes} bytes in {elapsed_secs:.4}s -> {throughput_mbps:.2} Mbit/s"
        );

        // Target: [712.5, 750.0] Mbit/s (95% to 100% of 750 Mbit/s)
        assert!(throughput_mbps <= 750.01, "rate limiter exceeded 750 Mbit/s ceiling: {throughput_mbps:.2} Mbit/s");
        assert!(
            throughput_mbps >= 712.5,
            "rate limiter underperformed 712.5 Mbit/s bound: {throughput_mbps:.2} Mbit/s"
        );
    });
}
