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
use crate::find_replace::{evaluate_cql_query, FindScope};
use coree_core::{
    rhai_array_to_ids, Affine2D, AlignOp, ArtisticText, Color, CommandHistory, CommandOp, Document,
    Effect, Fill, LayerId, LensKind, NodePayload, ObjectId, OutlineStyle, Point2D,
};
use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

/// Strict sandbox resource limits protecting the host application from runaway scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroSandboxConfig {
    /// Maximum Rhai VM operations allowed before forcibly terminating execution (default: `500_000`).
    pub max_operations: u64,
    /// Maximum expression nesting depth (default: `32`).
    pub max_expr_depth: usize,
    /// Maximum function call stack depth (default: `32`).
    pub max_call_levels: usize,
    /// Maximum string byte length (default: `1 MiB`).
    pub max_string_size: usize,
    /// Maximum array element count (default: `65_536`).
    pub max_array_size: usize,
    /// Maximum object-map entry count (default: `4_096`).
    pub max_map_size: usize,
}

impl Default for MacroSandboxConfig {
    fn default() -> Self {
        Self {
            max_operations: 500_000,
            max_expr_depth: 32,
            max_call_levels: 32,
            max_string_size: 1_048_576,
            max_array_size: 65_536,
            max_map_size: 4_096,
        }
    }
}

/// Lifecycle hook event kinds for automated Rhai macro listeners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MacroEventHook {
    OnOpen,
    OnBeforeExport,
    OnSelectionChange,
}

