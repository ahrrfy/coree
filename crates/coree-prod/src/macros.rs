// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandboxed local automation engine powered by `rhai`:
//! - Strict resource limits (`max_operations`, expression depth, call depth, string/array/map caps,
//!   disabled `eval`, zero module/filesystem/shell access)
//! - Built-in production macro library (`batch-resize`, `auto-cut-contour`, `align-grid`,
//!   `recolor-cmyk`, `clean-empty-layers`)
//! - Execution of recorded macros emitted by [`coree_core::CommandHistory::stop_macro_to_rhai`]
//!   with atomic rollback on error or infinite-loop termination.

use crate::cut_packaging::generate_cut_contour;
use crate::error::{ProdError, ProdResult};
use coree_core::{
    apply_command, rhai_array_to_ids, Affine2D, AlignOp, Color, CommandHistory, CommandOp,
    Document, Fill, NodePayload, ObjectId, OutlineStyle, Point2D,
};
use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Strict sandbox resource limits protecting the host application from runaway scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroSandboxConfig {
    /// Maximum Rhai VM operations allowed before forcibly terminating execution (default: `100_000`).
    pub max_operations: u64,
    /// Maximum expression nesting depth (default: `32`).
    pub max_expr_depth: usize,
    /// Maximum function call stack depth (default: `32`).
    pub max_call_levels: usize,
    /// Maximum string byte length (default: `1 MiB`).
    pub max_string_size: usize,
    /// Maximum array element count (default: `10_000`).
    pub max_array_size: usize,
    /// Maximum object-map entry count (default: `1_000`).
    pub max_map_size: usize,
}

impl Default for MacroSandboxConfig {
    fn default() -> Self {
        Self {
            max_operations: 100_000,
            max_expr_depth: 32,
            max_call_levels: 32,
            max_string_size: 1_048_576,
            max_array_size: 10_000,
            max_map_size: 1_000,
        }
    }
}

/// Typed input parameter passed into a Rhai macro scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MacroValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
}

impl MacroValue {
    fn into_dynamic(self) -> Dynamic {
        match self {
            Self::Int(v) => Dynamic::from(v),
            Self::Float(v) => Dynamic::from(v),
            Self::Bool(v) => Dynamic::from(v),
            Self::String(v) => Dynamic::from(v),
        }
    }
}

/// Descriptor for a built-in safe automation script shipped with `coree-prod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMacro {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub script: &'static str,
}

/// Result returned by [`run_macro`] and [`run_recorded_macro`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MacroExecutionResult {
    pub commands_executed: usize,
    pub logs: Vec<String>,
}

/// Returns the built-in library of safe production automation Rhai scripts.
#[must_use]
pub fn builtin_macros() -> Vec<BuiltinMacro> {
    vec![
        BuiltinMacro {
            id: "batch-resize",
            name: "Batch Resize Objects",
            description:
                "Resizes every object on the active page to target_w_mm x target_h_mm (defaults 50x50 mm).",
            script: r#"
let w = if is_def_var("target_w_mm") { target_w_mm } else { 50.0 };
let h = if is_def_var("target_h_mm") { target_h_mm } else { 50.0 };
let ids = all_object_ids();
let count = resize_objects(ids, w, h);
log("Resized " + count + " object(s) to " + w + "x" + h + " mm");
"#,
        },
        BuiltinMacro {
            id: "auto-cut-contour",
            name: "Auto CutContour Bleed",
            description:
                "Welds all non-cut objects on the active page and generates a CutContour spot hairline.",
            script: r#"
let offset = if is_def_var("offset_mm") { offset_mm } else { 2.0 };
let spot = if is_def_var("spot_name") { spot_name } else { "CutContour" };
let ids = non_cut_object_ids();
if ids.len() > 0 {
    let cid = generate_cut_contour(ids, offset, spot);
    log("Generated " + spot + " object #" + cid);
}
"#,
        },
        BuiltinMacro {
            id: "align-grid",
            name: "Align to Production Grid",
            description:
                "Arranges all objects on the active page into a tidy grid (cols, gap_x_mm, gap_y_mm).",
            script: r#"
let cols = if is_def_var("cols") { cols } else { 3 };
let gx = if is_def_var("gap_x_mm") { gap_x_mm } else { 5.0 };
let gy = if is_def_var("gap_y_mm") { gap_y_mm } else { 5.0 };
let sx = if is_def_var("start_x_mm") { start_x_mm } else { 10.0 };
let sy = if is_def_var("start_y_mm") { start_y_mm } else { 10.0 };
let ids = all_object_ids();
let moved = align_grid(ids, cols, gx, gy, sx, sy);
log("Aligned " + moved + " object(s) into " + cols + "-column grid");
"#,
        },
        BuiltinMacro {
            id: "recolor-cmyk",
            name: "Recolor All to CMYK",
            description:
                "Converts all RGB/HSL fills, outlines, and text colors across the document to print CMYK.",
            script: r#"
let converted = recolor_all_to_cmyk();
log("Converted " + converted + " color(s) to CMYK");
"#,
        },
        BuiltinMacro {
            id: "clean-empty-layers",
            name: "Clean Empty Layers",
            description:
                "Removes empty unused layers across every page while retaining at least one layer per page.",
            script: r#"
let removed = clean_empty_layers();
log("Removed " + removed + " empty layer(s)");
"#,
        },
    ]
}

