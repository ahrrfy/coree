// SPDX-License-Identifier: MIT OR Apache-2.0

//! Universal Cross-Platform Hardware Probe & Adaptive Resource Governor.
//!
//! Safely inspects logical CPU parallelism, physical/available RAM, container/cgroup
//! ceilings, and process RSS in 100% safe Pure-Rust (`#![forbid(unsafe_code)]`), computing
//! adaptive tile-cache, GPU-texture, worker-thread, Undo-history, and LOD budgets so
//! `coree` runs fluidly on any machine — from 4 GiB dual-core laptops to 64+ GiB workstations.

use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// Byte footprint of one `256 × 256` RGBA8 world tile (`256 KiB`).
pub const WORLD_TILE_BYTES: u64 = 256 * 256 * 4;

/// User-selectable or automatic hardware resource governance policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ResourceGovernorMode {
    /// Automatically adapts all memory, GPU-tile, thread, and LOD budgets to the host hardware
    /// and live OS memory pressure.
    #[default]
    Auto,
    /// Conservative low-memory profile for 4–8 GiB laptops or heavy multitasking.
    EcoLowMemory,
    /// Balanced studio profile reserving ample headroom for other desktop apps.
    Balanced,
    /// Workstation profile maximizing multi-page tile retention, GPU tile pool, and worker threads.
    MaxPerformance,
}

impl ResourceGovernorMode {
    /// All selectable modes in UI display order.
    pub const ALL: [Self; 4] = [
        Self::Auto,
        Self::EcoLowMemory,
        Self::Balanced,
        Self::MaxPerformance,
    ];
}

/// Snapshot of host CPU and memory resources detected at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostResourceProfile {
    /// Logical CPU threads available to this process (respects OS affinity / cgroup quotas).
    pub logical_cpus: usize,
    /// Total physical (or cgroup-capped) RAM in bytes.
    pub total_ram_bytes: u64,
    /// Currently available RAM in bytes before swapping/OOM pressure.
    pub available_ram_bytes: u64,
    /// Current resident set size (RSS) of this `coree` process in bytes, if readable.
    pub process_rss_bytes: Option<u64>,
    /// Active cgroup / environment memory cap in bytes, if any.
    pub cgroup_limit_bytes: Option<u64>,
    /// `true` when exact OS memory counters were read; `false` when using safe heuristic sizing.
    pub exact_memory_telemetry: bool,
}

impl Default for HostResourceProfile {
    fn default() -> Self {
        Self::probe()
    }
}

impl HostResourceProfile {
    /// Probes the current machine's CPU threads, RAM, cgroup limits, and process RSS safely.
    #[must_use]
    pub fn probe() -> Self {
        let logical_cpus = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4)
            .clamp(1, 256);

        let env_cap = std::env::var("COREE_MEMORY_LIMIT_MB")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&mb| mb >= 256)
            .map(|mb| mb.saturating_mul(MIB));

        let (mut total_ram, mut avail_ram, rss, cgroup_cap, exact) = probe_platform_memory(logical_cpus);

        let effective_cap = match (env_cap, cgroup_cap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        if let Some(cap) = effective_cap {
            if cap < total_ram {
                total_ram = cap;
            }
            let used_est = rss.unwrap_or(0);
            let cap_headroom = cap.saturating_sub(used_est);
            avail_ram = avail_ram.min(cap_headroom);
        }

        total_ram = total_ram.max(512 * MIB);
        avail_ram = avail_ram.min(total_ram).max(128 * MIB);

        Self {
            logical_cpus,
            total_ram_bytes: total_ram,
            available_ram_bytes: avail_ram,
            process_rss_bytes: rss,
            cgroup_limit_bytes: effective_cap,
            exact_memory_telemetry: exact,
        }
    }

    /// Refreshes live memory telemetry (`available_ram_bytes` and `process_rss_bytes`).
    pub fn refresh_live(&mut self) {
        *self = Self::probe();
    }

    /// Total RAM formatted in GiB (e.g. `45.1`).
    #[must_use]
    pub fn total_ram_gib(&self) -> f64 {
        self.total_ram_bytes as f64 / GIB as f64
    }

    /// Available RAM formatted in GiB.
    #[must_use]
    pub fn available_ram_gib(&self) -> f64 {
        self.available_ram_bytes as f64 / GIB as f64
    }

    /// Process RSS formatted in MiB, if available.
    #[must_use]
    pub fn process_rss_mib(&self) -> Option<f64> {
        self.process_rss_bytes.map(|b| b as f64 / MIB as f64)
    }

    /// Returns `true` when system available RAM is critically tight (`< 768 MiB` or `< 10%` of total).
    #[must_use]
    pub fn is_memory_pressured(&self) -> bool {
        self.available_ram_bytes < 768 * MIB
            || self.available_ram_bytes.saturating_mul(10) < self.total_ram_bytes
    }
}

