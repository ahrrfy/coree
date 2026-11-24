// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! 100% pure-Rust, offline deterministic corpus-mutating fuzz driver (`MND-04`).
//!
//! Provides a `libfuzzer-sys`-compatible `fuzz_target!(|data: &[u8]| { ... })` macro
//! backed by a deterministic `SplitMix64` / `Xorshift64` mutator and `std::panic::catch_unwind`
//! panic detector, requiring zero network dependencies and zero `unsafe` code.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

const MAX_FUZZ_LEN: usize = 256 * 1024;
const DEFAULT_FUZZ_ITERS: usize = 250;

/// Deterministic 64-bit PRNG (SplitMix64 + Xorshift64) for reproducible mutations.
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_usize(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            0
        } else {
            (self.next_u64() as usize) % bound
        }
    }
}

/// Mutates `buf` in place using coverage-free deterministic structure-aware & bit-level operators.
pub fn mutate_corpus_entry(rng: &mut DeterministicRng, buf: &mut Vec<u8>, seeds: &[Vec<u8>]) {
    if buf.is_empty() && !seeds.is_empty() {
        let pick = rng.next_usize(seeds.len());
        *buf = seeds[pick].clone();
    }
    let ops = 1 + rng.next_usize(4);
    for _ in 0..ops {
        match rng.next_usize(7) {
            0 if !buf.is_empty() => {
                // Single bit flip
                let idx = rng.next_usize(buf.len());
                let bit = 1u8 << rng.next_usize(8);
                buf[idx] ^= bit;
            }
            1 if !buf.is_empty() => {
                // Byte overwrite with boundary values
                const INTERESTING: [u8; 8] = [0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF, b'<', b'/'];
                let idx = rng.next_usize(buf.len());
                buf[idx] = INTERESTING[rng.next_usize(INTERESTING.len())];
            }
            2 if buf.len() >= 4 => {
                // Overwrite u32 boundary integer (tests overflow/size guards)
                const BOUNDARIES: [u32; 6] =
                    [0, 1, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFE, 0xFFFF_FFFF];
                let idx = rng.next_usize(buf.len() - 3);
                let val = BOUNDARIES[rng.next_usize(BOUNDARIES.len())].to_le_bytes();
                buf[idx..idx + 4].copy_from_slice(&val);
            }
            3 if buf.len() < MAX_FUZZ_LEN => {
                // Insert random or seed byte slice
                let ins_at = rng.next_usize(buf.len() + 1);
                let byte = (rng.next_u64() & 0xFF) as u8;
                buf.insert(ins_at, byte);
            }
            4 if buf.len() > 4 => {
                // Delete slice
                let start = rng.next_usize(buf.len() - 1);
                let max_del = (buf.len() - start).clamp(1, 16);
                let count = 1 + rng.next_usize(max_del);
                buf.drain(start..start + count);
            }
            5 if buf.len() > 8 => {
                // Truncate tail (tests EOF / truncated record handling)
                let new_len = 1 + rng.next_usize(buf.len() - 1);
                buf.truncate(new_len);
            }
            _ => {
                if !seeds.is_empty() && buf.len() < MAX_FUZZ_LEN {
                    // Splice prefix/body from another seed (preserves container magic)
                    let other = &seeds[rng.next_usize(seeds.len())];
                    if !other.is_empty() {
                        let take = (1 + rng.next_usize(other.len().min(64))).min(other.len());
                        let off = rng.next_usize(other.len() - take + 1);
                        let remaining = MAX_FUZZ_LEN.saturating_sub(buf.len());
                        let actual = take.min(remaining);
                        buf.extend_from_slice(&other[off..off + actual]);
                    }
                }
            }
        }
    }
    if buf.len() > MAX_FUZZ_LEN {
        buf.truncate(MAX_FUZZ_LEN);
    }
}

