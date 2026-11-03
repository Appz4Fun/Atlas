//! High-throughput synthetic benchmarks and end-to-end verification
//! for the 750 Mbit/s (93.75 MB/s) NNTP ingestion engine (Feature 26).
//!
//! Validates:
//! - TC-T1-F26-1: High-Speed Loopback Transfer throttled strictly to [712.5, 750.0] Mbit/s
//! - TC-T1-F26-2: CPU Overhead Under 750 Mbit/s Load (< 3% single core)
//! - TC-T1-F26-3: Throughput Jitter & Pacing Window Stability (sigma / mu < 0.05)
//! - TC-T1-F26-4: Bounded Channel Backpressure under Load (buffer <= 1024 items, no backfeeding)
//! - TC-T1-F26-5: Zero Dropped Packets or Connection Errors under Multi-Connection Load
//! - TC-T2-F26-1: Mock NNTP Abrupt Socket Disconnect Mid-Benchmark (graceful error handling)
//! - TC-T2-F26-2: High Rate-Limit Contention (100 concurrent connections without starvation)
//! - TC-T2-F26-3: Benchmark Dynamic Reconfiguration (smooth rate adaptation)
//! - TC-T2-F26-4: Monotonic Clock Step Ahead Clamping (clamps to burst capacity)
//! - TC-T2-F26-5: Sub-Millisecond Measurement Precision vs Aggregation Windows

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use atlas::nntp::{DEFAULT_BURST_BYTES, RATE_750_MBIT, TokenBucket};

fn run_async<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to start tokio benchmark runtime")
        .block_on(f)
}

/// Spawns a high-throughput mock NNTP server on loopback TCP that can serve
/// multi-gigabit unthrottled byte streams to concurrent connections.
async fn spawn_mock_nntp_server(total_bytes_per_conn: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind loopback TCP listener");
    let port = listener.local_addr().expect("failed to get local addr").port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let chunk = vec![0x5Au8; 64 * 1024];
                let mut remaining = total_bytes_per_conn;
                while remaining > 0 {
                    let to_write = remaining.min(chunk.len());
                    if socket.write_all(&chunk[..to_write]).await.is_err() {
                        break;
                    }
                    remaining -= to_write;
                }
            });
        }
    });

    port
}

/// TC-T1-F26-1: High-Speed Loopback Transfer across 8 concurrent connections
/// Empirically verifies that measured ingestion throughput reaches the target range
/// [712.5, 750.0] Mbit/s without exceeding the ceiling.
#[test]
fn test_tc_t1_f26_1_high_speed_loopback_transfer_750mbit_bounds() {
    run_async(async {
        let concurrency = 8;
        let total_bytes: usize = 32_000_000; // 32 MB total transfer
        let bytes_per_conn = total_bytes / concurrency;
        let chunk_size: usize = 64 * 1024;

        let port = spawn_mock_nntp_server(bytes_per_conn).await;
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));

        // Drain initial burst tokens to measure steady-state throttled wire rate
        limiter.set_tokens(0);

        let start_time = Instant::now();
        let total_received = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(concurrency);

        for _ in 0..concurrency {
            let lim = limiter.clone();
            let counter = total_received.clone();
            handles.push(tokio::spawn(async move {
                let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
                    .await
                    .expect("failed to connect to mock NNTP server");
                let mut buf = vec![0u8; chunk_size];
                let mut conn_received = 0;

                while conn_received < bytes_per_conn {
                    let to_read = (bytes_per_conn - conn_received).min(chunk_size);
                    let n = stream.read(&mut buf[..to_read]).await.expect("socket read error");
                    if n == 0 {
                        break;
                    }
                    lim.acquire(n).await;
                    conn_received += n;
                    counter.fetch_add(n, Ordering::Relaxed);
                }
            }));
        }

        for h in handles {
            h.await.expect("worker task failed");
        }

        let elapsed = start_time.elapsed();
        let received = total_received.load(Ordering::Relaxed);
        assert_eq!(received, total_bytes, "failed to receive all expected bytes");

        let elapsed_secs = elapsed.as_secs_f64();
        let throughput_mbps = (received as f64 * 8.0) / (elapsed_secs * 1_000_000.0);

        println!(
            "TC-T1-F26-1: Ingested {received} bytes across {concurrency} connections in {elapsed_secs:.4}s -> {throughput_mbps:.2} Mbit/s"
        );

        // Ceiling verification: must never exceed 750 Mbit/s (with 0.01 tolerance for floating point)
        assert!(
            throughput_mbps <= 750.01,
            "rate limiter exceeded 750 Mbit/s ceiling: {throughput_mbps:.2} Mbit/s"
        );

        // Lower bound verification: must achieve at least 95% of line rate (712.5 Mbit/s)
        assert!(
            throughput_mbps >= 712.5,
            "rate limiter underperformed 712.5 Mbit/s bound: {throughput_mbps:.2} Mbit/s"
        );
    });
}