/// Concrete runtime budgets derived from [`HostResourceProfile`] and [`ResourceGovernorMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceBudget {
    /// Background `JobPool` worker threads (leaves UI/compositor headroom on multi-core CPUs).
    pub worker_threads: usize,
    /// Maximum `256 × 256` RGBA world tiles retained in the active document's LRU cache
    /// across zoom levels and pages.
    pub max_cached_world_tiles: usize,
    /// Maximum `256 × 256` GPU tile textures uploaded simultaneously per active document tab.
    pub max_gpu_tile_textures: usize,
    /// Maximum cached world tiles retained on inactive background document tabs.
    pub inactive_tab_tile_cap: usize,
    /// Recommended `CommandHistory` Undo depth for newly opened documents.
    pub undo_history_limit: usize,
    /// Maximum Bézier anchor nodes painted per frame in `F10` Shape/Node tool before
    /// screen-space cell decimation activates.
    pub node_overlay_budget: usize,
    /// Maximum path nodes evaluated synchronously on the UI thread for status-bar metrics;
    /// heavier selections/pages compute asynchronously in the background.
    pub live_metrics_sync_node_limit: usize,
    /// File byte size above which interactive GUI Open/Import runs asynchronously on `JobPool`.
    pub async_file_threshold_bytes: u64,
    /// Whether live OS memory pressure trimming is currently active.
    pub under_memory_pressure: bool,
}

