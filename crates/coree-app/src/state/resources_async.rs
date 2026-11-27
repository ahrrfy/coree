// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime Hardware Resource Governor, Live Memory-Pressure Self-Trimming,
//! and Non-Blocking Async File Open / Import / Vector Trace / Metrics Pipeline.

use super::*;
use crate::io_bridge::OpenedDocument;
use coree_core::{Perspective3x3, ResourceBudget, ResourceGovernorMode};

/// Completed payload produced by an interactive non-blocking studio background operation.
pub(crate) enum AsyncUiOutcome {
    OpenDocument {
        path: PathBuf,
        opened: Box<OpenedDocument>,
        file_size: u64,
    },
    ImportDocument {
        tab_id: Uuid,
        path: PathBuf,
        opened: Box<OpenedDocument>,
    },
    VectorTrace {
        tab_id: Uuid,
        target_id: ObjectId,
        layer_id: Option<LayerId>,
        traced: Vec<(BezierPath, Color)>,
        offset_x_mm: f64,
        offset_y_mm: f64,
        placed_xf: Affine2D,
        persp: Option<Perspective3x3>,
        is_centerline: bool,
        keep_original: bool,
    },
}

/// Active non-blocking UI operation tracked on [`StudioApp`].
pub struct ActiveAsyncUiOp {
    pub(crate) handle: JobHandle<AsyncUiOutcome>,
}

fn is_heavy_container_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "pdf" | "ai" | "cdr" | "cdt" | "psd" | "psb" | "tiff" | "tif" | "eps" | "ps"
            )
        })
}

/// Estimates path/text node complexity across `selection` (or the active page when empty)
/// in bounded `O(objects)` time without flattening Bézier curves.
#[must_use]
pub(crate) fn estimate_tab_metrics_nodes(tab: &DocumentTab) -> usize {
    fn obj_nodes(doc: &Document, oid: ObjectId, depth: usize) -> usize {
        if depth > 8 {
            return 0;
        }
        let Some(obj) = doc.object(oid) else {
            return 0;
        };
        if !obj.visible {
            return 0;
        }
        let base = match &obj.payload {
            NodePayload::Path(p) => p.subpaths.iter().map(|s| s.nodes.len()).sum(),
            NodePayload::BSpline(bs) => bs.control_points.len() * 3,
            NodePayload::Shape(_) => 16,
            NodePayload::ArtisticMedia { spine, .. } => {
                spine.subpaths.iter().map(|s| s.nodes.len() * 4).sum()
            }
            NodePayload::ArtisticText(t) => t.plain_text().len() * 12,
            NodePayload::ParagraphText(p) => p.plain_text().len() * 12,
            NodePayload::TextOnPath(top) => top.text.plain_text().len() * 12,
            NodePayload::Group(children) => children
                .iter()
                .map(|&c| obj_nodes(doc, c, depth + 1))
                .sum(),
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                let cn: usize = container.subpaths.iter().map(|s| s.nodes.len()).sum();
                cn + contents
                    .iter()
                    .map(|&c| obj_nodes(doc, c, depth + 1))
                    .sum::<usize>()
            }
            _ => 4,
        };
        let eff_mul = if obj.effects.is_empty() { 1 } else { 3 };
        base.saturating_mul(eff_mul)
    }

    if !tab.selection.is_empty() {
        tab.selection
            .objects
            .iter()
            .map(|&oid| obj_nodes(&tab.document, oid, 0))
            .sum()
    } else if let Some(page) = tab.document.active_page() {
        let mut total = 0usize;
        for layer in &page.layers {
            if !layer.visible {
                continue;
            }
            for &oid in &layer.objects {
                total = total.saturating_add(obj_nodes(&tab.document, oid, 0));
                if total > 50_000 {
                    return total;
                }
            }
        }
        total
    } else {
        0
    }
}