/// Looks up a built-in macro by its identifier (e.g. `"batch-resize"` or `"recolor-cmyk"`).
#[must_use]
pub fn find_builtin_macro(id: &str) -> Option<BuiltinMacro> {
    builtin_macros()
        .into_iter()
        .find(|m| m.id.eq_ignore_ascii_case(id.trim()))
}

fn new_sandboxed_engine(config: &MacroSandboxConfig) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(config.max_operations.max(1));
    engine.set_max_expr_depths(config.max_expr_depth, config.max_expr_depth);
    engine.set_max_call_levels(config.max_call_levels);
    engine.set_max_string_size(config.max_string_size);
    engine.set_max_array_size(config.max_array_size);
    engine.set_max_map_size(config.max_map_size);
    engine.set_max_modules(0);
    // Block dynamic code evaluation inside the sandbox
    engine.disable_symbol("eval");
    engine
}

fn dyn_to_f64(v: Dynamic, field: &str) -> Result<f64, Box<EvalAltResult>> {
    v.as_float()
        .or_else(|_| v.as_int().map(|n| n as f64))
        .map_err(|_| format!("'{field}' must be numeric").into())
}

/// Runs a production Rhai automation script against `doc` inside a strict sandbox.
///
/// Execution is transactional: if the script fails or hits `config.max_operations`,
/// `doc` remains completely unmodified.
pub fn run_macro(
    doc: &mut Document,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
) -> ProdResult<MacroExecutionResult> {
    let work_doc = Rc::new(RefCell::new(doc.clone()));
    let logs = Rc::new(RefCell::new(Vec::<String>::new()));
    let ops_count = Rc::new(RefCell::new(0usize));

    let mut engine = new_sandboxed_engine(config);
    register_production_api(&mut engine, &work_doc, &logs, &ops_count);

    let mut scope = Scope::new();
    for (k, v) in vars {
        scope.push_dynamic(k.clone(), v.clone().into_dynamic());
    }

    engine
        .run_with_scope(&mut scope, script)
        .map_err(|e| ProdError::MacroError(e.to_string()))?;

    // Commit atomically only after sandboxed execution completes cleanly
    *doc = work_doc.borrow().clone();
    let result = MacroExecutionResult {
        commands_executed: *ops_count.borrow(),
        logs: logs.borrow().clone(),
    };
    Ok(result)
}