impl ResourceBudget {
    /// Computes safe, hardware-proportional runtime budgets for `profile` and `mode`.
    #[must_use]
    pub fn compute(profile: &HostResourceProfile, mode: ResourceGovernorMode) -> Self {
        let pressured = profile.is_memory_pressured();
        let cpus = profile.logical_cpus.max(1);

        let worker_threads = match mode {
            ResourceGovernorMode::EcoLowMemory => (cpus / 2).clamp(1, 4),
            ResourceGovernorMode::Balanced => cpus.saturating_sub(2).clamp(1, 12),
            ResourceGovernorMode::Auto => {
                if cpus <= 2 {
                    1
                } else {
                    cpus.saturating_sub(1).min(16)
                }
            }
            ResourceGovernorMode::MaxPerformance => cpus.saturating_sub(1).clamp(1, 32),
        };

        // Target tile budget as a safe fraction of available & total RAM so we never crowd the OS:
        // - Eco: ~4% of available RAM (64 MiB .. 128 MiB)
        // - Balanced: ~8% of available RAM (96 MiB .. 384 MiB)
        // - Auto: ~10% of available RAM (96 MiB .. 768 MiB -> up to 3,072 tiles for instant multi-page switching!)
        // - MaxPerformance: ~15% of available RAM (128 MiB .. 1024 MiB -> up to 4,096 tiles)
        let (ram_div, min_tiles, max_tiles) = if pressured {
            (25u64, 192usize, 384usize)
        } else {
            match mode {
                ResourceGovernorMode::EcoLowMemory => (24, 256, 512),
                ResourceGovernorMode::Balanced => (12, 384, 1536),
                ResourceGovernorMode::Auto => (10, 384, 3072),
                ResourceGovernorMode::MaxPerformance => (7, 512, 4096),
            }
        };

        let budget_bytes = (profile.available_ram_bytes / ram_div)
            .min(profile.total_ram_bytes / 8);
        let raw_tiles = (budget_bytes / WORLD_TILE_BYTES) as usize;
        let max_cached_world_tiles = raw_tiles.clamp(min_tiles, max_tiles);

        let max_gpu_tile_textures = if pressured {
            192
        } else {
            match mode {
                ResourceGovernorMode::EcoLowMemory => (max_cached_world_tiles / 2).clamp(192, 384),
                ResourceGovernorMode::Balanced => (max_cached_world_tiles / 2).clamp(256, 768),
                ResourceGovernorMode::Auto => (max_cached_world_tiles / 2).clamp(320, 1280),
                ResourceGovernorMode::MaxPerformance => {
                    (max_cached_world_tiles * 2 / 3).clamp(384, 1792)
                }
            }
        };

        let inactive_tab_tile_cap = if pressured {
            32
        } else {
            (max_cached_world_tiles / 8).clamp(64, 256)
        };

        let total_gib = profile.total_ram_bytes / GIB;
        let undo_history_limit = if pressured || mode == ResourceGovernorMode::EcoLowMemory {
            50
        } else if total_gib >= 32 {
            match mode {
                ResourceGovernorMode::MaxPerformance => 300,
                _ => 200,
            }
        } else if total_gib >= 12 {
            150
        } else if total_gib >= 6 {
            100
        } else {
            64
        };

        let node_overlay_budget = match mode {
            ResourceGovernorMode::EcoLowMemory => 1_800,
            ResourceGovernorMode::Balanced => 3_500,
            ResourceGovernorMode::Auto => {
                if cpus >= 8 {
                    5_000
                } else {
                    3_000
                }
            }
            ResourceGovernorMode::MaxPerformance => 8_000,
        };

        let live_metrics_sync_node_limit = match mode {
            ResourceGovernorMode::EcoLowMemory => 1_000,
            ResourceGovernorMode::Balanced => 2_000,
            ResourceGovernorMode::Auto => {
                if cpus >= 8 {
                    3_000
                } else {
                    1_500
                }
            }
            ResourceGovernorMode::MaxPerformance => 4_500,
        };

        let async_file_threshold_bytes = match mode {
            ResourceGovernorMode::EcoLowMemory => 256 * 1024,
            ResourceGovernorMode::Balanced | ResourceGovernorMode::Auto => 384 * 1024,
            ResourceGovernorMode::MaxPerformance => 768 * 1024,
        };

        Self {
            worker_threads,
            max_cached_world_tiles,
            max_gpu_tile_textures,
            inactive_tab_tile_cap,
            undo_history_limit,
            node_overlay_budget,
            live_metrics_sync_node_limit,
            async_file_threshold_bytes,
            under_memory_pressure: pressured,
        }
    }

    /// Maximum RAM budget in MiB allocated for cached `256 × 256` world tiles on the active tab.
    #[must_use]
    pub fn max_tile_cache_mib(&self) -> f64 {
        (self.max_cached_world_tiles as u64 * WORLD_TILE_BYTES) as f64 / MIB as f64
    }
}