impl StudioApp {
    /// Updates the studio's [`ResourceGovernorMode`], recomputes hardware-proportional budgets,
    /// resizes the background worker pool, applies cache/history limits across all open tabs,
    /// and persists the setting in SQLite preferences.
    pub fn set_resource_governor_mode(&mut self, mode: ResourceGovernorMode) {
        self.resource_mode = mode;
        self.host_resources.refresh_live();
        self.resource_budget = ResourceBudget::compute(&self.host_resources, self.resource_mode);
        if self
            .job_pool
            .as_ref()
            .is_none_or(|p| p.worker_count() != self.resource_budget.worker_threads)
            && self.active_jobs.is_empty()
            && self.async_ui_ops.is_empty()
        {
            if let Ok(pool) = JobPool::new(self.resource_budget.worker_threads) {
                self.job_pool = Some(pool);
            }
        }
        self.apply_resource_budget_to_tabs();
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("perf.governor_mode", "perf", &self.resource_mode);
        }
    }

    /// Applies the current [`ResourceBudget`] to all open document tabs (sizing active-tab
    /// tile & GPU pools and trimming background tabs to conserve RAM/VRAM).
    pub fn apply_resource_budget_to_tabs(&mut self) {
        let budget = self.resource_budget;
        let active_idx = self.active_tab;
        for (idx, tab) in self.tabs.iter_mut().enumerate() {
            tab.history.set_max_history(budget.undo_history_limit);
            if idx == active_idx {
                tab.tile_cache
                    .set_max_cached_tiles(budget.max_cached_world_tiles);
                tab.trim_gpu_tile_textures(budget.max_gpu_tile_textures);
            } else {
                tab.tile_cache
                    .trim_inactive_tiles(budget.inactive_tab_tile_cap);
                let inactive_gpu_cap = if budget.under_memory_pressure {
                    0
                } else {
                    budget.inactive_tab_tile_cap.min(128)
                };
                tab.trim_gpu_tile_textures(inactive_gpu_cap);
            }
        }
    }

    /// Periodically refreshes live host RAM / RSS telemetry (every `2.5s`) and self-trims
    /// background caches automatically if the OS enters memory pressure.
    pub fn tick_resource_governor(&mut self) {
        if self.last_resource_probe_at.elapsed() < Duration::from_millis(2500) {
            return;
        }
        self.last_resource_probe_at = Instant::now();
        self.host_resources.refresh_live();
        let updated = ResourceBudget::compute(&self.host_resources, self.resource_mode);
        let changed = updated != self.resource_budget;
        self.resource_budget = updated;
        if changed || updated.under_memory_pressure {
            self.apply_resource_budget_to_tabs();
        }
    }

    /// Immediately purges all off-screen CPU world tiles and unused GPU tile textures across
    /// every open tab, returning `(freed_cpu_tiles, freed_gpu_textures)`.
    pub fn purge_unused_caches(&mut self) -> (usize, usize) {
        let mut freed_cpu = 0usize;
        let mut freed_gpu = 0usize;
        let active_idx = self.active_tab;
        for (idx, tab) in self.tabs.iter_mut().enumerate() {
            let before_cpu = tab.tile_cache.cached_world_tile_count();
            let before_gpu = tab.gpu_tile_textures.len();
            let keep = if idx == active_idx {
                tab.tile_cache.tile_count().max(16)
            } else {
                16
            };
            tab.tile_cache.trim_inactive_tiles(keep);
            if idx == active_idx {
                let active_keys: HashSet<coree_render::WorldTileKey> = tab
                    .tile_cache
                    .active_world_tile_keys()
                    .iter()
                    .copied()
                    .collect();
                tab.gpu_tile_textures
                    .retain(|k, _| active_keys.contains(k));
            } else {
                tab.gpu_tile_textures.clear();
                tab.last_rendered_texture = None;
            }
            freed_cpu += before_cpu.saturating_sub(tab.tile_cache.cached_world_tile_count());
            freed_gpu += before_gpu.saturating_sub(tab.gpu_tile_textures.len());
        }
        self.host_resources.refresh_live();
        self.resource_budget = ResourceBudget::compute(&self.host_resources, self.resource_mode);
        (freed_cpu, freed_gpu)
    }

    /// Refreshes status-bar path length & area metrics without stalling the 60fps UI thread:
    /// small/medium selections compute inline (`< 0.4 ms`), while heavy `3,000+` node scenes
    /// compute asynchronously on [`JobPool`].
    pub fn poll_active_tab_metrics_nonblocking(&mut self) {
        let sync_limit = self.resource_budget.live_metrics_sync_node_limit;
        let pool_ref = self.job_pool.as_ref();
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        if let Some((gen_done, ref handle)) = tab.metrics_job {
            if let Some(outcome) = handle.try_recv() {
                if let JobOutcome::Completed(metrics) = outcome {
                    tab.last_metrics = metrics;
                    if gen_done == tab.metrics_generation {
                        tab.metrics_dirty = false;
                    }
                }
                tab.metrics_job = None;
            }
        }

        if !tab.metrics_dirty {
            return;
        }

        let est_nodes = estimate_tab_metrics_nodes(tab);
        if est_nodes <= sync_limit || !self.async_interactive_io || pool_ref.is_none() {
            tab.ensure_metrics_fresh();
            return;
        }

        // Coalesce: if a background metrics job is already running, wait for it to finish
        // before launching the next revision so rapid edits never queue duplicate clones.
        if tab.metrics_job.is_some() {
            return;
        }

        if let Some(pool) = pool_ref {
            let doc_clone = tab.document.clone();
            let sel_clone: Option<Vec<ObjectId>> =
                (!tab.selection.is_empty()).then(|| tab.selection.objects.clone());
            let target_gen = tab.metrics_generation;
            let handle = pool.submit("Metrics", move |ctx| {
                if ctx.is_cancelled() {
                    return Err("cancelled".to_owned());
                }
                Ok(compute_production_metrics(
                    &doc_clone,
                    sel_clone.as_deref(),
                ))
            });
            tab.metrics_job = Some((target_gen, handle));
        }
    }

    /// Interactive GUI file-open entrypoint: opens lightweight files immediately and offloads
    /// large or multi-page files (`.pdf`, `.cdr`, `.psd`, `.tiff`, etc.) onto [`JobPool`]
    /// with live progress so the desktop window never freezes.
    pub fn open_file_path_interactive(&mut self, path: &Path) {
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let threshold = if is_heavy_container_ext(path) {
            (self.resource_budget.async_file_threshold_bytes / 2).max(160 * 1024)
        } else {
            self.resource_budget.async_file_threshold_bytes
        };

        if !self.async_interactive_io || file_size < threshold || self.job_pool.is_none() {
            if let Err(err) = self.open_file_path(path) {
                self.push_toast(err, true);
            }
            return;
        }

        let Some(ref pool) = self.job_pool else {
            return;
        };
        let path_owned = path.to_path_buf();
        let file_label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("document")
            .to_owned();
        let job_name = format!("Open '{file_label}'");
        let handle = pool.submit(job_name, move |ctx| {
            ctx.set_progress(15);
            if ctx.is_cancelled() {
                return Err("cancelled".to_owned());
            }
            let opened = open_any_document(&path_owned)?;
            if ctx.is_cancelled() {
                return Err("cancelled".to_owned());
            }
            ctx.set_progress(95);
            Ok(AsyncUiOutcome::OpenDocument {
                path: path_owned,
                opened: Box::new(opened),
                file_size,
            })
        });
        self.async_ui_ops.push(ActiveAsyncUiOp { handle });
        self.show_welcome = false;
        self.status_message = format!("Opening '{file_label}'...");
    }

    /// Interactive GUI file-import entrypoint with automatic background parsing for large files.
    pub fn import_file_path_interactive(&mut self, path: &Path) {
        let Some(tab_id) = self.active_tab().map(|t| t.id) else {
            self.push_toast("no active document", true);
            return;
        };
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let threshold = if is_heavy_container_ext(path) {
            (self.resource_budget.async_file_threshold_bytes / 2).max(160 * 1024)
        } else {
            self.resource_budget.async_file_threshold_bytes
        };

        if !self.async_interactive_io || file_size < threshold || self.job_pool.is_none() {
            if let Err(err) = self.import_file_path(path) {
                self.push_toast(err, true);
            }
            return;
        }

        let Some(ref pool) = self.job_pool else {
            return;
        };
        let path_owned = path.to_path_buf();
        let file_label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("asset")
            .to_owned();
        let handle = pool.submit(format!("Import '{file_label}'"), move |ctx| {
            ctx.set_progress(20);
            if ctx.is_cancelled() {
                return Err("cancelled".to_owned());
            }
            let opened = open_any_document(&path_owned)?;
            ctx.set_progress(95);
            Ok(AsyncUiOutcome::ImportDocument {
                tab_id,
                path: path_owned,
                opened: Box::new(opened),
            })
        });
        self.async_ui_ops.push(ActiveAsyncUiOp { handle });
        self.status_message = format!("Importing '{file_label}'...");
    }

    /// Interactive GUI Vector Trace entrypoint: runs small bitmaps inline and offloads
    /// high-resolution bitmaps (`>= 360,000` px) onto [`JobPool`] so the UI stays responsive.
    pub fn trace_selected_raster_interactive(&mut self) {
        if !self.async_interactive_io || self.job_pool.is_none() {
            if let Err(err) = self.trace_selected_raster() {
                self.push_toast(err, true);
            }
            return;
        }

        let Some(tab) = self.tabs.get(self.active_tab) else {
            self.push_toast("no active document", true);
            return;
        };
        let Some(resolved) = Self::find_trace_target_in_tab(tab) else {
            self.push_toast("select a raster bitmap object to run Vector Trace", true);
            return;
        };
        let px_area = u64::from(resolved.asset.width).saturating_mul(u64::from(resolved.asset.height));
        if px_area < 360_000 {
            if let Err(err) = self.trace_selected_raster() {
                self.push_toast(err, true);
            }
            return;
        }

        let layer_id = match self.active_creation_layer_or_err() {
            Ok(lid) => lid,
            Err(e) => {
                self.push_toast(e, true);
                return;
            }
        };
        let tab_id = tab.id;
        let cfg = self.trace_state.to_config();
        let is_centerline = matches!(cfg.mode, TraceMode::CenterlineTrace);
        let place_side_by_side = self.trace_state.place_side_by_side;
        let keep_original = self.trace_state.keep_original;

        let Some(ref pool) = self.job_pool else {
            return;
        };
        let handle = pool.submit("Vector Trace", move |ctx| {
            ctx.set_progress(15);
            if ctx.is_cancelled() {
                return Err("cancelled".to_owned());
            }
            let target_id = resolved.root_id;
            let filters = resolved.raster.active_filters();
            let crop_rect = resolved.effective_crop;
            let w_mm = resolved.raster.width_mm;
            let h_mm = resolved.raster.height_mm;
            let xf = resolved.world_xf;
            let persp = resolved.persp;
            if !w_mm.is_finite() || w_mm <= 0.0 || !h_mm.is_finite() || h_mm <= 0.0 {
                return Err("raster has invalid dimensions".to_owned());
            }
            let filtered_asset = if filters.is_empty() {
                resolved.asset
            } else {
                coree_render::apply_raster_filters(&resolved.asset, &filters)
            };
            ctx.set_progress(35);
            if ctx.is_cancelled() {
                return Err("cancelled".to_owned());
            }
            let (trace_asset, offset_x_mm, offset_y_mm, trace_w_mm, trace_h_mm) =
                super::production::crop_filtered_trace_asset(filtered_asset, crop_rect, w_mm, h_mm)?;
            ctx.set_progress(50);
            let traced = trace_raster_to_mm(&trace_asset, trace_w_mm, trace_h_mm, &cfg);
            if ctx.is_cancelled() {
                return Err("cancelled".to_owned());
            }
            if traced.is_empty() {
                return Err("trace produced 0 contours (adjust detail or colors)".to_owned());
            }
            let placed_xf = super::production::compute_trace_placed_transform(
                xf,
                persp,
                offset_x_mm,
                offset_y_mm,
                trace_w_mm,
                trace_h_mm,
                place_side_by_side,
            );
            ctx.set_progress(95);
            Ok(AsyncUiOutcome::VectorTrace {
                tab_id,
                target_id,
                layer_id,
                traced,
                offset_x_mm,
                offset_y_mm,
                placed_xf,
                persp,
                is_centerline,
                keep_original,
            })
        });
        self.async_ui_ops.push(ActiveAsyncUiOp { handle });
        self.status_message = "Running Vector Trace in background...".to_owned();
    }

    /// Returns `(primary_label, average_progress_pct, total_running_count)` when any background
    /// export, open, import, or trace task is active.
    #[must_use]
    pub fn active_background_progress_summary(&self) -> Option<(String, u8, usize)> {
        let total = self.active_jobs.len() + self.async_ui_ops.len();
        if total == 0 {
            return None;
        }
        let mut sum_pct = 0u32;
        let mut primary = String::new();
        for op in &self.async_ui_ops {
            if primary.is_empty() {
                primary = op.handle.name().to_owned();
            }
            sum_pct += u32::from(op.handle.progress().max(5));
        }
        for job in &self.active_jobs {
            if primary.is_empty() {
                primary = job.handle.name().to_owned();
            }
            sum_pct += u32::from(job.handle.progress().max(5));
        }
        let avg = (sum_pct / (total as u32)).min(100) as u8;
        Some((primary, avg, total))
    }

    /// Requests cooperative cancellation of all running background export/open/import/trace jobs.
    pub fn cancel_all_background_jobs(&mut self) {
        for op in &self.async_ui_ops {
            op.handle.cancel();
        }
        for job in &self.active_jobs {
            job.handle.cancel();
        }
    }

    pub(crate) fn poll_async_ui_ops(&mut self) {
        let mut completed = Vec::new();
        let mut messages = Vec::new();
        self.async_ui_ops.retain(|op| match op.handle.try_recv() {
            Some(JobOutcome::Completed(payload)) => {
                completed.push(payload);
                false
            }
            Some(JobOutcome::Cancelled) => {
                messages.push((format!("Cancelled {}", op.handle.name()), false));
                false
            }
            Some(JobOutcome::Failed(err)) => {
                messages.push((format!("{} failed: {err}", op.handle.name()), true));
                false
            }
            None => true,
        });

        for (msg, is_err) in messages {
            self.push_toast(msg, is_err);
        }

        for item in completed {
            match item {
                AsyncUiOutcome::OpenDocument {
                    path,
                    opened,
                    file_size,
                } => {
                    self.finish_opened_document(&path, *opened, file_size);
                }
                AsyncUiOutcome::ImportDocument {
                    tab_id,
                    path,
                    opened,
                } => {
                    if let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) {
                        self.active_tab = idx;
                        if let Err(err) = self.finish_imported_document(&path, *opened) {
                            self.push_toast(err, true);
                        }
                    }
                }
                AsyncUiOutcome::VectorTrace {
                    tab_id,
                    target_id,
                    layer_id,
                    traced,
                    offset_x_mm,
                    offset_y_mm,
                    placed_xf,
                    persp,
                    is_centerline,
                    keep_original,
                } => {
                    if let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) {
                        self.active_tab = idx;
                        if let Err(err) = self.commit_traced_contours(
                            target_id,
                            layer_id,
                            traced,
                            offset_x_mm,
                            offset_y_mm,
                            placed_xf,
                            persp,
                            is_centerline,
                            keep_original,
                        ) {
                            self.push_toast(err, true);
                        }
                    }
                }
            }
        }
    }
}