/// Runs a recorded macro emitted by [`CommandHistory::stop_macro_to_rhai`] inside our
/// strictly sandboxed engine, committing all recorded commands as a single undoable
/// transaction on `history`.
pub fn run_recorded_macro(
    doc: &mut Document,
    history: &mut CommandHistory,
    script: &str,
    config: &MacroSandboxConfig,
) -> ProdResult<MacroExecutionResult> {
    let queued: Rc<RefCell<Vec<CommandOp>>> = Rc::new(RefCell::new(Vec::new()));
    let logs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let mut engine = new_sandboxed_engine(config);

    {
        let l = Rc::clone(&logs);
        engine.register_fn("log", move |msg: &str| {
            l.borrow_mut().push(msg.to_owned());
        });
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "transform_objects",
            move |ids: rhai::Array, m: rhai::Array| -> Result<(), Box<EvalAltResult>> {
                if m.len() != 6 {
                    return Err("transform_objects expects 6 affine coefficients".into());
                }
                let mut coeffs = [0.0_f64; 6];
                for (i, v) in m.into_iter().enumerate() {
                    let val = dyn_to_f64(v, "affine")?;
                    if !val.is_finite() {
                        return Err("transform_objects requires finite affine coefficients".into());
                    }
                    coeffs[i] = val;
                }
                q.borrow_mut().push(CommandOp::TransformObjects {
                    ids: rhai_array_to_ids(ids),
                    affine: Affine2D { m: coeffs },
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "set_fill_hex",
            move |ids: rhai::Array, hex: &str| -> Result<(), Box<EvalAltResult>> {
                let color = Color::from_hex(hex)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                q.borrow_mut().push(CommandOp::SetFill {
                    ids: rhai_array_to_ids(ids),
                    fill: Fill::Solid(color),
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "align_objects",
            move |ids: rhai::Array, mode: &str| -> Result<(), Box<EvalAltResult>> {
                let op = parse_align_mode(mode)?;
                q.borrow_mut().push(CommandOp::AlignObjects {
                    ids: rhai_array_to_ids(ids),
                    op,
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn("delete_objects", move |ids: rhai::Array| {
            q.borrow_mut().push(CommandOp::DeleteObjects {
                ids: rhai_array_to_ids(ids),
            });
        });
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn("group_objects", move |ids: rhai::Array, name: &str| {
            q.borrow_mut().push(CommandOp::Group {
                ids: rhai_array_to_ids(ids),
                name: name.to_owned(),
            });
        });
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "convert_to_curves",
            move |id: i64| -> Result<(), Box<EvalAltResult>> {
                if id <= 0 {
                    return Err(format!("invalid object id {id}").into());
                }
                q.borrow_mut().push(CommandOp::ConvertToCurves {
                    id: ObjectId(id as u64),
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "add_page",
            move |name: &str,
                  w: Dynamic,
                  h: Dynamic,
                  b: Dynamic|
                  -> Result<(), Box<EvalAltResult>> {
                let width_mm = dyn_to_f64(w, "width_mm")?;
                let height_mm = dyn_to_f64(h, "height_mm")?;
                let bleed_mm = dyn_to_f64(b, "bleed_mm")?;
                if !width_mm.is_finite()
                    || width_mm <= 0.0
                    || !height_mm.is_finite()
                    || height_mm <= 0.0
                    || !bleed_mm.is_finite()
                    || bleed_mm < 0.0
                {
                    return Err(
                        "add_page requires positive finite width/height and non-negative finite bleed"
                            .into(),
                    );
                }
                q.borrow_mut().push(CommandOp::AddPage {
                    name: name.to_owned(),
                    width_mm,
                    height_mm,
                    bleed_mm,
                });
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "exec_command_json",
            move |json: &str| -> Result<(), Box<EvalAltResult>> {
                let op: CommandOp = serde_json::from_str(json)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                q.borrow_mut().push(op);
                Ok(())
            },
        );
    }

    engine
        .run(script)
        .map_err(|e| ProdError::MacroError(e.to_string()))?;

    let ops = queued.borrow().clone();
    let count = ops.len();
    if count > 0 {
        history.begin_transaction(doc, "Run Recorded Macro")?;
        for op in ops {
            if let Err(err) = history.execute(doc, op) {
                let _ = history.cancel_transaction(doc);
                return Err(ProdError::Core(err));
            }
        }
        history.commit_transaction(doc)?;
    }

    let collected_logs = logs.borrow().clone();
    Ok(MacroExecutionResult {
        commands_executed: count,
        logs: collected_logs,
    })
}

fn parse_align_mode(mode: &str) -> Result<AlignOp, Box<EvalAltResult>> {
    match mode {
        "left" => Ok(AlignOp::Left),
        "center_h" => Ok(AlignOp::CenterH),
        "right" => Ok(AlignOp::Right),
        "top" => Ok(AlignOp::Top),
        "center_v" => Ok(AlignOp::CenterV),
        "bottom" => Ok(AlignOp::Bottom),
        "center_page" => Ok(AlignOp::CenterPage),
        other => Err(format!("unknown align mode '{other}'").into()),
    }
}

fn register_production_api(
    engine: &mut Engine,
    doc: &Rc<RefCell<Document>>,
    logs: &Rc<RefCell<Vec<String>>>,
    ops_count: &Rc<RefCell<usize>>,
) {
    // Logging (captured cleanly without stdout)
    {
        let l = Rc::clone(logs);
        engine.register_fn("log", move |msg: &str| {
            l.borrow_mut().push(msg.to_owned());
        });
    }

    // Introspection helpers
    {
        let d = Rc::clone(doc);
        engine.register_fn("all_object_ids", move || -> rhai::Array {
            let doc = d.borrow();
            let Some(page) = doc.active_page() else {
                return Vec::new();
            };
            page.layers
                .iter()
                .flat_map(|l| l.objects.iter())
                .map(|id| Dynamic::from(id.0 as i64))
                .collect()
        });
    }
    {
        let d = Rc::clone(doc);
        engine.register_fn("non_cut_object_ids", move || -> rhai::Array {
            let doc = d.borrow();
            let Some(page) = doc.active_page() else {
                return Vec::new();
            };
            page.layers
                .iter()
                .filter(|l| !l.is_cut_layer)
                .flat_map(|l| l.objects.iter())
                .map(|id| Dynamic::from(id.0 as i64))
                .collect()
        });
    }
    {
        let d = Rc::clone(doc);
        engine.register_fn("object_count", move || -> i64 {
            d.borrow().objects.len() as i64
        });
    }
    {
        let d = Rc::clone(doc);
        engine.register_fn("page_width_mm", move || -> f64 {
            d.borrow().active_page().map_or(0.0, |p| p.width_mm)
        });
    }
    {
        let d = Rc::clone(doc);
        engine.register_fn("page_height_mm", move || -> f64 {
            d.borrow().active_page().map_or(0.0, |p| p.height_mm)
        });
    }

    // Standard command API (compatible with recorded Rhai macros)
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "transform_objects",
            move |ids: rhai::Array, m: rhai::Array| -> Result<(), Box<EvalAltResult>> {
                if m.len() != 6 {
                    return Err("transform_objects expects 6 affine coefficients".into());
                }
                let mut coeffs = [0.0_f64; 6];
                for (i, v) in m.into_iter().enumerate() {
                    let val = dyn_to_f64(v, "affine")?;
                    if !val.is_finite() {
                        return Err("transform_objects requires finite affine coefficients".into());
                    }
                    coeffs[i] = val;
                }
                let op = CommandOp::TransformObjects {
                    ids: rhai_array_to_ids(ids),
                    affine: Affine2D { m: coeffs },
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "set_fill_hex",
            move |ids: rhai::Array, hex: &str| -> Result<(), Box<EvalAltResult>> {
                let color = Color::from_hex(hex)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                let op = CommandOp::SetFill {
                    ids: rhai_array_to_ids(ids),
                    fill: Fill::Solid(color),
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "align_objects",
            move |ids: rhai::Array, mode: &str| -> Result<(), Box<EvalAltResult>> {
                let align = parse_align_mode(mode)?;
                let op = CommandOp::AlignObjects {
                    ids: rhai_array_to_ids(ids),
                    op: align,
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "delete_objects",
            move |ids: rhai::Array| -> Result<(), Box<EvalAltResult>> {
                let op = CommandOp::DeleteObjects {
                    ids: rhai_array_to_ids(ids),
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "group_objects",
            move |ids: rhai::Array, name: &str| -> Result<(), Box<EvalAltResult>> {
                let op = CommandOp::Group {
                    ids: rhai_array_to_ids(ids),
                    name: name.to_owned(),
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "convert_to_curves",
            move |id: i64| -> Result<(), Box<EvalAltResult>> {
                if id <= 0 {
                    return Err(format!("invalid object id {id}").into());
                }
                let op = CommandOp::ConvertToCurves {
                    id: ObjectId(id as u64),
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "add_page",
            move |name: &str,
                  w: Dynamic,
                  h: Dynamic,
                  b: Dynamic|
                  -> Result<(), Box<EvalAltResult>> {
                let width_mm = dyn_to_f64(w, "width_mm")?;
                let height_mm = dyn_to_f64(h, "height_mm")?;
                let bleed_mm = dyn_to_f64(b, "bleed_mm")?;
                if !width_mm.is_finite()
                    || width_mm <= 0.0
                    || !height_mm.is_finite()
                    || height_mm <= 0.0
                    || !bleed_mm.is_finite()
                    || bleed_mm < 0.0
                {
                    return Err(
                        "add_page requires positive finite width/height and non-negative finite bleed"
                            .into(),
                    );
                }
                let op = CommandOp::AddPage {
                    name: name.to_owned(),
                    width_mm,
                    height_mm,
                    bleed_mm,
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "exec_command_json",
            move |json: &str| -> Result<(), Box<EvalAltResult>> {
                let op: CommandOp = serde_json::from_str(json)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }

    // Production automation host primitives
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "resize_objects",
            move |ids: rhai::Array,
                  w_dyn: Dynamic,
                  h_dyn: Dynamic|
                  -> Result<i64, Box<EvalAltResult>> {
                let target_w = dyn_to_f64(w_dyn, "target_w_mm")?;
                let target_h = dyn_to_f64(h_dyn, "target_h_mm")?;
                if !target_w.is_finite()
                    || target_w <= 0.0
                    || !target_h.is_finite()
                    || target_h <= 0.0
                {
                    return Err("resize_objects requires positive finite dimensions".into());
                }
                let obj_ids = rhai_array_to_ids(ids);
                let mut resized = 0i64;
                let mut doc_mut = d.borrow_mut();
                for oid in obj_ids {
                    let Some(bb) = doc_mut.object_bounds(oid) else {
                        continue;
                    };
                    if bb.width() <= 1e-6 || bb.height() <= 1e-6 {
                        continue;
                    }
                    let sx = target_w / bb.width();
                    let sy = target_h / bb.height();
                    if !sx.is_finite() || !sy.is_finite() {
                        continue;
                    }
                    let pivot = Point2D::new(bb.min_x, bb.min_y);
                    let scale_xf = Affine2D::scale_about(sx, sy, pivot);
                    if let Some(obj) = doc_mut.objects.get_mut(&oid) {
                        if !obj.locked {
                            obj.transform = scale_xf.compose(&obj.transform);
                            resized += 1;
                        }
                    }
                }
                if resized > 0 {
                    doc_mut.touch();
                    *c.borrow_mut() += 1;
                }
                Ok(resized)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "translate_objects",
            move |ids: rhai::Array,
                  dx_dyn: Dynamic,
                  dy_dyn: Dynamic|
                  -> Result<i64, Box<EvalAltResult>> {
                let dx = dyn_to_f64(dx_dyn, "dx_mm")?;
                let dy = dyn_to_f64(dy_dyn, "dy_mm")?;
                if !dx.is_finite() || !dy.is_finite() {
                    return Err("translate_objects requires finite offsets".into());
                }
                let obj_ids = rhai_array_to_ids(ids);
                let op = CommandOp::TransformObjects {
                    ids: obj_ids.clone(),
                    affine: Affine2D::translate(dx, dy),
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(obj_ids.len() as i64)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "generate_cut_contour",
            move |ids: rhai::Array,
                  offset_dyn: Dynamic,
                  spot_name: &str|
                  -> Result<i64, Box<EvalAltResult>> {
                let offset_mm = dyn_to_f64(offset_dyn, "offset_mm")?;
                let obj_ids = rhai_array_to_ids(ids);
                let res = generate_cut_contour(&mut d.borrow_mut(), &obj_ids, offset_mm, spot_name)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(res.object_id.0 as i64)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "align_grid",
            move |ids: rhai::Array,
                  cols: i64,
                  gx_dyn: Dynamic,
                  gy_dyn: Dynamic,
                  sx_dyn: Dynamic,
                  sy_dyn: Dynamic|
                  -> Result<i64, Box<EvalAltResult>> {
                if cols <= 0 {
                    return Err("align_grid cols must be >= 1".into());
                }
                let cols_usize = cols as usize;
                let gx = dyn_to_f64(gx_dyn, "gap_x_mm")?;
                let gy = dyn_to_f64(gy_dyn, "gap_y_mm")?;
                let sx = dyn_to_f64(sx_dyn, "start_x_mm")?;
                let sy = dyn_to_f64(sy_dyn, "start_y_mm")?;
                if !gx.is_finite()
                    || gx < 0.0
                    || !gy.is_finite()
                    || gy < 0.0
                    || !sx.is_finite()
                    || !sy.is_finite()
                {
                    return Err(
                        "align_grid requires finite start coordinates and finite non-negative gaps"
                            .into(),
                    );
                }
                let obj_ids = rhai_array_to_ids(ids);

                let mut doc_mut = d.borrow_mut();
                let mut cell_w = 0.0_f64;
                let mut cell_h = 0.0_f64;
                let mut valid = Vec::new();
                for oid in obj_ids {
                    if let Some(bb) = doc_mut.object_bounds(oid) {
                        cell_w = cell_w.max(bb.width());
                        cell_h = cell_h.max(bb.height());
                        valid.push((oid, bb));
                    }
                }
                let mut moved = 0i64;
                for (idx, (oid, bb)) in valid.into_iter().enumerate() {
                    let row = idx / cols_usize;
                    let col = idx % cols_usize;
                    let target_x = sx + (col as f64) * (cell_w + gx);
                    let target_y = sy + (row as f64) * (cell_h + gy);
                    let dx = target_x - bb.min_x;
                    let dy = target_y - bb.min_y;
                    if !dx.is_finite() || !dy.is_finite() {
                        continue;
                    }
                    if let Some(obj) = doc_mut.objects.get_mut(&oid) {
                        if !obj.locked {
                            obj.transform = Affine2D::translate(dx, dy).compose(&obj.transform);
                            moved += 1;
                        }
                    }
                }
                if moved > 0 {
                    doc_mut.touch();
                    *c.borrow_mut() += 1;
                }
                Ok(moved)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn("recolor_all_to_cmyk", move || -> i64 {
            let mut doc_mut = d.borrow_mut();
            let mut converted = 0i64;
            for obj in doc_mut.objects.values_mut() {
                if obj.locked {
                    continue;
                }
                converted += convert_fill_to_cmyk(&mut obj.fill);
                if obj.outline.is_visible() && convert_color_to_cmyk(&mut obj.outline.color) {
                    converted += 1;
                }
                match &mut obj.payload {
                    NodePayload::ArtisticText(t) => {
                        for r in &mut t.runs {
                            if convert_color_to_cmyk(&mut r.style.fill) {
                                converted += 1;
                            }
                        }
                    }
                    NodePayload::ParagraphText(t) => {
                        for r in &mut t.runs {
                            if convert_color_to_cmyk(&mut r.style.fill) {
                                converted += 1;
                            }
                        }
                    }
                    NodePayload::TextOnPath(top) => {
                        for r in &mut top.text.runs {
                            if convert_color_to_cmyk(&mut r.style.fill) {
                                converted += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if converted > 0 {
                doc_mut.touch();
                *c.borrow_mut() += 1;
            }
            converted
        });
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn("clean_empty_layers", move || -> i64 {
            let mut doc_mut = d.borrow_mut();
            let mut removed = 0i64;
            for page in &mut doc_mut.pages {
                let mut idx = 0usize;
                while idx < page.layers.len() && page.layers.len() > 1 {
                    if page.layers[idx].objects.is_empty() && !page.layers[idx].locked {
                        page.layers.remove(idx);
                        removed += 1;
                    } else {
                        idx += 1;
                    }
                }
            }
            if removed > 0 {
                doc_mut.touch();
                *c.borrow_mut() += 1;
            }
            removed
        });
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "set_cmyk_fill",
            move |ids: rhai::Array,
                  c_dyn: Dynamic,
                  m_dyn: Dynamic,
                  y_dyn: Dynamic,
                  k_dyn: Dynamic|
                  -> Result<(), Box<EvalAltResult>> {
                let cv = dyn_to_f64(c_dyn, "c")?;
                let mv = dyn_to_f64(m_dyn, "m")?;
                let yv = dyn_to_f64(y_dyn, "y")?;
                let kv = dyn_to_f64(k_dyn, "k")?;
                if !cv.is_finite() || !mv.is_finite() || !yv.is_finite() || !kv.is_finite() {
                    return Err("set_cmyk_fill requires finite channel values".into());
                }
                let color = Color::cmyk(cv as f32, mv as f32, yv as f32, kv as f32);
                let op = CommandOp::SetFill {
                    ids: rhai_array_to_ids(ids),
                    fill: Fill::Solid(color),
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let c = Rc::clone(ops_count);
        engine.register_fn(
            "set_outline_mm",
            move |ids: rhai::Array, w_dyn: Dynamic| -> Result<(), Box<EvalAltResult>> {
                let w = dyn_to_f64(w_dyn, "width_mm")?;
                if !w.is_finite() || w < 0.0 {
                    return Err(
                        "set_outline_mm requires a finite non-negative outline width".into(),
                    );
                }
                let op = CommandOp::SetOutline {
                    ids: rhai_array_to_ids(ids),
                    outline: OutlineStyle::solid(Color::KEY_BLACK, w),
                };
                apply_command(&mut d.borrow_mut(), &op)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
}

fn convert_color_to_cmyk(color: &mut Color) -> bool {
    match color {
        Color::Rgba { .. } | Color::Hsl { .. } => {
            *color = color.as_cmyk_color();
            true
        }
        _ => false,
    }
}

fn convert_fill_to_cmyk(fill: &mut Fill) -> i64 {
    let mut n = 0i64;
    match fill {
        Fill::Solid(c) => {
            if convert_color_to_cmyk(c) {
                n += 1;
            }
        }
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => {
            for s in stops {
                if convert_color_to_cmyk(&mut s.color) {
                    n += 1;
                }
            }
        }
        Fill::TwoColorPattern { fg, bg, .. } | Fill::TextureFill { fg, bg, .. } => {
            if convert_color_to_cmyk(fg) {
                n += 1;
            }
            if convert_color_to_cmyk(bg) {
                n += 1;
            }
        }
        Fill::MeshFill { nodes, .. } => {
            for node in nodes {
                if convert_color_to_cmyk(&mut node.color) {
                    n += 1;
                }
            }
        }
        Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {}
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, MasterLayerScope, ParametricShape, SceneObject};

    fn rect_obj(x: f64, y: f64, w: f64, h: f64, color: Color) -> SceneObject {
        SceneObject::new(
            ObjectId(0),
            "Box",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: w,
                height: h,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(color))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(x, y))
    }

    #[test]
    fn all_five_builtin_macros_execute_safely() {
        assert_eq!(builtin_macros().len(), 5);
        let cfg = MacroSandboxConfig::default();

        let mut doc = Document::new_a4("Automation");
        let page_id = doc.pages[0].id;
        let a = doc
            .add_object(
                rect_obj(5.0, 5.0, 20.0, 10.0, Color::rgb(1.0, 0.0, 0.0)),
                None,
            )
            .unwrap();
        let b = doc
            .add_object(
                rect_obj(40.0, 5.0, 30.0, 15.0, Color::rgb(0.0, 0.5, 1.0)),
                None,
            )
            .unwrap();
        // Add an empty layer to clean
        doc.add_layer(page_id, "Unused Empty", false, MasterLayerScope::None)
            .unwrap();

        // 1. batch-resize to 40x25 mm
        let mut vars = BTreeMap::new();
        vars.insert("target_w_mm".to_owned(), MacroValue::Float(40.0));
        vars.insert("target_h_mm".to_owned(), MacroValue::Float(25.0));
        let m_resize = find_builtin_macro("batch-resize").unwrap();
        let r1 = run_macro(&mut doc, m_resize.script, &vars, &cfg).unwrap();
        assert!(r1.commands_executed >= 1);
        assert!((doc.object_bounds(a).unwrap().width() - 40.0).abs() < 1e-4);
        assert!((doc.object_bounds(b).unwrap().height() - 25.0).abs() < 1e-4);

        // 2. align-grid
        let mut grid_vars = BTreeMap::new();
        grid_vars.insert("cols".to_owned(), MacroValue::Int(2));
        grid_vars.insert("gap_x_mm".to_owned(), MacroValue::Float(10.0));
        grid_vars.insert("start_x_mm".to_owned(), MacroValue::Float(15.0));
        grid_vars.insert("start_y_mm".to_owned(), MacroValue::Float(20.0));
        run_macro(
            &mut doc,
            find_builtin_macro("align-grid").unwrap().script,
            &grid_vars,
            &cfg,
        )
        .unwrap();
        assert!((doc.object_bounds(a).unwrap().min_x - 15.0).abs() < 1e-4);
        assert!((doc.object_bounds(b).unwrap().min_x - 65.0).abs() < 1e-4);

        // 3. recolor-cmyk
        run_macro(
            &mut doc,
            find_builtin_macro("recolor-cmyk").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        assert!(matches!(
            doc.objects[&a].fill,
            Fill::Solid(Color::Cmyk { .. })
        ));
        assert!(matches!(
            doc.objects[&b].fill,
            Fill::Solid(Color::Cmyk { .. })
        ));

        // 4. clean-empty-layers
        assert_eq!(doc.pages[0].layers.len(), 2);
        run_macro(
            &mut doc,
            find_builtin_macro("clean-empty-layers").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        assert_eq!(doc.pages[0].layers.len(), 1);

        // 5. auto-cut-contour
        run_macro(
            &mut doc,
            find_builtin_macro("auto-cut-contour").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        assert!(doc.pages[0]
            .layers
            .iter()
            .any(|l| l.is_cut_layer && !l.objects.is_empty()));
    }

    #[test]
    fn recorded_macro_executes_and_sandbox_blocks_infinite_loops_with_rollback() {
        let mut doc = Document::new_a4("Recorded Macro");
        let mut hist = CommandHistory::default();
        let id = doc
            .add_object(rect_obj(10.0, 10.0, 20.0, 20.0, Color::CYAN), None)
            .unwrap();

        hist.start_macro_recording();
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![id],
                affine: Affine2D::translate(30.0, 40.0),
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::SetFill {
                ids: vec![id],
                fill: Fill::Solid(Color::from_hex("#2266cc").unwrap()),
            },
        )
        .unwrap();
        let recorded_script = hist.stop_macro_to_rhai();

        // Replay onto fresh document using run_recorded_macro
        let mut doc2 = Document::new_a4("Target");
        let mut obj = rect_obj(10.0, 10.0, 20.0, 20.0, Color::CYAN);
        obj.id = id;
        doc2.add_object(obj, None).unwrap();
        let mut hist2 = CommandHistory::default();

        let res = run_recorded_macro(
            &mut doc2,
            &mut hist2,
            &recorded_script,
            &MacroSandboxConfig::default(),
        )
        .unwrap();
        assert_eq!(res.commands_executed, 2);
        let bb = doc2.object_bounds(id).unwrap();
        assert!((bb.min_x - 40.0).abs() < 1e-6);
        assert!((bb.min_y - 50.0).abs() < 1e-6);

        // Infinite loop must be aborted by max_operations AND roll back prior mutations!
        let snapshot_before = doc2.clone();
        let strict = MacroSandboxConfig {
            max_operations: 5_000,
            ..MacroSandboxConfig::default()
        };
        let loop_err = run_macro(
            &mut doc2,
            "translate_objects(all_object_ids(), 99.0, 99.0); loop { let x = 1 + 1; }",
            &BTreeMap::new(),
            &strict,
        );
        assert!(loop_err.is_err());
        assert_eq!(doc2, snapshot_before, "failed macro must roll back cleanly");

        // `eval` must also be blocked in the sandbox
        let eval_err = run_macro(&mut doc2, "eval(\"1 + 1\")", &BTreeMap::new(), &strict);
        assert!(eval_err.is_err());

        // `import` module escape must be blocked (`max_modules = 0`)
        let import_err = run_macro(
            &mut doc2,
            "import \"/etc/passwd\" as p;",
            &BTreeMap::new(),
            &strict,
        );
        assert!(import_err.is_err());

        // Unbounded recursion must be blocked by `max_call_levels`
        let rec_err = run_macro(
            &mut doc2,
            "fn f(n) { f(n + 1) } f(0);",
            &BTreeMap::new(),
            &strict,
        );
        assert!(rec_err.is_err());
    }
}