#[cfg(target_os = "linux")]
fn probe_platform_memory(_logical_cpus: usize) -> (u64, u64, Option<u64>, Option<u64>, bool) {
    let mut mem_total_kb: Option<u64> = None;
    let mut mem_avail_kb: Option<u64> = None;
    let mut mem_free_kb: u64 = 0;
    let mut buffers_kb: u64 = 0;
    let mut cached_kb: u64 = 0;

    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            let mut parts = line.split_whitespace();
            let Some(key) = parts.next() else { continue };
            let Some(val_str) = parts.next() else { continue };
            let Ok(val) = val_str.parse::<u64>() else {
                continue;
            };
            match key {
                "MemTotal:" => mem_total_kb = Some(val),
                "MemAvailable:" => mem_avail_kb = Some(val),
                "MemFree:" => mem_free_kb = val,
                "Buffers:" => buffers_kb = val,
                "Cached:" => cached_kb = val,
                _ => {}
            }
        }
    }

    let rss_bytes = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages.saturating_mul(4096));

    // Respect cgroup v2 (`memory.max`) and cgroup v1 (`memory.limit_in_bytes`) container caps
    let cgroup_v2_max = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&v| v > 0 && v < (1u64 << 60));
    let cgroup_v1_max = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&v| v > 0 && v < (1u64 << 60));
    let cgroup_cap = match (cgroup_v2_max, cgroup_v1_max) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    if let Some(total_kb) = mem_total_kb {
        let total = total_kb.saturating_mul(1024);
        let avail_kb =
            mem_avail_kb.unwrap_or_else(|| mem_free_kb.saturating_add(buffers_kb).saturating_add(cached_kb));
        let mut avail = avail_kb.saturating_mul(1024);
        // Also refine cgroup available memory when `memory.current` is exposed
        if let Some(cap) = cgroup_cap {
            if let Ok(cur_str) = std::fs::read_to_string("/sys/fs/cgroup/memory.current") {
                if let Ok(cur) = cur_str.trim().parse::<u64>() {
                    avail = avail.min(cap.saturating_sub(cur));
                }
            }
        }
        (total, avail, rss_bytes, cgroup_cap, true)
    } else {
        heuristic_memory_from_cpus(_logical_cpus, rss_bytes)
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_platform_memory(logical_cpus: usize) -> (u64, u64, Option<u64>, Option<u64>, bool) {
    heuristic_memory_from_cpus(logical_cpus, None)
}

fn heuristic_memory_from_cpus(
    logical_cpus: usize,
    rss_bytes: Option<u64>,
) -> (u64, u64, Option<u64>, Option<u64>, bool) {
    let total_gib: u64 = match logical_cpus {
        0..=2 => 4,
        3..=4 => 8,
        5..=8 => 12,
        9..=16 => 16,
        _ => 32,
    };
    let total = total_gib * GIB;
    let avail = (total * 65) / 100;
    (total, avail, rss_bytes, None, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_resource_probe_and_adaptive_budgets_scale_cleanly_across_hardware_tiers() {
        let live = HostResourceProfile::probe();
        assert!(live.logical_cpus >= 1);
        assert!(live.total_ram_bytes >= 512 * MIB);
        assert!(live.available_ram_bytes <= live.total_ram_bytes);

        // Low-end 4 GiB dual-core laptop
        let laptop_4g = HostResourceProfile {
            logical_cpus: 2,
            total_ram_bytes: 4 * GIB,
            available_ram_bytes: 1800 * MIB,
            process_rss_bytes: Some(120 * MIB),
            cgroup_limit_bytes: None,
            exact_memory_telemetry: true,
        };
        let b_low = ResourceBudget::compute(&laptop_4g, ResourceGovernorMode::Auto);
        assert_eq!(b_low.worker_threads, 1);
        assert!(b_low.max_cached_world_tiles <= 720);
        assert_eq!(b_low.undo_history_limit, 64);

        // High-end 45 GiB 12-thread workstation (e.g. Ryzen 5 5600X + 45 GiB RAM)
        let workstation_45g = HostResourceProfile {
            logical_cpus: 12,
            total_ram_bytes: 45 * GIB,
            available_ram_bytes: 38 * GIB,
            process_rss_bytes: Some(250 * MIB),
            cgroup_limit_bytes: None,
            exact_memory_telemetry: true,
        };
        let b_high = ResourceBudget::compute(&workstation_45g, ResourceGovernorMode::Auto);
        assert_eq!(b_high.worker_threads, 11);
        assert_eq!(b_high.max_cached_world_tiles, 3072);
        assert!(b_high.max_gpu_tile_textures >= 1024);
        assert_eq!(b_high.undo_history_limit, 200);

        // Severe OS memory pressure triggers emergency self-throttling on any machine
        let pressured = HostResourceProfile {
            logical_cpus: 12,
            total_ram_bytes: 16 * GIB,
            available_ram_bytes: 512 * MIB,
            process_rss_bytes: Some(900 * MIB),
            cgroup_limit_bytes: None,
            exact_memory_telemetry: true,
        };
        assert!(pressured.is_memory_pressured());
        let b_pressured = ResourceBudget::compute(&pressured, ResourceGovernorMode::MaxPerformance);
        assert!(b_pressured.under_memory_pressure);
        assert!(b_pressured.max_cached_world_tiles <= 384);
        assert_eq!(b_pressured.inactive_tab_tile_cap, 32);
    }
}