impl MacroEventHook {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnOpen => "on_open",
            Self::OnBeforeExport => "on_before_export",
            Self::OnSelectionChange => "on_selection_change",
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

/// Plans a production Rhai automation script against `doc` inside a strict sandbox,
/// Plans a Rhai automation script against `doc` without mutating it, returning the execution
/// result and the exact replayable [`CommandOp`] sequence.
pub fn plan_macro_ops(
    doc: &Document,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
    plan_macro_ops_with_selection(doc, script, vars, config, &[])
}

/// Variant of [`plan_macro_ops`] seeding `doc.selection` from `initial_selection`.
pub fn plan_macro_ops_with_selection(
    doc: &Document,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
    initial_selection: &[ObjectId],
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
    let (_new_doc, result, ops) =
        run_macro_inner_with_selection(doc, script, vars, config, initial_selection)?;
    Ok((result, ops))
}

/// Runs a production Rhai automation script against `doc` inside a strict sandbox,
/// returning both the execution summary and the exact replayable [`CommandOp`] sequence.
///
/// Execution is transactional: if the script fails or hits `config.max_operations`,
/// `doc` remains completely unmodified.
pub fn run_macro_with_ops(
    doc: &mut Document,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
    run_macro_with_ops_and_selection(doc, script, vars, config, &[])
}

/// Variant of [`run_macro_with_ops`] seeding `doc.selection` from `initial_selection`.
pub fn run_macro_with_ops_and_selection(
    doc: &mut Document,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
    initial_selection: &[ObjectId],
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
    let (new_doc, result, ops) =
        run_macro_inner_with_selection(doc, script, vars, config, initial_selection)?;
    *doc = new_doc;
    Ok((result, ops))
}

fn run_macro_inner_with_selection(
    doc: &Document,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
    initial_selection: &[ObjectId],
) -> ProdResult<(Document, MacroExecutionResult, Vec<CommandOp>)> {
    let work_doc = Rc::new(RefCell::new(doc.clone()));
    let history = Rc::new(RefCell::new(CommandHistory::new(256)));
    let logs = Rc::new(RefCell::new(Vec::<String>::new()));
    let ops_count = Rc::new(RefCell::new(0usize));
    let recorded_ops = Rc::new(RefCell::new(Vec::<CommandOp>::new()));
    let selection = Rc::new(RefCell::new(initial_selection.to_vec()));

    let mut engine = new_sandboxed_engine(config);
    register_production_api(
        &mut engine,
        &work_doc,
        &history,
        &logs,
        &ops_count,
        &recorded_ops,
    );
    let dom_doc = register_hierarchical_scene_dom(
        &mut engine,
        &work_doc,
        &history,
        &logs,
        &ops_count,
        &recorded_ops,
        &selection,
    );

    let mut scope = Scope::new();
    scope.push("doc", dom_doc);
    for (k, v) in vars {
        scope.push_dynamic(k.clone(), v.clone().into_dynamic());
    }

    engine
        .run_with_scope(&mut scope, script)
        .map_err(|e| ProdError::MacroError(e.to_string()))?;

    let new_doc = work_doc.borrow().clone();
    let ops = recorded_ops.borrow().clone();
    let result = MacroExecutionResult {
        commands_executed: *ops_count.borrow(),
        logs: logs.borrow().clone(),
    };
    Ok((new_doc, result, ops))
}

/// Runs a lifecycle hook script (`on_open`, `on_before_export`, `on_selection_change`) against `doc`.
pub fn run_macro_event_hook(
    doc: &mut Document,
    hook: MacroEventHook,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
    run_macro_event_hook_with_selection(doc, hook, script, vars, config, &[])
}

/// Variant of [`run_macro_event_hook`] seeding `doc.selection` from `initial_selection`.
pub fn run_macro_event_hook_with_selection(
    doc: &mut Document,
    hook: MacroEventHook,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
    initial_selection: &[ObjectId],
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
    let mut hook_vars = vars.clone();
    hook_vars.insert(
        "hook_event".to_owned(),
        MacroValue::String(hook.as_str().to_owned()),
    );
    run_macro_with_ops_and_selection(doc, script, &hook_vars, config, initial_selection)
}

/// Convenience dispatcher for the [`MacroEventHook::OnBeforeExport`] lifecycle hook.
pub fn on_before_export(
    doc: &mut Document,
    script: &str,
    vars: &BTreeMap<String, MacroValue>,
    config: &MacroSandboxConfig,
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
    run_macro_event_hook(doc, MacroEventHook::OnBeforeExport, script, vars, config)
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
    let (result, _ops) = run_macro_with_ops(doc, script, vars, config)?;
    Ok(result)
}

/// Parses and validates a recorded macro emitted by [`CommandHistory::stop_macro_to_rhai`]
/// inside the sandboxed Rhai engine, returning the execution summary and queued [`CommandOp`]s
/// without mutating a [`Document`].
pub fn plan_recorded_macro_ops(
    script: &str,
    config: &MacroSandboxConfig,
) -> ProdResult<(MacroExecutionResult, Vec<CommandOp>)> {
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
                let obj_ids = rhai_array_to_ids(ids);
                if !obj_ids.is_empty() {
                    q.borrow_mut().push(CommandOp::TransformObjects {
                        ids: obj_ids,
                        affine: Affine2D { m: coeffs },
                    });
                }
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
                let obj_ids = rhai_array_to_ids(ids);
                if !obj_ids.is_empty() {
                    q.borrow_mut().push(CommandOp::SetFill {
                        ids: obj_ids,
                        fill: Fill::Solid(color),
                    });
                }
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
                let obj_ids = rhai_array_to_ids(ids);
                if !obj_ids.is_empty() {
                    q.borrow_mut()
                        .push(CommandOp::AlignObjects { ids: obj_ids, op });
                }
                Ok(())
            },
        );
    }
    {
        let q = Rc::clone(&queued);
        engine.register_fn("delete_objects", move |ids: rhai::Array| {
            let obj_ids = rhai_array_to_ids(ids);
            if !obj_ids.is_empty() {
                q.borrow_mut()
                    .push(CommandOp::DeleteObjects { ids: obj_ids });
            }
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
    let collected_logs = logs.borrow().clone();
    Ok((
        MacroExecutionResult {
            commands_executed: count,
            logs: collected_logs,
        },
        ops,
    ))
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
    let (res, ops) = plan_recorded_macro_ops(script, config)?;
    if !ops.is_empty() {
        history.begin_transaction(doc, "Run Recorded Macro")?;
        for op in ops {
            if let Err(err) = history.execute(doc, op) {
                let _ = history.cancel_transaction(doc);
                return Err(ProdError::Core(err));
            }
        }
        history.commit_transaction(doc)?;
    }
    Ok(res)
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
    history: &Rc<RefCell<CommandHistory>>,
    logs: &Rc<RefCell<Vec<String>>>,
    ops_count: &Rc<RefCell<usize>>,
    recorded_ops: &Rc<RefCell<Vec<CommandOp>>>,
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
            d.borrow().object_count() as i64
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
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
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
                let obj_ids = rhai_array_to_ids(ids);
                if obj_ids.is_empty() {
                    return Ok(());
                }
                let op = CommandOp::TransformObjects {
                    ids: obj_ids,
                    affine: Affine2D { m: coeffs },
                };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "set_fill_hex",
            move |ids: rhai::Array, hex: &str| -> Result<(), Box<EvalAltResult>> {
                let color = Color::from_hex(hex)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                let obj_ids = rhai_array_to_ids(ids);
                if obj_ids.is_empty() {
                    return Ok(());
                }
                let op = CommandOp::SetFill {
                    ids: obj_ids,
                    fill: Fill::Solid(color),
                };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "align_objects",
            move |ids: rhai::Array, mode: &str| -> Result<(), Box<EvalAltResult>> {
                let align = parse_align_mode(mode)?;
                let obj_ids = rhai_array_to_ids(ids);
                if obj_ids.is_empty() {
                    return Ok(());
                }
                let op = CommandOp::AlignObjects {
                    ids: obj_ids,
                    op: align,
                };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "delete_objects",
            move |ids: rhai::Array| -> Result<(), Box<EvalAltResult>> {
                let obj_ids = rhai_array_to_ids(ids);
                if obj_ids.is_empty() {
                    return Ok(());
                }
                let op = CommandOp::DeleteObjects { ids: obj_ids };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "group_objects",
            move |ids: rhai::Array, name: &str| -> Result<(), Box<EvalAltResult>> {
                let op = CommandOp::Group {
                    ids: rhai_array_to_ids(ids),
                    name: name.to_owned(),
                };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "convert_to_curves",
            move |id: i64| -> Result<(), Box<EvalAltResult>> {
                if id <= 0 {
                    return Err(format!("invalid object id {id}").into());
                }
                let op = CommandOp::ConvertToCurves {
                    id: ObjectId(id as u64),
                };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "add_page",
            move |name: &str,
                  w: Dynamic,
                  h_val: Dynamic,
                  b: Dynamic|
                  -> Result<(), Box<EvalAltResult>> {
                let width_mm = dyn_to_f64(w, "width_mm")?;
                let height_mm = dyn_to_f64(h_val, "height_mm")?;
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
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "exec_command_json",
            move |json: &str| -> Result<(), Box<EvalAltResult>> {
                let op: CommandOp = serde_json::from_str(json)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }

    // Production automation host primitives
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
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
                let mut ops = Vec::new();
                let mut doc_mut = d.borrow_mut();
                let locked_ids = collect_locked_object_ids(&doc_mut);
                for oid in obj_ids {
                    if locked_ids.contains(&oid) {
                        continue;
                    }
                    let Some(bb) = doc_mut.world_object_bounds(oid) else {
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
                    if doc_mut.object(oid).is_none_or(|obj| obj.locked) {
                        continue;
                    }
                    let pivot = Point2D::new(bb.min_x, bb.min_y);
                    let scale_xf = Affine2D::scale_about(sx, sy, pivot);
                    let local_xf = if let Some(pid) = doc_mut.parent_container_of(oid) {
                        let Some(pw) = doc_mut.world_transform_of(pid) else {
                            continue;
                        };
                        let Some(inv_pw) = pw.inverse() else {
                            continue;
                        };
                        let conjugated = inv_pw.compose(&scale_xf).compose(&pw);
                        if !conjugated.is_finite() {
                            continue;
                        }
                        conjugated
                    } else {
                        scale_xf
                    };
                    ops.push(CommandOp::TransformObjects {
                        ids: vec![oid],
                        affine: local_xf,
                    });
                }
                let resized = ops.len() as i64;
                if !ops.is_empty() {
                    let batch = CommandOp::Batch(ops);
                    h.borrow_mut()
                        .execute(&mut doc_mut, batch.clone())
                        .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                    rec.borrow_mut().push(batch);
                    *c.borrow_mut() += 1;
                }
                Ok(resized)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
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
                if obj_ids.is_empty() {
                    return Ok(0);
                }
                let world_xf = Affine2D::translate(dx, dy);
                let mut doc_mut = d.borrow_mut();
                let effective_locked = doc_mut.effective_locked_object_ids();
                for &oid in &obj_ids {
                    if !doc_mut.has_object(oid) {
                        return Err(format!("object {oid} not found").into());
                    }
                    if effective_locked.contains(&oid) {
                        return Err(format!("object {oid} is locked").into());
                    }
                }
                let has_nested = obj_ids
                    .iter()
                    .any(|&oid| doc_mut.parent_container_of(oid).is_some());
                if !has_nested {
                    let count = obj_ids.len() as i64;
                    let op = CommandOp::TransformObjects {
                        ids: obj_ids,
                        affine: world_xf,
                    };
                    h.borrow_mut()
                        .execute(&mut doc_mut, op.clone())
                        .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                    rec.borrow_mut().push(op);
                    *c.borrow_mut() += 1;
                    return Ok(count);
                }
                let mut ops = Vec::with_capacity(obj_ids.len());
                for oid in obj_ids {
                    let local_xf = if let Some(pid) = doc_mut.parent_container_of(oid) {
                        let Some(pw) = doc_mut.world_transform_of(pid) else {
                            continue;
                        };
                        let Some(inv_pw) = pw.inverse() else {
                            continue;
                        };
                        let conjugated = inv_pw.compose(&world_xf).compose(&pw);
                        if !conjugated.is_finite() {
                            continue;
                        }
                        conjugated
                    } else {
                        world_xf
                    };
                    ops.push(CommandOp::TransformObjects {
                        ids: vec![oid],
                        affine: local_xf,
                    });
                }
                let count = ops.len() as i64;
                if !ops.is_empty() {
                    let batch = CommandOp::Batch(ops);
                    h.borrow_mut()
                        .execute(&mut doc_mut, batch.clone())
                        .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                    rec.borrow_mut().push(batch);
                    *c.borrow_mut() += 1;
                }
                Ok(count)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "generate_cut_contour",
            move |ids: rhai::Array,
                  offset_dyn: Dynamic,
                  spot_name: &str|
                  -> Result<i64, Box<EvalAltResult>> {
                let offset_mm = dyn_to_f64(offset_dyn, "offset_mm")?;
                let obj_ids = rhai_array_to_ids(ids);
                let mut doc_mut = d.borrow_mut();
                let mut hist_mut = h.borrow_mut();
                let existing_layers: HashSet<LayerId> = doc_mut
                    .pages()
                    .iter()
                    .flat_map(|p| p.layers.iter().map(|l| l.id))
                    .collect();
                hist_mut
                    .begin_transaction(&doc_mut, "Generate Cut Contour")
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                let res = match generate_cut_contour(&mut doc_mut, &obj_ids, offset_mm, spot_name) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = hist_mut.cancel_transaction(&mut doc_mut);
                        return Err(e.to_string().into());
                    }
                };
                let owner_page_id = doc_mut
                    .pages()
                    .iter()
                    .find(|p| p.layers.iter().any(|l| l.id == res.layer_id))
                    .map(|p| p.id);
                let mut batch_ops = Vec::new();
                if existing_layers.contains(&res.layer_id) {
                    batch_ops.push(CommandOp::SetLayerFlags {
                        layer_id: res.layer_id,
                        visible: Some(true),
                        locked: None,
                        printable: Some(true),
                        is_cut_layer: Some(true),
                    });
                } else if let (Some(page_id), Some(layer)) =
                    (owner_page_id, doc_mut.find_layer(res.layer_id))
                {
                    batch_ops.push(CommandOp::AddLayer {
                        page_id,
                        name: layer.name.clone(),
                        is_cut_layer: layer.is_cut_layer,
                        master_scope: layer.master_scope,
                    });
                }
                if let Some(created_obj) = doc_mut.object(res.object_id) {
                    batch_ops.push(CommandOp::create_object(
                        created_obj.clone(),
                        Some(res.layer_id),
                    ));
                }
                let _ = hist_mut.cancel_transaction(&mut doc_mut);
                let batch = CommandOp::Batch(batch_ops);
                hist_mut
                    .execute(&mut doc_mut, batch.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(batch);
                *c.borrow_mut() += 1;
                Ok(res.object_id.0 as i64)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
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
                let locked_ids = collect_locked_object_ids(&doc_mut);
                let mut cell_w = 0.0_f64;
                let mut cell_h = 0.0_f64;
                let mut valid = Vec::new();
                for oid in obj_ids {
                    if locked_ids.contains(&oid) {
                        continue;
                    }
                    if let Some(bb) = doc_mut.world_object_bounds(oid) {
                        cell_w = cell_w.max(bb.width());
                        cell_h = cell_h.max(bb.height());
                        valid.push((oid, bb));
                    }
                }
                let mut ops = Vec::new();
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
                    if doc_mut.object(oid).is_none_or(|obj| obj.locked) {
                        continue;
                    }
                    let world_xf = Affine2D::translate(dx, dy);
                    let local_xf = if let Some(pid) = doc_mut.parent_container_of(oid) {
                        let Some(pw) = doc_mut.world_transform_of(pid) else {
                            continue;
                        };
                        let Some(inv_pw) = pw.inverse() else {
                            continue;
                        };
                        let conjugated = inv_pw.compose(&world_xf).compose(&pw);
                        if !conjugated.is_finite() {
                            continue;
                        }
                        conjugated
                    } else {
                        world_xf
                    };
                    ops.push(CommandOp::TransformObjects {
                        ids: vec![oid],
                        affine: local_xf,
                    });
                }
                let moved = ops.len() as i64;
                if !ops.is_empty() {
                    let batch = CommandOp::Batch(ops);
                    h.borrow_mut()
                        .execute(&mut doc_mut, batch.clone())
                        .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                    rec.borrow_mut().push(batch);
                    *c.borrow_mut() += 1;
                }
                Ok(moved)
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn("recolor_all_to_cmyk", move || -> i64 {
            let mut doc_mut = d.borrow_mut();
            let locked_ids = collect_locked_object_ids(&doc_mut);
            let obj_ids: Vec<ObjectId> = doc_mut.objects().keys().copied().collect();
            let mut ops = Vec::new();
            let mut converted = 0usize;
            for oid in obj_ids {
                if locked_ids.contains(&oid) {
                    continue;
                }
                let Some(obj) = doc_mut.object(oid) else {
                    continue;
                };
                if obj.locked {
                    continue;
                }
                let mut fill = obj.fill.clone();
                let fill_hits = map_fill_to_cmyk(&mut fill);
                if fill_hits > 0 {
                    converted += fill_hits;
                    ops.push(CommandOp::SetFill {
                        ids: vec![oid],
                        fill,
                    });
                }
                if obj.outline.is_visible() {
                    let mut outline = obj.outline.clone();
                    if convert_color_to_cmyk(&mut outline.color) {
                        converted += 1;
                        ops.push(CommandOp::SetOutline {
                            ids: vec![oid],
                            outline,
                        });
                    }
                }
                match &obj.payload {
                    NodePayload::ArtisticText(t) => {
                        let mut t2 = t.clone();
                        let mut hits = 0usize;
                        for r in &mut t2.runs {
                            if convert_color_to_cmyk(&mut r.style.fill) {
                                hits += 1;
                            }
                        }
                        if hits > 0 {
                            converted += hits;
                            ops.push(CommandOp::EditText {
                                id: oid,
                                payload: NodePayload::ArtisticText(t2),
                            });
                        }
                    }
                    NodePayload::ParagraphText(t) => {
                        let mut t2 = t.clone();
                        let mut hits = 0usize;
                        for r in &mut t2.runs {
                            if convert_color_to_cmyk(&mut r.style.fill) {
                                hits += 1;
                            }
                        }
                        if hits > 0 {
                            converted += hits;
                            ops.push(CommandOp::EditText {
                                id: oid,
                                payload: NodePayload::ParagraphText(t2),
                            });
                        }
                    }
                    NodePayload::TextOnPath(top) => {
                        let mut top2 = top.clone();
                        let mut hits = 0usize;
                        for r in &mut top2.text.runs {
                            if convert_color_to_cmyk(&mut r.style.fill) {
                                hits += 1;
                            }
                        }
                        if hits > 0 {
                            converted += hits;
                            ops.push(CommandOp::EditText {
                                id: oid,
                                payload: NodePayload::TextOnPath(top2),
                            });
                        }
                    }
                    NodePayload::Table(table) => {
                        let mut tbl2 = table.clone();
                        let mut hits = 0usize;
                        for cell in &mut tbl2.cells {
                            hits += map_fill_to_cmyk(&mut cell.fill);
                            if convert_color_to_cmyk(&mut cell.text_style.fill) {
                                hits += 1;
                            }
                        }
                        if tbl2.border_style.is_visible()
                            && convert_color_to_cmyk(&mut tbl2.border_style.color)
                        {
                            hits += 1;
                        }
                        if hits > 0 {
                            converted += hits;
                            ops.push(CommandOp::EditTable {
                                id: oid,
                                table: tbl2,
                            });
                        }
                    }
                    NodePayload::Dimension(dim) => {
                        let mut dim2 = dim.clone();
                        if convert_color_to_cmyk(&mut dim2.text_style.fill) {
                            converted += 1;
                            ops.push(CommandOp::EditDimension {
                                id: oid,
                                dimension: dim2,
                            });
                        }
                    }
                    _ => {}
                }

                if let Some(lens) = &obj.lens {
                    let mut lens2 = lens.clone();
                    let lens_hits = map_lens_to_cmyk(&mut lens2);
                    if lens_hits > 0 {
                        converted += lens_hits;
                        ops.push(CommandOp::SetLens {
                            ids: vec![oid],
                            lens: Some(lens2),
                        });
                    }
                }

                if !obj.effects.is_empty() {
                    let mut changed_effects: Vec<(usize, Effect)> = Vec::new();
                    for (idx, eff) in obj.effects.iter().enumerate() {
                        let mut eff2 = eff.clone();
                        let hits = map_effect_to_cmyk(&mut eff2);
                        if hits > 0 {
                            converted += hits;
                            changed_effects.push((idx, eff2));
                        }
                    }
                    if !changed_effects.is_empty() {
                        for &(idx, _) in changed_effects.iter().rev() {
                            ops.push(CommandOp::RemoveEffect {
                                id: oid,
                                effect_index: idx,
                            });
                        }
                        for (_, eff2) in changed_effects {
                            ops.push(CommandOp::AddEffect {
                                id: oid,
                                effect: eff2,
                            });
                        }
                    }
                }
            }
            let sym_ids: Vec<coree_core::SymbolId> = doc_mut.symbols().keys().copied().collect();
            for sym_id in sym_ids {
                if let Some(mut sym) = doc_mut.symbol(sym_id).cloned() {
                    let mut sym_hits = 0usize;
                    for sub in &mut sym.objects {
                        if sub.locked {
                            continue;
                        }
                        sym_hits += map_scene_object_fields_to_cmyk(sub);
                    }
                    if sym_hits > 0 {
                        converted += sym_hits;
                        ops.push(CommandOp::UpsertSymbol { symbol: sym });
                    }
                }
            }
            if !ops.is_empty() {
                let batch = CommandOp::Batch(ops);
                if h.borrow_mut().execute(&mut doc_mut, batch.clone()).is_ok() {
                    rec.borrow_mut().push(batch);
                }
            }
            if converted > 0 {
                *c.borrow_mut() += 1;
            }
            converted as i64
        });
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn("clean_empty_layers", move || -> i64 {
            let mut doc_mut = d.borrow_mut();
            let mut to_delete = Vec::new();
            for page in doc_mut.pages() {
                let mut remaining = page.layers.len();
                for layer in &page.layers {
                    if remaining > 1 && layer.objects.is_empty() && !layer.locked {
                        to_delete.push(layer.id);
                        remaining -= 1;
                    }
                }
            }
            let ops: Vec<CommandOp> = to_delete
                .into_iter()
                .map(|layer_id| CommandOp::DeleteLayer { layer_id })
                .collect();
            let removed = ops.len() as i64;
            if !ops.is_empty() {
                let batch = CommandOp::Batch(ops);
                if h.borrow_mut().execute(&mut doc_mut, batch.clone()).is_ok() {
                    rec.borrow_mut().push(batch);
                    *c.borrow_mut() += 1;
                }
            }
            removed
        });
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
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
                let obj_ids = rhai_array_to_ids(ids);
                if obj_ids.is_empty() {
                    return Ok(());
                }
                let color = Color::cmyk(cv as f32, mv as f32, yv as f32, kv as f32);
                let op = CommandOp::SetFill {
                    ids: obj_ids,
                    fill: Fill::Solid(color),
                };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
    {
        let d = Rc::clone(doc);
        let h = Rc::clone(history);
        let c = Rc::clone(ops_count);
        let rec = Rc::clone(recorded_ops);
        engine.register_fn(
            "set_outline_mm",
            move |ids: rhai::Array, w_dyn: Dynamic| -> Result<(), Box<EvalAltResult>> {
                let w = dyn_to_f64(w_dyn, "width_mm")?;
                if !w.is_finite() || w < 0.0 {
                    return Err(
                        "set_outline_mm requires a finite non-negative outline width".into(),
                    );
                }
                let obj_ids = rhai_array_to_ids(ids);
                if obj_ids.is_empty() {
                    return Ok(());
                }
                let op = CommandOp::SetOutline {
                    ids: obj_ids,
                    outline: OutlineStyle::solid(Color::KEY_BLACK, w),
                };
                h.borrow_mut()
                    .execute(&mut d.borrow_mut(), op.clone())
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                rec.borrow_mut().push(op);
                *c.borrow_mut() += 1;
                Ok(())
            },
        );
    }
}

// =============================================================================
// Rhai Hierarchical Scene DOM (`doc` -> `pages` -> `layers` -> `shapes`)
// =============================================================================

#[derive(Clone)]
struct DomContext {
    doc: Rc<RefCell<Document>>,
    history: Rc<RefCell<CommandHistory>>,
    logs: Rc<RefCell<Vec<String>>>,
    ops_count: Rc<RefCell<usize>>,
    recorded_ops: Rc<RefCell<Vec<CommandOp>>>,
    selection: Rc<RefCell<Vec<ObjectId>>>,
}

impl DomContext {
    fn apply_op(&self, op: CommandOp) -> Result<(), Box<EvalAltResult>> {
        self.history
            .borrow_mut()
            .execute(&mut self.doc.borrow_mut(), op.clone())
            .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
        self.recorded_ops.borrow_mut().push(op);
        *self.ops_count.borrow_mut() += 1;
        Ok(())
    }
}

#[derive(Clone)]
struct DomDocument {
    ctx: DomContext,
}

#[derive(Clone)]
struct DomPage {
    ctx: DomContext,
    page_idx: usize,
}

#[derive(Clone)]
struct DomLayer {
    ctx: DomContext,
    page_idx: usize,
    layer_id: LayerId,
}

#[derive(Clone)]
struct DomShape {
    ctx: DomContext,
    object_id: ObjectId,
}

#[derive(Clone)]
struct DomStyles {
    ctx: DomContext,
}

#[derive(Clone)]
struct DomSelection {
    ctx: DomContext,
}

fn shape_type_label(payload: &NodePayload) -> &'static str {
    match payload {
        NodePayload::Path(_) => "curve",
        NodePayload::Shape(_) => "shape",
        NodePayload::BSpline(_) => "bspline",
        NodePayload::ArtisticMedia { .. } => "artistic_media",
        NodePayload::ArtisticText(_) => "artistic_text",
        NodePayload::ParagraphText(_) => "paragraph_text",
        NodePayload::TextOnPath(_) => "text_on_path",
        NodePayload::Raster(_) => "raster",
        NodePayload::Group(_) => "group",
        NodePayload::PowerClip { .. } => "powerclip",
        NodePayload::SymbolInstance { .. } => "symbol_instance",
        NodePayload::Clone { .. } => "clone",
        NodePayload::Connector(_) => "connector",
        NodePayload::Table(_) => "table",
        NodePayload::Dimension(_) => "dimension",
    }
}

fn register_hierarchical_scene_dom(
    engine: &mut Engine,
    doc: &Rc<RefCell<Document>>,
    history: &Rc<RefCell<CommandHistory>>,
    logs: &Rc<RefCell<Vec<String>>>,
    ops_count: &Rc<RefCell<usize>>,
    recorded_ops: &Rc<RefCell<Vec<CommandOp>>>,
    selection: &Rc<RefCell<Vec<ObjectId>>>,
) -> DomDocument {
    let ctx = DomContext {
        doc: Rc::clone(doc),
        history: Rc::clone(history),
        logs: Rc::clone(logs),
        ops_count: Rc::clone(ops_count),
        recorded_ops: Rc::clone(recorded_ops),
        selection: Rc::clone(selection),
    };

    engine
        .register_type_with_name::<DomDocument>("DomDocument")
        .register_type_with_name::<DomPage>("DomPage")
        .register_type_with_name::<DomLayer>("DomLayer")
        .register_type_with_name::<DomShape>("DomShape")
        .register_type_with_name::<DomStyles>("DomStyles")
        .register_type_with_name::<DomSelection>("DomSelection");

    // Top-level helpers
    {
        let root = DomDocument { ctx: ctx.clone() };
        engine.register_fn("doc", move || root.clone());
    }
    {
        let d = Rc::clone(doc);
        engine.register_fn(
            "cql",
            move |query: &str| -> Result<rhai::Array, Box<EvalAltResult>> {
                let ids = evaluate_cql_query(&d.borrow(), FindScope::WholeDocument, query)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                Ok(ids
                    .into_iter()
                    .map(|id| Dynamic::from(id.0 as i64))
                    .collect())
            },
        );
    }
    {
        let c = ctx.clone();
        engine.register_fn(
            "cql_shapes",
            move |query: &str| -> Result<rhai::Array, Box<EvalAltResult>> {
                let ids = evaluate_cql_query(&c.doc.borrow(), FindScope::WholeDocument, query)
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                Ok(ids
                    .into_iter()
                    .map(|oid| {
                        Dynamic::from(DomShape {
                            ctx: c.clone(),
                            object_id: oid,
                        })
                    })
                    .collect())
            },
        );
    }
    engine.register_fn(
        "cancel_export",
        |reason: &str| -> Result<(), Box<EvalAltResult>> {
            Err(format!("export cancelled by hook: {reason}").into())
        },
    );
    engine.register_fn(
        "abort_export",
        |reason: &str| -> Result<(), Box<EvalAltResult>> {
            Err(format!("export aborted by hook: {reason}").into())
        },
    );

    // DomDocument API
    engine.register_get("title", |d: &mut DomDocument| -> String {
        d.ctx.doc.borrow().title().to_owned()
    });
    engine.register_get("color_mode", |d: &mut DomDocument| -> String {
        format!("{:?}", d.ctx.doc.borrow().color_mode())
    });
    engine.register_get("pages", |d: &mut DomDocument| -> rhai::Array {
        let count = d.ctx.doc.borrow().pages().len();
        (0..count)
            .map(|page_idx| {
                Dynamic::from(DomPage {
                    ctx: d.ctx.clone(),
                    page_idx,
                })
            })
            .collect()
    });
    engine.register_get("active_page", |d: &mut DomDocument| -> DomPage {
        let page_idx = d.ctx.doc.borrow().active_page_index();
        DomPage {
            ctx: d.ctx.clone(),
            page_idx,
        }
    });
    engine.register_get("styles", |d: &mut DomDocument| -> DomStyles {
        DomStyles { ctx: d.ctx.clone() }
    });
    engine.register_get("selection", |d: &mut DomDocument| -> DomSelection {
        DomSelection { ctx: d.ctx.clone() }
    });
    engine.register_fn(
        "cql",
        |d: &mut DomDocument, query: &str| -> Result<rhai::Array, Box<EvalAltResult>> {
            let ids = evaluate_cql_query(&d.ctx.doc.borrow(), FindScope::WholeDocument, query)
                .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
            Ok(ids
                .into_iter()
                .map(|oid| {
                    Dynamic::from(DomShape {
                        ctx: d.ctx.clone(),
                        object_id: oid,
                    })
                })
                .collect())
        },
    );
    engine.register_fn("undo", |d: &mut DomDocument| -> bool {
        let mut doc_mut = d.ctx.doc.borrow_mut();
        if d.ctx.history.borrow_mut().undo(&mut doc_mut).is_ok() {
            d.ctx.recorded_ops.borrow_mut().pop();
            let mut cnt = d.ctx.ops_count.borrow_mut();
            *cnt = cnt.saturating_sub(1);
            d.ctx
                .logs
                .borrow_mut()
                .push("dom.undo() reverted".to_owned());
            true
        } else {
            false
        }
    });
    engine.register_fn(
        "export",
        |d: &mut DomDocument, format: &str| -> Result<String, Box<EvalAltResult>> {
            let doc_ref = d.ctx.doc.borrow();
            match format.trim().to_ascii_lowercase().as_str() {
                "svg" => {
                    let svg = coree_io::svg::export_svg_to_string(&doc_ref, 0)
                        .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                    Ok(svg)
                }
                "dxf" => {
                    let plan = crate::laser_plugin::plan_laser_toolpath(
                        &doc_ref,
                        0,
                        None,
                        &crate::laser_plugin::LaserJobConfig::default(),
                    )
                    .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                    Ok(crate::laser_plugin::export_laser_dxf_r12(&plan))
                }
                "gcode" => {
                    let cfg = crate::laser_plugin::LaserJobConfig::default();
                    let plan = crate::laser_plugin::plan_laser_toolpath(&doc_ref, 0, None, &cfg)
                        .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
                    Ok(crate::laser_plugin::export_laser_gcode(&plan, &cfg))
                }
                other => Err(format!("unsupported dom.export format '{other}'").into()),
            }
        },
    );

    // DomStyles API
    engine.register_get("count", |s: &mut DomStyles| -> i64 {
        s.ctx.doc.borrow().palettes().len() as i64
    });
    engine.register_fn("palette_names", |s: &mut DomStyles| -> rhai::Array {
        s.ctx
            .doc
            .borrow()
            .palettes()
            .iter()
            .map(|p| Dynamic::from(p.name.clone()))
            .collect()
    });

    // DomSelection API
    engine.register_get("count", |sel: &mut DomSelection| -> i64 {
        sel.ctx.selection.borrow().len() as i64
    });
    engine.register_get("ids", |sel: &mut DomSelection| -> rhai::Array {
        sel.ctx
            .selection
            .borrow()
            .iter()
            .map(|id| Dynamic::from(id.0 as i64))
            .collect()
    });
    engine.register_get("shapes", |sel: &mut DomSelection| -> rhai::Array {
        sel.ctx
            .selection
            .borrow()
            .iter()
            .map(|&oid| {
                Dynamic::from(DomShape {
                    ctx: sel.ctx.clone(),
                    object_id: oid,
                })
            })
            .collect()
    });
    engine.register_fn("clear", |sel: &mut DomSelection| {
        sel.ctx.selection.borrow_mut().clear();
    });
    engine.register_fn("select", |sel: &mut DomSelection, ids: rhai::Array| {
        *sel.ctx.selection.borrow_mut() = rhai_array_to_ids(ids);
    });

    // DomPage API
    engine.register_get("index", |p: &mut DomPage| -> i64 { p.page_idx as i64 });
    engine.register_get("name", |p: &mut DomPage| -> String {
        p.ctx
            .doc
            .borrow()
            .page(p.page_idx)
            .map(|pg| pg.name.clone())
            .unwrap_or_default()
    });
    engine.register_get("width_mm", |p: &mut DomPage| -> f64 {
        p.ctx
            .doc
            .borrow()
            .page(p.page_idx)
            .map_or(0.0, |pg| pg.width_mm)
    });
    engine.register_get("height_mm", |p: &mut DomPage| -> f64 {
        p.ctx
            .doc
            .borrow()
            .page(p.page_idx)
            .map_or(0.0, |pg| pg.height_mm)
    });
    engine.register_get("bleed_mm", |p: &mut DomPage| -> f64 {
        p.ctx
            .doc
            .borrow()
            .page(p.page_idx)
            .map_or(0.0, |pg| pg.bleed_mm)
    });
    engine.register_get("layers", |p: &mut DomPage| -> rhai::Array {
        p.ctx
            .doc
            .borrow()
            .page(p.page_idx)
            .map(|pg| {
                pg.layers
                    .iter()
                    .map(|l| {
                        Dynamic::from(DomLayer {
                            ctx: p.ctx.clone(),
                            page_idx: p.page_idx,
                            layer_id: l.id,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    let page_shapes_fn = |p: &mut DomPage| -> rhai::Array {
        p.ctx
            .doc
            .borrow()
            .page(p.page_idx)
            .map(|pg| {
                pg.layers
                    .iter()
                    .flat_map(|l| l.objects.iter())
                    .map(|&oid| {
                        Dynamic::from(DomShape {
                            ctx: p.ctx.clone(),
                            object_id: oid,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    engine.register_get("shapes", page_shapes_fn);
    engine.register_get("objects", page_shapes_fn);

    // DomLayer API
    engine.register_get("id", |l: &mut DomLayer| -> i64 { l.layer_id.0 as i64 });
    engine.register_get("name", |l: &mut DomLayer| -> String {
        l.ctx
            .doc
            .borrow()
            .page(l.page_idx)
            .and_then(|pg| pg.layers.iter().find(|ly| ly.id == l.layer_id))
            .map(|ly| ly.name.clone())
            .unwrap_or_default()
    });
    engine.register_get("visible", |l: &mut DomLayer| -> bool {
        l.ctx
            .doc
            .borrow()
            .page(l.page_idx)
            .and_then(|pg| pg.layers.iter().find(|ly| ly.id == l.layer_id))
            .is_some_and(|ly| ly.visible)
    });
    engine.register_set(
        "visible",
        |l: &mut DomLayer, v: bool| -> Result<(), Box<EvalAltResult>> {
            l.ctx.apply_op(CommandOp::SetLayerFlags {
                layer_id: l.layer_id,
                visible: Some(v),
                locked: None,
                printable: None,
                is_cut_layer: None,
            })
        },
    );
    engine.register_get("locked", |l: &mut DomLayer| -> bool {
        l.ctx
            .doc
            .borrow()
            .page(l.page_idx)
            .and_then(|pg| pg.layers.iter().find(|ly| ly.id == l.layer_id))
            .is_some_and(|ly| ly.locked)
    });
    engine.register_set(
        "locked",
        |l: &mut DomLayer, v: bool| -> Result<(), Box<EvalAltResult>> {
            l.ctx.apply_op(CommandOp::SetLayerFlags {
                layer_id: l.layer_id,
                visible: None,
                locked: Some(v),
                printable: None,
                is_cut_layer: None,
            })
        },
    );
    engine.register_get("printable", |l: &mut DomLayer| -> bool {
        l.ctx
            .doc
            .borrow()
            .page(l.page_idx)
            .and_then(|pg| pg.layers.iter().find(|ly| ly.id == l.layer_id))
            .is_some_and(|ly| ly.printable)
    });
    engine.register_get("is_cut_layer", |l: &mut DomLayer| -> bool {
        l.ctx
            .doc
            .borrow()
            .page(l.page_idx)
            .and_then(|pg| pg.layers.iter().find(|ly| ly.id == l.layer_id))
            .is_some_and(|ly| ly.is_cut_layer)
    });
    let layer_shapes_fn = |l: &mut DomLayer| -> rhai::Array {
        l.ctx
            .doc
            .borrow()
            .page(l.page_idx)
            .and_then(|pg| pg.layers.iter().find(|ly| ly.id == l.layer_id))
            .map(|ly| {
                ly.objects
                    .iter()
                    .map(|&oid| {
                        Dynamic::from(DomShape {
                            ctx: l.ctx.clone(),
                            object_id: oid,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    engine.register_get("shapes", layer_shapes_fn);
    engine.register_get("objects", layer_shapes_fn);

    // DomShape API
    engine.register_get("id", |s: &mut DomShape| -> i64 { s.object_id.0 as i64 });
    engine.register_get("name", |s: &mut DomShape| -> String {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .map(|o| o.name.clone())
            .unwrap_or_default()
    });
    engine.register_set(
        "name",
        |s: &mut DomShape, name: String| -> Result<(), Box<EvalAltResult>> {
            s.ctx.apply_op(CommandOp::RenameObject {
                id: s.object_id,
                new_name: name,
            })
        },
    );
    let shape_type_fn = |s: &mut DomShape| -> String {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .map(|o| shape_type_label(&o.payload).to_owned())
            .unwrap_or_default()
    };
    engine.register_get("type", shape_type_fn);
    engine.register_get("kind", shape_type_fn);
    engine.register_get("visible", |s: &mut DomShape| -> bool {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .is_some_and(|o| o.visible)
    });
    engine.register_set(
        "visible",
        |s: &mut DomShape, visible: bool| -> Result<(), Box<EvalAltResult>> {
            s.ctx.apply_op(CommandOp::SetObjectVisibility {
                ids: vec![s.object_id],
                visible,
            })
        },
    );
    engine.register_get("locked", |s: &mut DomShape| -> bool {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .is_some_and(|o| o.locked)
    });
    engine.register_set(
        "locked",
        |s: &mut DomShape, locked: bool| -> Result<(), Box<EvalAltResult>> {
            s.ctx.apply_op(CommandOp::SetObjectLocked {
                ids: vec![s.object_id],
                locked,
            })
        },
    );
    engine.register_get("x", |s: &mut DomShape| -> f64 {
        s.ctx
            .doc
            .borrow()
            .world_object_bounds(s.object_id)
            .map_or(0.0, |b| b.min_x)
    });
    engine.register_get("y", |s: &mut DomShape| -> f64 {
        s.ctx
            .doc
            .borrow()
            .world_object_bounds(s.object_id)
            .map_or(0.0, |b| b.min_y)
    });
    engine.register_get("width", |s: &mut DomShape| -> f64 {
        s.ctx
            .doc
            .borrow()
            .world_object_bounds(s.object_id)
            .map_or(0.0, |b| b.width())
    });
    engine.register_get("height", |s: &mut DomShape| -> f64 {
        s.ctx
            .doc
            .borrow()
            .world_object_bounds(s.object_id)
            .map_or(0.0, |b| b.height())
    });
    engine.register_get("fill_hex", |s: &mut DomShape| -> String {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .and_then(|o| match &o.fill {
                Fill::Solid(c) => Some(c.to_hex_rgba()),
                _ => None,
            })
            .unwrap_or_default()
    });
    engine.register_set(
        "fill_hex",
        |s: &mut DomShape, hex: String| -> Result<(), Box<EvalAltResult>> {
            let color = Color::from_hex(&hex)
                .map_err(|e| -> Box<EvalAltResult> { e.to_string().into() })?;
            s.ctx.apply_op(CommandOp::SetFill {
                ids: vec![s.object_id],
                fill: Fill::Solid(color),
            })
        },
    );
    engine.register_get("outline_mm", |s: &mut DomShape| -> f64 {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .map_or(0.0, |o| o.outline.effective_width_mm())
    });
    engine.register_set(
        "outline_mm",
        |s: &mut DomShape, w: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let width = dyn_to_f64(w, "outline_mm")?;
            if !width.is_finite() || width < 0.0 {
                return Err("outline_mm must be finite and >= 0".into());
            }
            s.ctx.apply_op(CommandOp::SetOutline {
                ids: vec![s.object_id],
                outline: OutlineStyle::solid(Color::KEY_BLACK, width),
            })
        },
    );
    engine.register_get("text", |s: &mut DomShape| -> String {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .and_then(|o| coree_core::extract_searchable_text(&o.payload))
            .unwrap_or_default()
    });
    engine.register_set(
        "text",
        |s: &mut DomShape, new_text: String| -> Result<(), Box<EvalAltResult>> {
            let maybe_payload =
                s.ctx
                    .doc
                    .borrow()
                    .object(s.object_id)
                    .and_then(|o| match &o.payload {
                        NodePayload::ArtisticText(at) => {
                            let style =
                                at.runs.first().map(|r| r.style.clone()).unwrap_or_default();
                            Some(NodePayload::ArtisticText(ArtisticText::from_plain(
                                new_text.clone(),
                                style,
                            )))
                        }
                        NodePayload::ParagraphText(pt) => {
                            let mut next = pt.clone();
                            if let Some(first) = next.runs.first_mut() {
                                first.text = new_text.clone();
                                next.runs.truncate(1);
                            }
                            Some(NodePayload::ParagraphText(next))
                        }
                        _ => None,
                    });
            if let Some(payload) = maybe_payload {
                s.ctx.apply_op(CommandOp::EditText {
                    id: s.object_id,
                    payload,
                })
            } else {
                Err("object is not editable text".into())
            }
        },
    );
    engine.register_get("children", |s: &mut DomShape| -> rhai::Array {
        s.ctx
            .doc
            .borrow()
            .object(s.object_id)
            .map(|o| match &o.payload {
                NodePayload::Group(kids) | NodePayload::PowerClip { contents: kids, .. } => kids
                    .iter()
                    .map(|&cid| {
                        Dynamic::from(DomShape {
                            ctx: s.ctx.clone(),
                            object_id: cid,
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default()
    });
    engine.register_fn(
        "move_by",
        |s: &mut DomShape, dx: Dynamic, dy: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let dxv = dyn_to_f64(dx, "dx")?;
            let dyv = dyn_to_f64(dy, "dy")?;
            if !dxv.is_finite() || !dyv.is_finite() {
                return Err("move_by requires finite deltas".into());
            }
            s.ctx.apply_op(CommandOp::TransformObjects {
                ids: vec![s.object_id],
                affine: Affine2D::translate(dxv, dyv),
            })
        },
    );
    engine.register_fn(
        "set_cmyk_fill",
        |s: &mut DomShape,
         c: Dynamic,
         m: Dynamic,
         y: Dynamic,
         k: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let cv = dyn_to_f64(c, "c")?;
            let mv = dyn_to_f64(m, "m")?;
            let yv = dyn_to_f64(y, "y")?;
            let kv = dyn_to_f64(k, "k")?;
            s.ctx.apply_op(CommandOp::SetFill {
                ids: vec![s.object_id],
                fill: Fill::Solid(Color::cmyk(cv as f32, mv as f32, yv as f32, kv as f32)),
            })
        },
    );
    engine.register_fn(
        "convert_to_curves",
        |s: &mut DomShape| -> Result<(), Box<EvalAltResult>> {
            s.ctx
                .apply_op(CommandOp::ConvertToCurves { id: s.object_id })
        },
    );
    engine.register_fn(
        "delete",
        |s: &mut DomShape| -> Result<(), Box<EvalAltResult>> {
            s.ctx.apply_op(CommandOp::DeleteObjects {
                ids: vec![s.object_id],
            })
        },
    );

    DomDocument { ctx }
}

fn collect_locked_object_ids(doc: &Document) -> HashSet<ObjectId> {
    fn mark_subtree(
        doc: &Document,
        id: ObjectId,
        ancestor_locked: bool,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
        locked: &mut HashSet<ObjectId>,
    ) {
        if depth > 64 || !visiting.insert(id) {
            return;
        }
        if let Some(obj) = doc.object(id) {
            let is_locked = ancestor_locked || obj.locked;
            if is_locked {
                locked.insert(id);
            }
            match &obj.payload {
                NodePayload::Group(children) => {
                    for &cid in children {
                        mark_subtree(doc, cid, is_locked, depth + 1, visiting, locked);
                    }
                }
                NodePayload::PowerClip { contents, .. } => {
                    for &cid in contents {
                        mark_subtree(doc, cid, is_locked, depth + 1, visiting, locked);
                    }
                }
                _ => {}
            }
        }
        visiting.remove(&id);
    }

    let mut locked = HashSet::new();
    let mut visiting = HashSet::new();
    for page in doc.pages() {
        for layer in &page.layers {
            for &oid in &layer.objects {
                mark_subtree(doc, oid, layer.locked, 0, &mut visiting, &mut locked);
            }
        }
    }
    for (&oid, obj) in doc.objects() {
        if obj.locked {
            mark_subtree(doc, oid, true, 0, &mut visiting, &mut locked);
        }
    }
    locked
}

fn map_fill_to_cmyk(fill: &mut Fill) -> usize {
    let mut hits = 0usize;
    match fill {
        Fill::Solid(c) => {
            if convert_color_to_cmyk(c) {
                hits += 1;
            }
        }
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => {
            for stop in stops {
                if convert_color_to_cmyk(&mut stop.color) {
                    hits += 1;
                }
            }
        }
        Fill::TwoColorPattern { fg, bg, .. } | Fill::TextureFill { fg, bg, .. } => {
            if convert_color_to_cmyk(fg) {
                hits += 1;
            }
            if convert_color_to_cmyk(bg) {
                hits += 1;
            }
        }
        Fill::MeshFill { nodes, .. } => {
            for node in nodes {
                if convert_color_to_cmyk(&mut node.color) {
                    hits += 1;
                }
            }
        }
        Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {}
    }
    hits
}

fn map_lens_to_cmyk(lens: &mut coree_core::LensConfig) -> usize {
    let mut hits = 0usize;
    match &mut lens.kind {
        LensKind::TintedGrayscale { tint } => {
            if convert_color_to_cmyk(tint) {
                hits += 1;
            }
        }
        LensKind::ColorAdd { color, .. }
        | LensKind::ColorLimit { color, .. }
        | LensKind::Transparency { color, .. } => {
            if convert_color_to_cmyk(color) {
                hits += 1;
            }
        }
        LensKind::CustomColorMap {
            from_color,
            to_color,
            ..
        } => {
            if convert_color_to_cmyk(from_color) {
                hits += 1;
            }
            if convert_color_to_cmyk(to_color) {
                hits += 1;
            }
        }
        LensKind::Wireframe {
            override_outline,
            override_fill,
        } => {
            if let Some(c) = override_outline {
                if convert_color_to_cmyk(c) {
                    hits += 1;
                }
            }
            if let Some(c) = override_fill {
                if convert_color_to_cmyk(c) {
                    hits += 1;
                }
            }
        }
        _ => {}
    }
    hits
}

fn map_effect_to_cmyk(eff: &mut Effect) -> usize {
    let mut hits = 0usize;
    match eff {
        Effect::DropShadow { color, .. }
        | Effect::InnerShadow { color, .. }
        | Effect::BlockShadow { color, .. } => {
            if convert_color_to_cmyk(color) {
                hits += 1;
            }
        }
        Effect::Contour {
            fill_color,
            outline_color,
            ..
        } => {
            if convert_color_to_cmyk(fill_color) {
                hits += 1;
            }
            if convert_color_to_cmyk(outline_color) {
                hits += 1;
            }
        }
        Effect::Bevel {
            highlight_color,
            shadow_color,
            ..
        } => {
            if convert_color_to_cmyk(highlight_color) {
                hits += 1;
            }
            if convert_color_to_cmyk(shadow_color) {
                hits += 1;
            }
        }
        Effect::Extrude25D { shade_color, .. } | Effect::Extrude3D { shade_color, .. } => {
            if convert_color_to_cmyk(shade_color) {
                hits += 1;
            }
        }
        _ => {}
    }
    hits
}

fn map_scene_object_fields_to_cmyk(obj: &mut coree_core::SceneObject) -> usize {
    let mut converted = map_fill_to_cmyk(&mut obj.fill);
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
        NodePayload::Table(table) => {
            for cell in &mut table.cells {
                converted += map_fill_to_cmyk(&mut cell.fill);
                if convert_color_to_cmyk(&mut cell.text_style.fill) {
                    converted += 1;
                }
            }
            if table.border_style.is_visible()
                && convert_color_to_cmyk(&mut table.border_style.color)
            {
                converted += 1;
            }
        }
        NodePayload::Dimension(dim) => {
            if convert_color_to_cmyk(&mut dim.text_style.fill) {
                converted += 1;
            }
        }
        _ => {}
    }
    if let Some(lens) = &mut obj.lens {
        converted += map_lens_to_cmyk(lens);
    }
    for eff in &mut obj.effects {
        converted += map_effect_to_cmyk(eff);
    }
    converted
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

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, MasterLayerScope, NodePayload, ParametricShape, SceneObject};

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
        let page_id = doc.pages()[0].id;
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
            doc.objects()[&a].fill,
            Fill::Solid(Color::Cmyk { .. })
        ));
        assert!(matches!(
            doc.objects()[&b].fill,
            Fill::Solid(Color::Cmyk { .. })
        ));

        // 4. clean-empty-layers
        assert_eq!(doc.pages()[0].layers.len(), 2);
        run_macro(
            &mut doc,
            find_builtin_macro("clean-empty-layers").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        assert_eq!(doc.pages()[0].layers.len(), 1);

        // 5. auto-cut-contour
        run_macro(
            &mut doc,
            find_builtin_macro("auto-cut-contour").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        assert!(doc.pages()[0]
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

    #[test]
    fn plan_macro_ops_replays_all_five_builtins_and_skips_empty_noop_commands() {
        let cfg = MacroSandboxConfig::default();

        // Empty page: calling translate_objects(all_object_ids(), ...) or log(...) emits ZERO ops
        let empty_doc = Document::new_a4("Empty");
        let (empty_exec, empty_ops) = plan_macro_ops(
            &empty_doc,
            "translate_objects(all_object_ids(), 10.0, 20.0); log(\"noop\");",
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        assert_eq!(empty_exec.commands_executed, 0);
        assert!(empty_ops.is_empty());

        // Non-empty doc: run all 5 builtin macros + multi-step script via plan_macro_ops + replay_log
        let mut init_doc = Document::new_a4("ReplayMacros");
        let page_id = init_doc.pages()[0].id;
        init_doc
            .add_object(
                rect_obj(5.0, 5.0, 20.0, 10.0, Color::rgb(1.0, 0.0, 0.0)),
                None,
            )
            .unwrap();
        init_doc
            .add_object(
                rect_obj(40.0, 5.0, 30.0, 15.0, Color::rgb(0.0, 0.5, 1.0)),
                None,
            )
            .unwrap();
        init_doc
            .add_layer(page_id, "Unused Empty", false, MasterLayerScope::None)
            .unwrap();

        let mut direct_doc = init_doc.clone();
        let mut ops_doc = init_doc.clone();
        let mut hist = CommandHistory::default();

        let resize_vars = BTreeMap::from([
            ("target_w_mm".to_owned(), MacroValue::Float(40.0)),
            ("target_h_mm".to_owned(), MacroValue::Float(25.0)),
        ]);
        let grid_vars = BTreeMap::from([
            ("cols".to_owned(), MacroValue::Int(2)),
            ("gap_x_mm".to_owned(), MacroValue::Float(10.0)),
            ("start_x_mm".to_owned(), MacroValue::Float(15.0)),
            ("start_y_mm".to_owned(), MacroValue::Float(20.0)),
        ]);
        let empty_vars = BTreeMap::new();
        let steps: Vec<(&str, &BTreeMap<String, MacroValue>)> = vec![
            (find_builtin_macro("batch-resize").unwrap().script, &resize_vars),
            (find_builtin_macro("align-grid").unwrap().script, &grid_vars),
            (find_builtin_macro("recolor-cmyk").unwrap().script, &empty_vars),
            (
                find_builtin_macro("clean-empty-layers").unwrap().script,
                &empty_vars,
            ),
            (
                find_builtin_macro("auto-cut-contour").unwrap().script,
                &empty_vars,
            ),
            (
                "let cid = generate_cut_contour(non_cut_object_ids(), 1.5, \"CutContour\"); set_outline_mm([cid], 0.5);",
                &empty_vars,
            ),
        ];

        for (script, vars) in steps {
            run_macro(&mut direct_doc, script, vars, &cfg).unwrap();
            let (_exec, ops) = plan_macro_ops(&ops_doc, script, vars, &cfg).unwrap();
            assert!(!ops.is_empty());
            for op in &ops {
                assert!(
                    !matches!(op, CommandOp::TransformObjects { ids, .. } if ids.is_empty()),
                    "must never emit empty TransformObjects"
                );
            }
            hist.execute(&mut ops_doc, CommandOp::Batch(ops)).unwrap();
        }

        assert_eq!(ops_doc.objects(), direct_doc.objects());
        assert_eq!(ops_doc.pages(), direct_doc.pages());

        let mut replayed = init_doc;
        CommandHistory::replay_log(&mut replayed, &hist.command_log()).unwrap();
        assert_eq!(replayed.objects(), ops_doc.objects());
        assert_eq!(replayed.pages(), ops_doc.pages());
    }

    #[test]
    fn recolor_cmyk_converts_table_gradients_and_macros_respect_locked_layers() {
        use coree_core::{GradientStop, Point2D, TableCell, VectorTable};

        let mut doc = Document::new_a4("TableAndLockedLayer");
        let page_id = doc.pages()[0].id;
        let unlocked_layer = doc.pages()[0].layers[0].id;
        let locked_layer = doc
            .add_layer(page_id, "Locked Layer", false, MasterLayerScope::None)
            .unwrap();

        // Unlocked layer: VectorTable with LinearGradient & RadialGradient cells + two rectangles
        let mut tbl = VectorTable::new(1, 2, 30.0, 12.0);
        tbl.cells[0].fill = Fill::LinearGradient {
            start: Point2D::ZERO,
            end: Point2D::new(30.0, 12.0),
            stops: vec![
                GradientStop::new(0.0, Color::rgb(1.0, 0.0, 0.0)),
                GradientStop::new(1.0, Color::rgb(0.0, 0.0, 1.0)),
            ],
        };
        tbl.cells[1].fill = Fill::RadialGradient {
            center: Point2D::new(15.0, 6.0),
            radius: 10.0,
            focal: Point2D::new(15.0, 6.0),
            stops: vec![
                GradientStop::new(0.0, Color::hsl(120.0, 1.0, 0.5)),
                GradientStop::new(1.0, Color::rgb(0.2, 0.8, 0.4)),
            ],
        };
        let _ = TableCell::new(0, 0);
        let tbl_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "GradTable", NodePayload::Table(tbl)),
                Some(unlocked_layer),
            )
            .unwrap();
        let r1 = doc
            .add_object(
                rect_obj(10.0, 10.0, 20.0, 10.0, Color::rgb(1.0, 0.2, 0.3)),
                Some(unlocked_layer),
            )
            .unwrap();
        let r2 = doc
            .add_object(
                rect_obj(60.0, 10.0, 20.0, 10.0, Color::rgb(0.1, 0.7, 0.2)),
                Some(unlocked_layer),
            )
            .unwrap();

        // Locked layer: top-level unlocked-flag rect + nested Group child (both must be skipped!)
        let locked_top = doc
            .add_object(
                rect_obj(100.0, 100.0, 25.0, 15.0, Color::rgb(0.3, 0.4, 0.5)),
                Some(locked_layer),
            )
            .unwrap();
        let locked_nested = doc
            .add_object(
                rect_obj(140.0, 100.0, 18.0, 12.0, Color::rgb(0.9, 0.1, 0.2)),
                Some(locked_layer),
            )
            .unwrap();
        let _locked_grp = doc.group_objects(&[locked_nested], "LockedGroup").unwrap();
        doc.set_layer_flags(locked_layer, None, Some(true), None, None)
            .unwrap();

        let cfg = MacroSandboxConfig::default();

        // 1. recolor-cmyk converts Table cell LinearGradient/RadialGradient to CMYK & skips locked layer
        run_macro(
            &mut doc,
            find_builtin_macro("recolor-cmyk").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();

        match &doc.object(tbl_id).unwrap().payload {
            NodePayload::Table(t) => {
                for (cell_idx, cell) in t.cells.iter().enumerate().take(2) {
                    match &cell.fill {
                        Fill::LinearGradient { stops, .. } | Fill::RadialGradient { stops, .. } => {
                            assert!(
                                stops.iter().all(|s| matches!(s.color, Color::Cmyk { .. })),
                                "table cell {cell_idx} gradient stops must all convert to CMYK"
                            );
                        }
                        other => panic!("unexpected table cell fill: {other:?}"),
                    }
                }
            }
            other => panic!("expected Table payload, got {other:?}"),
        }
        assert!(
            matches!(
                doc.object(locked_top).unwrap().fill,
                Fill::Solid(Color::Rgba { .. })
            ),
            "top-level object on locked layer must not be recolored"
        );
        assert!(
            matches!(
                doc.object(locked_nested).unwrap().fill,
                Fill::Solid(Color::Rgba { .. })
            ),
            "nested Group object on locked layer must not be recolored"
        );

        // 2. resize_objects skips objects on locked layer
        let resize_script = format!(
            "resize_objects([{}, {}, {}], 50.0, 40.0);",
            r1.0, locked_top.0, locked_nested.0
        );
        run_macro(&mut doc, &resize_script, &BTreeMap::new(), &cfg).unwrap();
        assert!((doc.object_bounds(r1).unwrap().width() - 50.0).abs() < 1e-4);
        assert!((doc.object_bounds(r1).unwrap().height() - 40.0).abs() < 1e-4);
        assert!((doc.object_bounds(locked_top).unwrap().width() - 25.0).abs() < 1e-4);
        assert!((doc.object_bounds(locked_nested).unwrap().width() - 18.0).abs() < 1e-4);

        // 3. align_grid skips objects on locked layer (and doesn't waste a grid slot on locked_top)
        let locked_before = doc.object_bounds(locked_top).unwrap();
        let align_script = format!(
            "align_grid([{}, {}, {}], 2, 10.0, 10.0, 5.0, 5.0);",
            r1.0, locked_top.0, r2.0
        );
        run_macro(&mut doc, &align_script, &BTreeMap::new(), &cfg).unwrap();
        assert!((doc.object_bounds(r1).unwrap().min_x - 5.0).abs() < 1e-4);
        assert!((doc.object_bounds(r2).unwrap().min_x - 65.0).abs() < 1e-4);
        assert_eq!(doc.object_bounds(locked_top).unwrap(), locked_before);
    }

    #[test]
    fn recolor_cmyk_converts_table_text_border_and_dimension_text_losslessly() {
        use coree_core::{DimensionKind, DimensionObject, OutlineStyle, Point2D, VectorTable};

        let mut doc = Document::new_a4("TableDimCmyk");
        let layer = doc.pages()[0].layers[0].id;

        let mut tbl = VectorTable::new(1, 2, 25.0, 10.0);
        tbl.cells[0].text = "Item".into();
        tbl.cells[0].text_style.fill = Color::rgb(0.9, 0.1, 0.1);
        tbl.cells[1].text = "Spot".into();
        tbl.cells[1].text_style.fill = Color::cut_contour_spot();
        tbl.border_style = OutlineStyle::solid(Color::rgb(1.0, 0.0, 0.0), 0.4);
        let tbl_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "StyledTbl", NodePayload::Table(tbl)),
                Some(layer),
            )
            .unwrap();

        let mut dim = DimensionObject::linear(
            DimensionKind::Horizontal,
            Point2D::ZERO,
            Point2D::new(40.0, 0.0),
            6.0,
        );
        dim.text_style.fill = Color::rgb(1.0, 0.0, 0.0);
        let dim_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "StyledDim", NodePayload::Dimension(dim)),
                Some(layer),
            )
            .unwrap();

        let init_doc = doc.clone();
        let cfg = MacroSandboxConfig::default();
        let (_exec, ops) = plan_macro_ops(
            &doc,
            find_builtin_macro("recolor-cmyk").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        let mut hist = CommandHistory::default();
        hist.execute(&mut doc, CommandOp::Batch(ops)).unwrap();

        if let NodePayload::Table(ref t) = doc.object(tbl_id).unwrap().payload {
            assert!(matches!(t.cells[0].text_style.fill, Color::Cmyk { .. }));
            assert!(matches!(t.cells[1].text_style.fill, Color::Spot { .. }));
            assert!(matches!(t.border_style.color, Color::Cmyk { .. }));
        } else {
            panic!("expected Table");
        }
        if let NodePayload::Dimension(ref d) = doc.object(dim_id).unwrap().payload {
            assert!(matches!(d.text_style.fill, Color::Cmyk { .. }));
        } else {
            panic!("expected Dimension");
        }

        // Lossless undo / redo / replay_log
        let post_doc = doc.clone();
        hist.undo(&mut doc).unwrap();
        assert_eq!(doc.objects(), init_doc.objects());
        hist.redo(&mut doc).unwrap();
        assert_eq!(doc.objects(), post_doc.objects());
        let mut replayed = init_doc;
        CommandHistory::replay_log(&mut replayed, &hist.command_log()).unwrap();
        assert_eq!(replayed.objects(), post_doc.objects());
    }

    #[test]
    fn rhai_hierarchical_scene_dom_traverses_and_mutates() {
        let mut doc = Document::new_a4("DOM Test");
        let id = doc
            .add_object(
                rect_obj(10.0, 20.0, 30.0, 40.0, Color::rgb(1.0, 0.0, 0.0)),
                None,
            )
            .unwrap();
        let script = r#"
            let p = doc.pages[0];
            let l = p.layers[0];
            let count = l.shapes.len();
            let s = p.shapes[0];
            s.name = "RenamedByDom";
            s.move_by(5.0, 10.0);
            s.set_cmyk_fill(0.0, 1.0, 1.0, 0.0);
            let hits = doc.cql("@name == 'RenamedByDom'");
            log("count=" + count + ",hits=" + hits.len());
        "#;
        let res = run_macro(
            &mut doc,
            script,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
        .unwrap();
        assert!(res.logs.iter().any(|l| l == "count=1,hits=1"));
        assert_eq!(doc.object(id).unwrap().name, "RenamedByDom");
        let bb = doc.object_bounds(id).unwrap();
        assert!((bb.min_x - 15.0).abs() < 1e-4);
        assert!((bb.min_y - 30.0).abs() < 1e-4);
    }

    #[test]
    fn on_before_export_hook_can_inspect_modify_and_abort() {
        let mut doc = Document::new_a4("Hook Test");
        doc.add_object(
            rect_obj(10.0, 10.0, 20.0, 20.0, Color::rgb(0.2, 0.4, 0.8)),
            None,
        )
        .unwrap();

        let (res, ops) = run_macro_event_hook(
            &mut doc,
            MacroEventHook::OnBeforeExport,
            r#"
                if hook_event == "on_before_export" {
                    recolor_all_to_cmyk();
                    log("hooked:" + doc.title);
                }
            "#,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
        .unwrap();
        assert!(!ops.is_empty());
        assert!(res.logs.iter().any(|l| l == "hooked:Hook Test"));

        // Abort export hook leaves document untouched
        let err = run_macro_event_hook(
            &mut doc,
            MacroEventHook::OnBeforeExport,
            r#"cancel_export("missing bleed");"#,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("missing bleed"));
    }

    #[test]
    fn recolor_cmyk_converts_effects_and_lens_losslessly() {
        use coree_core::{FeatherDirection, LensConfig};

        let mut doc = Document::new_a4("EffectLensCmyk");
        let mut obj =
            rect_obj(10.0, 10.0, 25.0, 25.0, Color::CYAN).with_outline(OutlineStyle::none());
        obj.lens = Some(LensConfig::new(LensKind::TintedGrayscale {
            tint: Color::rgb(0.8, 0.2, 0.1),
        }));
        obj.effects.push(Effect::DropShadow {
            dx_mm: 2.0,
            dy_mm: 2.0,
            blur_mm: 3.0,
            opacity: 0.7,
            color: Color::rgb(0.1, 0.2, 0.9),
            feather_direction: FeatherDirection::Outside,
        });
        let id = doc.add_object(obj, None).unwrap();

        run_macro(
            &mut doc,
            find_builtin_macro("recolor-cmyk").unwrap().script,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
        .unwrap();

        let updated = doc.object(id).unwrap();
        if let Some(lens) = &updated.lens {
            match &lens.kind {
                LensKind::TintedGrayscale { tint } => {
                    assert!(matches!(tint, Color::Cmyk { .. }));
                }
                other => panic!("unexpected lens {other:?}"),
            }
        } else {
            panic!("expected lens");
        }
        match updated.effects.last() {
            Some(Effect::DropShadow { color, .. }) => {
                assert!(matches!(color, Color::Cmyk { .. }));
            }
            other => panic!("expected DropShadow, got {other:?}"),
        }
    }

    #[test]
    fn dom_undo_reverts_document_and_keeps_plan_macro_ops_in_sync() {
        let mut doc = Document::new_a4("UndoDomTest");
        let id = doc
            .add_object(
                rect_obj(
                    10.0,
                    10.0,
                    20.0,
                    20.0,
                    Color::from_hex("#e61a1aff").unwrap(),
                ),
                None,
            )
            .unwrap();
        let script = r##"
            let s = doc.pages[0].layers[0].shapes[0];
            let before = s.fill_hex;
            s.fill_hex = "#112233";
            let mid = doc.pages[0].layers[0].shapes[0].fill_hex;
            let ok = doc.undo();
            let after = doc.pages[0].layers[0].shapes[0].fill_hex;
            log(before + "|" + mid + "|" + ok + "|" + after);
        "##;
        let (exec, ops) = plan_macro_ops(
            &doc,
            script,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
        .unwrap();
        assert!(
            ops.is_empty(),
            "plan_macro_ops must emit 0 ops after doc.undo()"
        );
        assert_eq!(exec.commands_executed, 0);
        assert!(
            exec.logs
                .iter()
                .any(|l| l == "#e61a1aff|#112233ff|true|#e61a1aff"),
            "unexpected logs: {:?}",
            exec.logs
        );

        let mut live_doc = doc.clone();
        run_macro(
            &mut live_doc,
            script,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
        .unwrap();
        assert_eq!(
            live_doc.object(id).unwrap().fill,
            doc.object(id).unwrap().fill
        );
    }

    #[test]
    fn recolor_cmyk_converts_symbol_defs_and_inner_bevel_extrude3d_effects() {
        use coree_core::{BevelStyle, ColorMode, ExtrudeLight, FeatherDirection, SymbolDef};

        let mut doc = Document::new("SymCmyk", 100.0, 100.0, ColorMode::Cmyk, 300.0);
        let sym_rect = SceneObject::new(
            ObjectId(901),
            "SymRgbRect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.9, 0.1, 0.1)))
        .with_outline(OutlineStyle::none());
        let sym = SymbolDef::new("RgbSym", vec![sym_rect]);
        let sym_id = sym.id;
        doc.insert_symbol(sym);
        doc.add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id)),
            None,
        )
        .unwrap();

        let mut fx_obj =
            rect_obj(30.0, 30.0, 20.0, 20.0, Color::CYAN).with_outline(OutlineStyle::none());
        fx_obj.effects.push(Effect::InnerShadow {
            dx_mm: 1.0,
            dy_mm: 1.0,
            blur_mm: 2.0,
            opacity: 0.6,
            color: Color::rgb(0.8, 0.1, 0.1),
            feather_direction: FeatherDirection::Inside,
        });
        fx_obj.effects.push(Effect::Bevel {
            style: BevelStyle::SoftEdge,
            distance_mm: 2.0,
            softness_mm: 1.0,
            light_angle_deg: 45.0,
            altitude_deg: 30.0,
            highlight_color: Color::rgb(1.0, 0.9, 0.8),
            shadow_color: Color::rgb(0.1, 0.1, 0.2),
            intensity: 0.7,
        });
        fx_obj.effects.push(Effect::Extrude3D {
            vanishing_point: Point2D::new(80.0, 80.0),
            depth: 10.0,
            bevel_depth: 1.0,
            shade_color: Color::rgb(0.2, 0.3, 0.4),
            rotation_3d: [0.0; 3],
            lights: vec![ExtrudeLight {
                dir: [0.0, 0.0, 1.0],
                intensity: 1.0,
            }],
        });
        let fx_id = doc.add_object(fx_obj, None).unwrap();

        let cfg = MacroSandboxConfig::default();
        let (_exec, ops) = plan_macro_ops(
            &doc,
            find_builtin_macro("recolor-cmyk").unwrap().script,
            &BTreeMap::new(),
            &cfg,
        )
        .unwrap();
        let mut hist = CommandHistory::default();
        hist.execute(&mut doc, CommandOp::Batch(ops)).unwrap();

        let preflight =
            crate::preflight::run_preflight(&doc, &crate::preflight::PreflightRules::default());
        assert!(
            !preflight
                .issues
                .iter()
                .any(|i| i.kind == crate::preflight::PreflightIssueKind::RgbInCmykDocument),
            "Expected 0 RgbInCmykDocument issues after recolor-cmyk, got: {:?}",
            preflight.issues
        );
        for eff in &doc.object(fx_id).unwrap().effects {
            match eff {
                Effect::InnerShadow { color, .. } => assert!(matches!(color, Color::Cmyk { .. })),
                Effect::Bevel {
                    highlight_color,
                    shadow_color,
                    ..
                } => {
                    assert!(matches!(highlight_color, Color::Cmyk { .. }));
                    assert!(matches!(shadow_color, Color::Cmyk { .. }));
                }
                Effect::Extrude3D { shade_color, .. } => {
                    assert!(matches!(shade_color, Color::Cmyk { .. }));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn selection_seeded_macro_and_event_hook_expose_doc_selection() {
        let mut doc = Document::new_a4("SelMacro");
        let id1 = doc
            .add_object(rect_obj(10.0, 10.0, 20.0, 20.0, Color::CYAN), None)
            .unwrap();
        let _id2 = doc
            .add_object(rect_obj(40.0, 10.0, 20.0, 20.0, Color::MAGENTA), None)
            .unwrap();

        let (res, ops) = run_macro_event_hook_with_selection(
            &mut doc,
            MacroEventHook::OnSelectionChange,
            r#"
                log("cnt=" + doc.selection.count + "|ids=" + doc.selection.ids.len());
                for sh in doc.selection.shapes {
                    sh.name = "Selected_" + sh.id;
                }
            "#,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
            &[id1],
        )
        .unwrap();
        assert!(res.logs.iter().any(|l| l == "cnt=1|ids=1"));
        assert!(!ops.is_empty());
        assert_eq!(doc.object(id1).unwrap().name, format!("Selected_{}", id1.0));
    }
}
