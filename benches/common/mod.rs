// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Shared offline `std::time` statistical harness for `coree` benchmarks (`IMP-12`).

/// Summary timing metrics in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct BenchStats {
    pub mean_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub min_ms: f64,
}

/// Computes mean, p99, max, and min from millisecond samples.
#[must_use]
pub fn summarize_durations(samples_ms: &[f64]) -> BenchStats {
    if samples_ms.is_empty() {
        return BenchStats {
            mean_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
            min_ms: 0.0,
        };
    }
    let mut sorted = samples_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let sum: f64 = sorted.iter().sum();
    let mean_ms = sum / sorted.len() as f64;
    let p99_idx = ((sorted.len() - 1) as f64 * 0.99).round() as usize;
    let p99_ms = sorted[p99_idx.min(sorted.len() - 1)];
    BenchStats {
        mean_ms,
        p99_ms,
        max_ms: *sorted.last().unwrap_or(&0.0),
        min_ms: *sorted.first().unwrap_or(&0.0),
    }
}

/// Parses `--iters <N>` or `COREE_BENCH_ITERS`, ignoring Cargo's `--bench` argument.
#[must_use]
pub fn parse_bench_iters(default_iters: usize) -> usize {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--iters" {
            if let Some(v) = args.next().and_then(|s| s.parse::<usize>().ok()) {
                return v.max(1);
            }
        } else if let Some(v) = arg
            .strip_prefix("--iters=")
            .and_then(|s| s.parse::<usize>().ok())
        {
            return v.max(1);
        }
    }
    std::env::var("COREE_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map_or(default_iters, |n| n.max(1))
}