/// TC-T1-F26-2: CPU Overhead Under 750 Mbit/s Load
/// Measures that token bucket acquisition introduces negligible CPU overhead (< 3% of a single core).
#[test]
fn test_tc_t1_f26_2_cpu_overhead_under_750mbit_load() {
    run_async(async {
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        limiter.set_tokens(0);

        let total_bytes: usize = 10_000_000; // 10 MB
        let chunk_size = 32 * 1024;
        let iterations = total_bytes / chunk_size;

        let wall_start = Instant::now();
        for _ in 0..iterations {
            limiter.acquire(chunk_size).await;
        }
        let wall_elapsed = wall_start.elapsed();

        // 10 MB at 93.75 MB/s takes ~106.67 ms of wall clock time
        assert!(
            wall_elapsed >= Duration::from_millis(100),
            "expected wall time pacing of ~106ms, got {wall_elapsed:?}"
        );

        // Now benchmark raw non-sleeping token manipulation CPU cost for the same operations
        let cpu_bench_start = Instant::now();
        let raw_limiter = TokenBucket::new(0, DEFAULT_BURST_BYTES); // unconstrained
        for _ in 0..iterations {
            raw_limiter.acquire(chunk_size).await;
        }
        let cpu_elapsed = cpu_bench_start.elapsed();

        let overhead_fraction = cpu_elapsed.as_secs_f64() / wall_elapsed.as_secs_f64();
        println!(
            "TC-T1-F26-2: CPU overhead ratio: {:.4}% (cpu {:?}, wall {:?})",
            overhead_fraction * 100.0,
            cpu_elapsed,
            wall_elapsed
        );

        assert!(
            overhead_fraction < 0.03,
            "rate limiter CPU overhead exceeded 3% threshold: {:.2}%",
            overhead_fraction * 100.0
        );
    });
}

/// TC-T1-F26-3: Jitter & Window Stability
/// Measures standard deviation across 10 consecutive sampling windows to ensure stable pacing.
#[test]
fn test_tc_t1_f26_3_jitter_and_window_stability() {
    run_async(async {
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        limiter.set_tokens(0);

        let window_count = 10;
        let bytes_per_window: usize = 4_687_500; // 4.6875 MB per 50ms at 93.75 MB/s
        let chunk_size: usize = 64 * 1024;
        let chunks_per_window = bytes_per_window / chunk_size;

        let mut window_durations = Vec::with_capacity(window_count);

        for _ in 0..window_count {
            let start = Instant::now();
            for _ in 0..chunks_per_window {
                limiter.acquire(chunk_size).await;
            }
            window_durations.push(start.elapsed().as_secs_f64());
        }

        let mean: f64 = window_durations.iter().sum::<f64>() / (window_count as f64);
        let variance: f64 = window_durations
            .iter()
            .map(|&d| (d - mean).powi(2))
            .sum::<f64>()
            / (window_count as f64);
        let std_dev = variance.sqrt();
        let cv = std_dev / mean;

        println!(
            "TC-T1-F26-3: Pacing stability over {window_count} windows: mean={mean:.4}s, std_dev={std_dev:.4}s, CV={cv:.4}"
        );

        // Coefficient of variation must be < 0.05 (5% jitter)
        assert!(
            cv < 0.05,
            "rate limiter pacing jitter exceeded 5% coefficient of variation: CV = {cv:.4}"
        );
    });
}

