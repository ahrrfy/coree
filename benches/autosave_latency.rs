// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Offline quantitative benchmark (`IMP-12` + `IMP-07`):
//! Measures UI-thread enqueue latency for async differential autosave on a 10,000-object document
//! (target `<= 2.0 ms`).

use coree_bench_common::{parse_bench_iters, summarize_durations};
use coree_core::{
    apply_command, BezierPath, Color, CommandOp, Document, Fill, NodePayload, ObjectId, Rect2D,
    SceneObject,
};
use coree_storage::{DifferentialSnapshotPatch, StorageDb};
use std::time::{Duration, Instant};

fn main() {
    let iters = parse_bench_iters(30);
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench_autosave.db");
    let db = StorageDb::open(&db_path).expect("open WAL db");

    let mut doc = Document::new_a4("10k Autosave Bench");
    let doc_id = doc.id().to_string();
    let layer_id = doc.page(0).unwrap().layers[0].id;
    let mut ids = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        let x = (i % 100) as f64 * 2.0;
        let y = (i / 100) as f64 * 2.0;
        let oid = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("Item{i}"),
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(x, y, 1.5, 1.5))),
                )
                .with_fill(Fill::Solid(Color::CYAN)),
                Some(layer_id),
            )
            .unwrap();
        ids.push(oid);
    }

    let baseline = db
        .save_snapshot(&doc_id, doc.title(), None, &doc)
        .expect("baseline snapshot");
    let mut prev_snap_id = baseline.id;
    let mut enqueue_samples_ms = Vec::with_capacity(iters);

    for step in 0..iters {
        let idx = (step * 5) % 9995;
        let touched = &ids[idx..idx + 5];
        let color = if step % 2 == 0 {
            Color::MAGENTA
        } else {
            Color::YELLOW
        };
        let mut upserted_objects = Vec::with_capacity(5);
        for &oid in touched {
            apply_command(
                &mut doc,
                &CommandOp::SetFill {
                    ids: vec![oid],
                    fill: Fill::Solid(color.clone()),
                },
            )
            .unwrap();
            upserted_objects.push((oid, doc.object(oid).unwrap().clone()));
        }

        let patch = DifferentialSnapshotPatch {
            doc_id: doc_id.clone(),
            session_id: baseline.session_id.clone(),
            title: doc.title().to_owned(),
            source_path: None,
            base_snapshot_id: prev_snap_id.clone(),
            last_command_seq: 0,
            upserted_objects,
            deleted_object_ids: Vec::new(),
            upserted_assets: Vec::new(),
            deleted_asset_ids: Vec::new(),
            full_assets: None,
            pages: None,
            active_page_index: None,
            next_id: Some(doc.next_id()),
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
            palettes: None,
            document_palette: None,
            guides: None,
            grid: None,
            perspective_grid: None,
            color_styles: None,
            object_styles: None,
            symbols: None,
            metadata: None,
        };

        let t0 = Instant::now();
        let delta_id = db
            .enqueue_differential_autosave(patch)
            .expect("enqueue differential autosave");
        enqueue_samples_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        prev_snap_id = delta_id;
    }

    db.flush_autosave(Duration::from_secs(10))
        .expect("flush autosave worker");
    let stats = summarize_durations(&enqueue_samples_ms);
    println!(
        "autosave_ui_enqueue: mean = {:.3} ms (target <= 2.0 ms)",
        stats.mean_ms
    );
    println!("autosave_ui_p99: {:.3} ms", stats.p99_ms);
    assert!(
        stats.mean_ms <= 2.0 && stats.p99_ms <= 2.0,
        "autosave UI enqueue exceeded 2.0 ms budget: mean={:.3} ms p99={:.3} ms",
        stats.mean_ms,
        stats.p99_ms
    );
}