/// Default multi-format adversarial seed inputs embedded offline.
#[must_use]
pub fn builtin_format_seeds() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"RIFF\x24\x00\x00\x00CDR3vrsn\x02\x00\x00\x00\x2c\x01loda\x00\x00\x00\x00".to_vec(),
        b"RIFF\x24\x00\x00\x00CDR6vrsn\x02\x00\x00\x00\x00\x04loda\x00\x00\x00\x00".to_vec(),
        b"RIFF\x24\x00\x00\x00CDR9vrsn\x02\x00\x00\x00\x84\x03txsm\x00\x00\x00\x00".to_vec(),
        b"RIFF\x10\x00\x00\x00CDREvrsn\x02\x00\x00\x00\x00\x04".to_vec(),
        b"RIFF\x1c\x00\x00\x00CDROvrsn\x10\x00\x00\x00\xff\xff\xff\xff\x02\x00\x00\x00\x60\x09\x00\x00\x00\x00\x00\x00".to_vec(),
        b"RIFF\x10\x00\x00\x00CMX1cont\x00\x00\x00\x00".to_vec(),
        b"RIFF\x10\x00\x00\x00CMX2cont\x00\x00\x00\x00".to_vec(),
        b"CPT9FILE\x1c\x00\x00\x00\x02\x00\x00\x00\x02\x00\x00\x00\x02\x00\x20\x00\x00\x00\x96\x43\x00\x00\x96\x43\x00\x00\x00\x00\x00\x00".to_vec(),
        b"PK\x03\x04\x14\x00\x00\x00\x08\x00".to_vec(),
        b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n%%EOF\n".to_vec(),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\"><rect x=\"1\" y=\"1\" width=\"10\" height=\"10\"/></svg>".to_vec(),
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n0 0 moveto 50 50 lineto stroke\n%%EOF\n".to_vec(),
        b"8BPS\x00\x01\x00\x00\x00\x00\x00\x00\x00\x03\x00\x00\x00\x04\x00\x00\x00\x04\x00\x08\x00\x03".to_vec(),
    ]
}

fn parse_iters_from_args_or_env() -> usize {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--iters" {
            if let Some(val) = args.next().and_then(|s| s.parse::<usize>().ok()) {
                return val.max(1);
            }
        } else if let Some(val) = arg
            .strip_prefix("--iters=")
            .and_then(|s| s.parse::<usize>().ok())
        {
            return val.max(1);
        }
    }
    std::env::var("COREE_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map_or(DEFAULT_FUZZ_ITERS, |n| n.max(1))
}

/// Executes deterministic fuzzing for `target_name` and aborts with exit code 1 on any panic.
pub fn run_offline_fuzz(target_name: &str, f: fn(&[u8])) {
    let iters = parse_iters_from_args_or_env();
    let mut seeds = builtin_format_seeds();

    let corpus_dir = Path::new("fuzz/corpus").join(target_name);
    if let Ok(entries) = fs::read_dir(&corpus_dir) {
        for entry in entries.flatten() {
            if let Ok(bytes) = fs::read(entry.path()) {
                if bytes.len() <= MAX_FUZZ_LEN {
                    seeds.push(bytes);
                }
            }
        }
    }

    let mut seed_hash: u64 = 0x517C_C1B7_2722_0A95;
    for b in target_name.as_bytes() {
        seed_hash = seed_hash.wrapping_mul(31).wrapping_add(u64::from(*b));
    }
    let mut rng = DeterministicRng::from_seed(seed_hash);

    // 1. Run every raw seed unmodified
    for (idx, seed) in seeds.iter().enumerate() {
        if catch_unwind(AssertUnwindSafe(|| f(seed))).is_err() {
            eprintln!(
                "FUZZ PANIC in {target_name} on seed #{idx} (len={})",
                seed.len()
            );
            std::process::exit(1);
        }
    }

    // 2. Run deterministic mutations
    let mut buf = Vec::new();
    for iter in 0..iters {
        let base = &seeds[rng.next_usize(seeds.len())];
        buf.clear();
        buf.extend_from_slice(base);
        mutate_corpus_entry(&mut rng, &mut buf, &seeds);
        if catch_unwind(AssertUnwindSafe(|| f(&buf))).is_err() {
            eprintln!(
                "FUZZ PANIC in {target_name} at iteration {iter} (len={})",
                buf.len()
            );
            std::process::exit(1);
        }
    }

    println!(
        "fuzz_{target_name}: PASS ({} seeds + {iters} mutations, 0 panics)",
        seeds.len()
    );
}

/// `libfuzzer-sys`-compatible macro generating an offline deterministic fuzz runner binary.
#[macro_export]
macro_rules! fuzz_target {
    (|$data:ident : &[u8]| $body:block) => {
        fn fuzz_entry($data: &[u8]) $body
        fn main() {
            let bin_name = std::env::args()
                .next()
                .and_then(|s| {
                    std::path::Path::new(&s)
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "target".to_owned());
            $crate::run_offline_fuzz(&bin_name, fuzz_entry);
        }
    };
}