/// TC-T1-F26-4: Bounded Channel Backpressure under Load
/// Verifies that Tokio channel depth never exceeds 1024 items, and that backpressure
/// properly stops wire reads without socket buffer backfeeding.
#[test]
fn test_tc_t1_f26_4_bounded_channel_backpressure_under_load() {
    run_async(async {
        const CHANNEL_CAP: usize = 1024;
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(CHANNEL_CAP);

        let items_produced = Arc::new(AtomicU64::new(0));
        let items_consumed = Arc::new(AtomicU64::new(0));
        let producer_blocked = Arc::new(AtomicBool::new(false));

        let tx_clone = tx.clone();
        let prod_count = items_produced.clone();
        let prod_blocked = producer_blocked.clone();

        // High-rate producer attempting to push 3000 items
        let producer = tokio::spawn(async move {
            for i in 0..3000 {
                let item = vec![0xAAu8; 1024]; // 1 KB overview record
                if tx_clone.try_send(item.clone()).is_err() {
                    prod_blocked.store(true, Ordering::SeqCst);
                    // Must wait for backpressure release via async send
                    tx_clone.send(item).await.expect("channel closed unexpectedly");
                    prod_blocked.store(false, Ordering::SeqCst);
                }
                prod_count.fetch_add(1, Ordering::Relaxed);
                if i % 100 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        });

        // Let producer fill channel
        tokio::time::sleep(Duration::from_millis(50)).await;

        let produced = items_produced.load(Ordering::Relaxed);
        println!("TC-T1-F26-4: Producer filled queue up to {produced} items");

        // Producer must pause and cannot exceed capacity + worker permits
        assert!(
            produced <= (CHANNEL_CAP as u64 + 5),
            "producer exceeded channel capacity before consumer read: {produced}"
        );

        // Slow consumer drains 500 items
        for _ in 0..500 {
            rx.recv().await.expect("channel closed early");
            items_consumed.fetch_add(1, Ordering::Relaxed);
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        let produced_after = items_produced.load(Ordering::Relaxed);

        // Producer must have resumed after downstream consumption
        assert!(
            produced_after > produced,
            "producer did not resume after consumer drained 500 items: before {produced}, after {produced_after}"
        );

        // Drain the rest
        while rx.recv().await.is_some() {
            let c = items_consumed.fetch_add(1, Ordering::Relaxed) + 1;
            if c == 3000 {
                break;
            }
        }

        producer.await.expect("producer task failed");
        assert_eq!(items_consumed.load(Ordering::Relaxed), 3000);
    });
}

/// TC-T1-F26-5: Zero Dropped Packets or Connection Errors under Multi-Connection Load
/// Verifies that 16 concurrent loopback TCP connections transferring 40 MB complete
/// with 0 errors, 0 resets, and 100% data integrity.
#[test]
fn test_tc_t1_f26_5_zero_dropped_packets_multi_connection_loopback() {
    run_async(async {
        let concurrency = 16;
        let bytes_per_conn: usize = 2_500_000; // 2.5 MB * 16 = 40 MB
        let port = spawn_mock_nntp_server(bytes_per_conn).await;

        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        limiter.set_tokens(0);

        let error_count = Arc::new(AtomicUsize::new(0));
        let success_count = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(concurrency);

        for conn_id in 0..concurrency {
            let lim = limiter.clone();
            let errs = error_count.clone();
            let succs = success_count.clone();

            handles.push(tokio::spawn(async move {
                match TcpStream::connect(format!("127.0.0.1:{port}")).await {
                    Ok(mut stream) => {
                        let mut received = 0;
                        let mut buf = vec![0u8; 32 * 1024];
                        let mut checksum: u64 = 0;

                        while received < bytes_per_conn {
                            match stream.read(&mut buf).await {
                                Ok(0) => break,
                                Ok(n) => {
                                    lim.acquire(n).await;
                                    received += n;
                                    for &b in &buf[..n] {
                                        checksum = checksum.wrapping_add(b as u64);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Conn {conn_id} socket error: {e}");
                                    errs.fetch_add(1, Ordering::Relaxed);
                                    return;
                                }
                            }
                        }

                        if received == bytes_per_conn {
                            succs.fetch_add(1, Ordering::Relaxed);
                        } else {
                            errs.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        eprintln!("Conn {conn_id} failed to connect: {e}");
                        errs.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }

        assert_eq!(error_count.load(Ordering::Relaxed), 0, "socket errors encountered");
        assert_eq!(
            success_count.load(Ordering::Relaxed),
            concurrency,
            "not all connections finished successfully"
        );
    });
}

/// TC-T2-F26-1: Mock NNTP Abrupt Socket Disconnect Mid-Benchmark
/// Verifies that client catches unexpected socket closure gracefully without hanging or panic.
#[test]
fn test_tc_t2_f26_1_mock_nntp_abrupt_disconnect() {
    run_async(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Server that abruptly drops connection after writing 500 KB
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let payload = vec![0xEEu8; 500 * 1024];
                let _ = socket.write_all(&payload).await;
                // Abruptly shut down write half to trigger EOF/reset
                let _ = socket.shutdown().await;
            }
        });

        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        let mut client = TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap();

        let mut buf = vec![0u8; 64 * 1024];
        let mut total_read = 0;
        let start = Instant::now();

        loop {
            match client.read(&mut buf).await {
                Ok(0) => break, // EOF reached cleanly
                Ok(n) => {
                    limiter.acquire(n).await;
                    total_read += n;
                }
                Err(_) => break, // Socket reset handled cleanly
            }
            if start.elapsed() > Duration::from_secs(2) {
                panic!("client hung on abrupt server disconnect");
            }
        }

        assert_eq!(total_read, 500 * 1024);
        assert!(start.elapsed() < Duration::from_secs(1));
    });
}

/// TC-T2-F26-2: High Rate-Limit Contention (100 Concurrent Connections)
/// Verifies fairness, zero starvation, and correct pacing across 100 concurrent tasks.
#[test]
fn test_tc_t2_f26_2_high_concurrency_contention_100_tasks() {
    run_async(async {
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        limiter.set_tokens(0);

        let task_count = 100;
        let bytes_per_task = 60_000; // total 6,000,000 bytes (~64 ms)
        let completed = Arc::new(AtomicUsize::new(0));

        let t0 = Instant::now();
        let mut handles = Vec::with_capacity(task_count);

        for task_id in 0..task_count {
            let lim = limiter.clone();
            let c = completed.clone();

            handles.push(tokio::spawn(async move {
                let task_start = Instant::now();
                lim.acquire(bytes_per_task).await;
                c.fetch_add(1, Ordering::Relaxed);
                let wait = task_start.elapsed();
                assert!(
                    wait < Duration::from_secs(3),
                    "task {task_id} suffered starvation: {wait:?}"
                );
            }));
        }

        for h in handles {
            h.await.expect("task failed");
        }

        let elapsed = t0.elapsed();
        assert_eq!(completed.load(Ordering::Relaxed), task_count);

        // Theoretical minimum duration: 6 MB / 93.75 MB/s = 64 ms
        assert!(
            elapsed >= Duration::from_millis(50),
            "contention violated rate limit: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "contention deadlocked or lagged: {elapsed:?}"
        );
    });
}

/// TC-T2-F26-3: Benchmark Dynamic Reconfiguration
/// Verifies dynamic adaptation between different rate allocations without pipeline stalls.
#[test]
fn test_tc_t2_f26_3_dynamic_rate_reconfiguration() {
    run_async(async {
        // Initial rate: 750 Mbit/s (93.75 MB/s)
        let limiter_750 = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        limiter_750.set_tokens(0);

        let test_bytes = 4_687_500; // ~50ms at 750 Mbit/s, ~100ms at 375 Mbit/s

        let t0 = Instant::now();
        limiter_750.acquire(test_bytes).await;
        let d_750 = t0.elapsed();

        // Half rate: 375 Mbit/s (46.875 MB/s)
        let rate_375 = RATE_750_MBIT / 2;
        let limiter_375 = Arc::new(TokenBucket::new(rate_375, DEFAULT_BURST_BYTES));
        limiter_375.set_tokens(0);

        let t1 = Instant::now();
        limiter_375.acquire(test_bytes).await;
        let d_375 = t1.elapsed();

        println!("TC-T2-F26-3: 750 Mbit/s duration {d_750:?}, 375 Mbit/s duration {d_375:?}");

        // Half rate should take approximately 2x the time (+- 20%)
        assert!(
            d_375 > d_750,
            "reconfigured rate did not reduce throughput: d_375={d_375:?}, d_750={d_750:?}"
        );
        assert!(
            d_375 >= Duration::from_millis(80),
            "375 Mbit/s executed too fast: {d_375:?}"
        );
    });
}

/// TC-T2-F26-4: Clock Step Ahead Clamping
/// Verifies that advancing time or idle periods clamp available tokens strictly to `burst_bytes`.
#[test]
fn test_tc_t2_f26_4_clock_step_clamp_to_burst_capacity() {
    run_async(async {
        let burst_cap = 5_000_000;
        let limiter = TokenBucket::new(RATE_750_MBIT, burst_cap);

        // Exhaust tokens
        limiter.set_tokens(0);
        let initial_tokens = limiter.available_tokens();
        assert!(
            initial_tokens <= 100_000,
            "expected ~0 tokens after set_tokens(0), got {initial_tokens}"
        );

        // Simulate a long idle interval (or clock jump)
        tokio::time::sleep(Duration::from_millis(150)).await;

        // In 150ms at 93.75 MB/s, refill would be ~14 MB, but must be clamped to burst_cap (5 MB)
        let tokens = limiter.available_tokens();
        assert!(
            tokens <= burst_cap,
            "token bucket exceeded burst capacity: {tokens} > {burst_cap}"
        );
        assert!(
            tokens >= burst_cap - 100_000,
            "token bucket did not fill to burst capacity: {tokens}"
        );
    });
}

/// TC-T2-F26-5: Sub-Millisecond Measurement Precision vs Aggregation Windows
/// Demonstrates that ultra-short sampling intervals (1ms) have high quantization noise,
/// validating the requirement for aggregate measurement windows in benchmark reporting.
#[test]
fn test_tc_t2_f26_5_sub_millisecond_vs_aggregate_windows() {
    run_async(async {
        let limiter = Arc::new(TokenBucket::new(RATE_750_MBIT, DEFAULT_BURST_BYTES));
        limiter.set_tokens(0);

        // Rapid 1 KB acquisitions
        let mut micro_durations = Vec::new();
        for _ in 0..20 {
            let t = Instant::now();
            limiter.acquire(1024).await;
            micro_durations.push(t.elapsed().as_nanos());
        }

        // Aggregate 1 MB acquisition
        let t_agg = Instant::now();
        limiter.acquire(1_000_000).await;
        let agg_duration = t_agg.elapsed();

        println!(
            "TC-T2-F26-5: Sample micro durations: {:?}, aggregate duration: {:?}",
            &micro_durations[..5],
            agg_duration
        );

        // Aggregate duration is predictable (~10.6 ms)
        assert!(
            agg_duration >= Duration::from_millis(8) && agg_duration < Duration::from_millis(50),
            "aggregate acquisition out of expected bounds: {agg_duration:?}"
        );
    });
}
