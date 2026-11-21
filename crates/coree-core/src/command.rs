// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic serializable command log, transactional undo/redo history,
//! JSON replay, and Record-Macro -> Rhai script export & sandboxed execution.

use crate::boolean::{apply_boolean, apply_boolean_many, BooleanOp};
use crate::color::Color;
use crate::document::{
    AlignOp, DistributeOp, Document, GuideOrientation, MasterLayerScope, NodePayload,
    RasterLayerData, SceneObject, ZOrderOp,
};
use crate::effects::Effect;
use crate::error::{CoreError, CoreResult};
use crate::geometry::BezierPath;
use crate::ids::{LayerId, ObjectId, PageId};
use crate::lens::LensConfig;
use crate::math::Affine2D;
use crate::style::{Fill, OutlineStyle};
use crate::table::VectorTable;
use rhai::{Dynamic, Engine, EvalAltResult};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;

/// Every document mutation represented as a deterministic, JSON-serializable command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandOp {
    CreateObject {
        object: Box<SceneObject>,
        layer_id: Option<LayerId>,
    },
    DeleteObjects {
        ids: Vec<ObjectId>,
    },
    TransformObjects {
        ids: Vec<ObjectId>,
        affine: Affine2D,
    },
    SetFill {
        ids: Vec<ObjectId>,
        fill: Fill,
    },
    SetOutline {
        ids: Vec<ObjectId>,
        outline: OutlineStyle,
    },
    SetLens {
        ids: Vec<ObjectId>,
        lens: Option<LensConfig>,
    },
    AddEffect {
        id: ObjectId,
        effect: Effect,
    },
    RemoveEffect {
        id: ObjectId,
        effect_index: usize,
    },
    EditPath {
        id: ObjectId,
        path: BezierPath,
    },
    BooleanOp {
        op: BooleanOp,
        ids: Vec<ObjectId>,
    },
    Group {
        ids: Vec<ObjectId>,
        name: String,
    },
    Ungroup {
        group_id: ObjectId,
    },
    SetZOrder {
        id: ObjectId,
        op: ZOrderOp,
    },
    AlignObjects {
        ids: Vec<ObjectId>,
        op: AlignOp,
    },
    DistributeObjects {
        ids: Vec<ObjectId>,
        op: DistributeOp,
    },
    AddPage {
        name: String,
        width_mm: f64,
        height_mm: f64,
        bleed_mm: f64,
    },
    DeletePage {
        page_id: PageId,
    },
    RenamePage {
        page_id: PageId,
        new_name: String,
    },
    AddLayer {
        page_id: PageId,
        name: String,
        is_cut_layer: bool,
        master_scope: MasterLayerScope,
    },
    SetLayerFlags {
        layer_id: LayerId,
        visible: Option<bool>,
        locked: Option<bool>,
        printable: Option<bool>,
        is_cut_layer: Option<bool>,
    },
    AddGuide {
        orientation: GuideOrientation,
        locked: bool,
    },
    MoveGuide {
        id: u64,
        orientation: GuideOrientation,
    },
    RemoveGuide {
        id: u64,
    },
    ToggleGuideLock {
        id: u64,
    },
    EditText {
        id: ObjectId,
        payload: NodePayload,
    },
    EditTable {
        id: ObjectId,
        table: VectorTable,
    },
    CreatePowerClip {
        container_id: ObjectId,
        content_ids: Vec<ObjectId>,
    },
    ExtractPowerClip {
        powerclip_id: ObjectId,
    },
    SetPowerClipLock {
        id: ObjectId,
        lock_contents: bool,
    },
    MoveToDesktopLayer {
        id: ObjectId,
    },
    ConvertToCurves {
        id: ObjectId,
    },
    UpdateRaster {
        id: ObjectId,
        data: RasterLayerData,
    },
    Batch(Vec<CommandOp>),
}

impl CommandOp {
    /// Convenience constructor for [`CommandOp::CreateObject`].
    #[must_use]
    pub fn create_object(object: SceneObject, layer_id: Option<LayerId>) -> Self {
        Self::CreateObject {
            object: Box::new(object),
            layer_id,
        }
    }

    /// Short human-readable label for the Undo/Redo history panel.
    #[must_use]
    pub fn default_label(&self) -> String {
        match self {
            Self::CreateObject { object, .. } => format!("Create {}", object.name),
            Self::DeleteObjects { ids } => format!("Delete {} Object(s)", ids.len()),
            Self::TransformObjects { .. } => "Transform Objects".to_owned(),
            Self::SetFill { .. } => "Set Fill".to_owned(),
            Self::SetOutline { .. } => "Set Outline".to_owned(),
            Self::SetLens { .. } => "Set Lens".to_owned(),
            Self::AddEffect { .. } => "Add Effect".to_owned(),
            Self::RemoveEffect { .. } => "Remove Effect".to_owned(),
            Self::EditPath { .. } => "Edit Path".to_owned(),
            Self::BooleanOp { op, .. } => format!("Boolean {op:?}"),
            Self::Group { name, .. } => format!("Group {name}"),
            Self::Ungroup { .. } => "Ungroup".to_owned(),
            Self::SetZOrder { op, .. } => format!("Order {op:?}"),
            Self::AlignObjects { op, .. } => format!("Align {op:?}"),
            Self::DistributeObjects { op, .. } => format!("Distribute {op:?}"),
            Self::AddPage { name, .. } => format!("Add Page {name}"),
            Self::DeletePage { .. } => "Delete Page".to_owned(),
            Self::RenamePage { new_name, .. } => format!("Rename Page {new_name}"),
            Self::AddLayer { name, .. } => format!("Add Layer {name}"),
            Self::SetLayerFlags { .. } => "Update Layer Flags".to_owned(),
            Self::AddGuide { .. } => "Add Guide".to_owned(),
            Self::MoveGuide { .. } => "Move Guide".to_owned(),
            Self::RemoveGuide { .. } => "Remove Guide".to_owned(),
            Self::ToggleGuideLock { .. } => "Toggle Guide Lock".to_owned(),
            Self::EditText { .. } => "Edit Text".to_owned(),
            Self::EditTable { .. } => "Edit Table".to_owned(),
            Self::CreatePowerClip { .. } => "Create PowerClip".to_owned(),
            Self::ExtractPowerClip { .. } => "Extract PowerClip".to_owned(),
            Self::SetPowerClipLock { .. } => "Toggle PowerClip Lock Contents".to_owned(),
            Self::MoveToDesktopLayer { .. } => "Move to Desktop Layer".to_owned(),
            Self::ConvertToCurves { .. } => "Convert to Curves".to_owned(),
            Self::UpdateRaster { .. } => "Update Raster".to_owned(),
            Self::Batch(ops) => format!("Batch ({} ops)", ops.len()),
        }
    }
}

/// Applies a single [`CommandOp`] deterministically to `doc`.
pub fn apply_command(doc: &mut Document, op: &CommandOp) -> CoreResult<()> {
    match op {
        CommandOp::CreateObject { object, layer_id } => {
            doc.add_object((**object).clone(), *layer_id)?;
        }
        CommandOp::DeleteObjects { ids } => {
            for &id in ids {
                if let Some(obj) = doc.objects.get(&id) {
                    if obj.locked {
                        return Err(CoreError::ObjectLocked(id));
                    }
                }
            }
            for &id in ids {
                if doc.objects.contains_key(&id) {
                    doc.remove_object(id)?;
                }
            }
        }
        CommandOp::TransformObjects { ids, affine } => {
            if !affine.is_finite() {
                return Err(CoreError::InvalidGeometry(
                    "transform contains non-finite entries".into(),
                ));
            }
            for &id in ids {
                doc.transform_object(id, *affine)?;
            }
        }
        CommandOp::SetFill { ids, fill } => {
            for &id in ids {
                let obj = doc
                    .objects
                    .get_mut(&id)
                    .ok_or(CoreError::ObjectNotFound(id))?;
                if obj.locked {
                    return Err(CoreError::ObjectLocked(id));
                }
                obj.fill = fill.clone();
            }
            doc.touch();
        }
        CommandOp::SetOutline { ids, outline } => {
            for &id in ids {
                let obj = doc
                    .objects
                    .get_mut(&id)
                    .ok_or(CoreError::ObjectNotFound(id))?;
                if obj.locked {
                    return Err(CoreError::ObjectLocked(id));
                }
                obj.outline = outline.clone();
            }
            doc.touch();
        }
        CommandOp::SetLens { ids, lens } => {
            for &id in ids {
                let obj = doc
                    .objects
                    .get_mut(&id)
                    .ok_or(CoreError::ObjectNotFound(id))?;
                if obj.locked {
                    return Err(CoreError::ObjectLocked(id));
                }
                obj.lens = lens.clone();
            }
            doc.touch();
        }
        CommandOp::AddEffect { id, effect } => {
            let obj = doc
                .objects
                .get_mut(id)
                .ok_or(CoreError::ObjectNotFound(*id))?;
            if obj.locked {
                return Err(CoreError::ObjectLocked(*id));
            }
            obj.effects.push(effect.clone());
            doc.touch();
        }
        CommandOp::RemoveEffect { id, effect_index } => {
            let obj = doc
                .objects
                .get_mut(id)
                .ok_or(CoreError::ObjectNotFound(*id))?;
            if obj.locked {
                return Err(CoreError::ObjectLocked(*id));
            }
            if *effect_index >= obj.effects.len() {
                return Err(CoreError::InvalidParameter(
                    "effect index out of range".into(),
                ));
            }
            obj.effects.remove(*effect_index);
            doc.touch();
        }
        CommandOp::EditPath { id, path } => {
            if !path.is_finite() {
                return Err(CoreError::InvalidGeometry(
                    "edited path contains non-finite coordinates".into(),
                ));
            }
            let obj = doc
                .objects
                .get_mut(id)
                .ok_or(CoreError::ObjectNotFound(*id))?;
            if obj.locked {
                return Err(CoreError::ObjectLocked(*id));
            }
            obj.payload = NodePayload::Path(path.clone());
            doc.touch();
        }
        CommandOp::BooleanOp { op, ids } => {
            if ids.is_empty() {
                return Err(CoreError::BooleanOpFailed("no objects selected".into()));
            }
            let mut world_paths = Vec::with_capacity(ids.len());
            let mut sources = Vec::with_capacity(ids.len());
            for &id in ids {
                let obj = doc
                    .objects
                    .get(&id)
                    .cloned()
                    .ok_or(CoreError::ObjectNotFound(id))?;
                if *op != BooleanOp::CreateBoundary && obj.locked {
                    return Err(CoreError::ObjectLocked(id));
                }
                let wp = doc.world_path_of(id).ok_or_else(|| {
                    CoreError::BooleanOpFailed(format!("object {id} is not a vector path/shape"))
                })?;
                sources.push((obj, doc.layer_of_object(id)));
                world_paths.push(wp);
            }

            if *op == BooleanOp::Simplify {
                // Trim each source by all upper sources while preserving each source's own styling
                let mut trimmed_items: Vec<(SceneObject, Option<LayerId>, BezierPath)> =
                    Vec::with_capacity(sources.len());
                for i in 0..world_paths.len() {
                    let mut current = world_paths[i].clone();
                    for upper in &world_paths[(i + 1)..] {
                        let trimmed = apply_boolean(BooleanOp::Trim, &current, upper)?;
                        current = trimmed.into_iter().next().unwrap_or_default();
                        if current.is_empty() {
                            break;
                        }
                    }
                    if !current.is_empty() {
                        trimmed_items.push((sources[i].0.clone(), sources[i].1, current));
                    }
                }
                for &id in ids {
                    if doc.objects.contains_key(&id) {
                        doc.remove_object(id)?;
                    }
                }
                for (src, lid, path) in trimmed_items {
                    let mut new_obj =
                        SceneObject::new(ObjectId(0), src.name, NodePayload::Path(path));
                    new_obj.fill = src.fill;
                    new_obj.outline = src.outline;
                    new_obj.blend_mode = src.blend_mode;
                    new_obj.opacity = src.opacity;
                    doc.add_object(new_obj, lid)?;
                }
            } else {
                let (ref style_src, layer_id) = sources[0];
                let results = apply_boolean_many(*op, &world_paths)?;

                // For non-destructive CreateBoundary, keep source shapes intact
                if *op != BooleanOp::CreateBoundary {
                    for &id in ids {
                        if doc.objects.contains_key(&id) {
                            doc.remove_object(id)?;
                        }
                    }
                }
                for (idx, path) in results.into_iter().enumerate() {
                    let mut new_obj = SceneObject::new(
                        ObjectId(0),
                        format!("{op:?} {}", idx + 1),
                        NodePayload::Path(path),
                    );
                    new_obj.fill = style_src.fill.clone();
                    new_obj.outline = style_src.outline.clone();
                    new_obj.blend_mode = style_src.blend_mode;
                    new_obj.opacity = style_src.opacity;
                    doc.add_object(new_obj, layer_id)?;
                }
            }
        }
        CommandOp::Group { ids, name } => {
            doc.group_objects(ids, name.clone())?;
        }
        CommandOp::Ungroup { group_id } => {
            doc.ungroup_object(*group_id)?;
        }
        CommandOp::SetZOrder { id, op } => {
            doc.set_z_order(*id, *op)?;
        }
        CommandOp::AlignObjects { ids, op } => {
            doc.align_objects(ids, *op)?;
        }
        CommandOp::DistributeObjects { ids, op } => {
            doc.distribute_objects(ids, *op)?;
        }
        CommandOp::AddPage {
            name,
            width_mm,
            height_mm,
            bleed_mm,
        } => {
            doc.add_page(name.clone(), *width_mm, *height_mm, *bleed_mm);
        }
        CommandOp::DeletePage { page_id } => {
            doc.delete_page(*page_id)?;
        }
        CommandOp::RenamePage { page_id, new_name } => {
            doc.rename_page(*page_id, new_name.clone())?;
        }
        CommandOp::AddLayer {
            page_id,
            name,
            is_cut_layer,
            master_scope,
        } => {
            doc.add_layer(*page_id, name.clone(), *is_cut_layer, *master_scope)?;
        }
        CommandOp::SetLayerFlags {
            layer_id,
            visible,
            locked,
            printable,
            is_cut_layer,
        } => {
            doc.set_layer_flags(*layer_id, *visible, *locked, *printable, *is_cut_layer)?;
        }
        CommandOp::AddGuide {
            orientation,
            locked,
        } => {
            doc.add_guide(*orientation, *locked);
        }
        CommandOp::MoveGuide { id, orientation } => {
            if let Some(g) = doc.guides.iter_mut().find(|g| g.id == *id) {
                if !g.locked {
                    g.orientation = *orientation;
                    doc.touch();
                }
            }
        }
        CommandOp::RemoveGuide { id } => {
            let _ = doc.remove_guide(*id);
        }
        CommandOp::ToggleGuideLock { id } => {
            if let Some(g) = doc.guides.iter_mut().find(|g| g.id == *id) {
                g.locked = !g.locked;
                doc.touch();
            }
        }
        CommandOp::EditText { id, payload } => {
            if !matches!(
                payload,
                NodePayload::ArtisticText(_)
                    | NodePayload::ParagraphText(_)
                    | NodePayload::TextOnPath(_)
            ) {
                return Err(CoreError::InvalidParameter(
                    "EditText payload must be ArtisticText, ParagraphText, or TextOnPath".into(),
                ));
            }
            let obj = doc
                .objects
                .get_mut(id)
                .ok_or(CoreError::ObjectNotFound(*id))?;
            if obj.locked {
                return Err(CoreError::ObjectLocked(*id));
            }
            obj.payload = payload.clone();
            doc.touch();
        }
        CommandOp::EditTable { id, table } => {
            let obj = doc
                .objects
                .get_mut(id)
                .ok_or(CoreError::ObjectNotFound(*id))?;
            if obj.locked {
                return Err(CoreError::ObjectLocked(*id));
            }
            obj.payload = NodePayload::Table(table.clone());
            doc.touch();
        }
        CommandOp::CreatePowerClip {
            container_id,
            content_ids,
        } => {
            doc.create_powerclip(*container_id, content_ids)?;
        }
        CommandOp::ExtractPowerClip { powerclip_id } => {
            doc.extract_powerclip(*powerclip_id)?;
        }
        CommandOp::SetPowerClipLock { id, lock_contents } => {
            doc.set_powerclip_lock_contents(*id, *lock_contents)?;
        }
        CommandOp::MoveToDesktopLayer { id } => {
            doc.move_object_to_desktop_layer(*id)?;
        }
        CommandOp::ConvertToCurves { id } => {
            let obj = doc.objects.get(id).ok_or(CoreError::ObjectNotFound(*id))?;
            if obj.locked {
                return Err(CoreError::ObjectLocked(*id));
            }
            doc.convert_to_curves(*id)?;
        }
        CommandOp::UpdateRaster { id, data } => {
            let obj = doc
                .objects
                .get_mut(id)
                .ok_or(CoreError::ObjectNotFound(*id))?;
            if obj.locked {
                return Err(CoreError::ObjectLocked(*id));
            }
            obj.payload = NodePayload::Raster(data.clone());
            doc.touch();
        }
        CommandOp::Batch(ops) => {
            for sub in ops {
                apply_command(doc, sub)?;
            }
        }
    }
    Ok(())
}

/// Compact reversible scene delta stored on the undo/redo stack instead of duplicating
/// the entire object table twice per edit.
#[derive(Debug, Clone)]
struct ObjectDelta {
    id: ObjectId,
    before: Option<SceneObject>,
    after: Option<SceneObject>,
}

#[derive(Debug, Clone)]
struct DocumentDelta {
    title: Option<(String, String)>,
    pages: Option<(Vec<crate::document::Page>, Vec<crate::document::Page>)>,
    active_page_index: Option<(usize, usize)>,
    multipage_view: Option<(bool, bool)>,
    guides: Option<(Vec<crate::document::Guide>, Vec<crate::document::Guide>)>,
    next_id: Option<(u64, u64)>,
    objects: Vec<ObjectDelta>,
}

/// Full rollback snapshot kept temporarily during an active command/transaction.
#[derive(Debug, Clone)]
struct DocumentStateSnapshot {
    title: String,
    pages: Vec<crate::document::Page>,
    active_page_index: usize,
    multipage_view: bool,
    objects: std::collections::BTreeMap<ObjectId, SceneObject>,
    guides: Vec<crate::document::Guide>,
    next_id: u64,
}

impl DocumentStateSnapshot {
    fn capture(doc: &Document) -> Self {
        Self {
            title: doc.title.clone(),
            pages: doc.pages.clone(),
            active_page_index: doc.active_page_index,
            multipage_view: doc.multipage_view,
            objects: doc.objects.clone(),
            guides: doc.guides.clone(),
            next_id: doc.next_id,
        }
    }

    fn restore_into(&self, doc: &mut Document) {
        doc.title = self.title.clone();
        doc.pages = self.pages.clone();
        doc.active_page_index = self.active_page_index;
        doc.multipage_view = self.multipage_view;
        doc.objects = self.objects.clone();
        doc.guides = self.guides.clone();
        doc.next_id = self.next_id;
        doc.touch();
    }

    fn into_delta(self, after: &Document) -> DocumentDelta {
        let mut object_deltas = Vec::new();
        for (id, before_obj) in &self.objects {
            match after.objects.get(id) {
                Some(after_obj) if after_obj == before_obj => {}
                Some(after_obj) => object_deltas.push(ObjectDelta {
                    id: *id,
                    before: Some(before_obj.clone()),
                    after: Some(after_obj.clone()),
                }),
                None => object_deltas.push(ObjectDelta {
                    id: *id,
                    before: Some(before_obj.clone()),
                    after: None,
                }),
            }
        }
        for (id, after_obj) in &after.objects {
            if !self.objects.contains_key(id) {
                object_deltas.push(ObjectDelta {
                    id: *id,
                    before: None,
                    after: Some(after_obj.clone()),
                });
            }
        }

        DocumentDelta {
            title: (self.title != after.title).then(|| (self.title, after.title.clone())),
            pages: (self.pages != after.pages).then(|| (self.pages, after.pages.clone())),
            active_page_index: (self.active_page_index != after.active_page_index)
                .then_some((self.active_page_index, after.active_page_index)),
            multipage_view: (self.multipage_view != after.multipage_view)
                .then_some((self.multipage_view, after.multipage_view)),
            guides: (self.guides != after.guides).then(|| (self.guides, after.guides.clone())),
            next_id: (self.next_id != after.next_id).then_some((self.next_id, after.next_id)),
            objects: object_deltas,
        }
    }
}

impl DocumentDelta {
    fn apply_before(&self, doc: &mut Document) {
        if let Some((b, _)) = &self.title {
            doc.title = b.clone();
        }
        if let Some((b, _)) = &self.pages {
            doc.pages = b.clone();
        }
        if let Some((b, _)) = self.active_page_index {
            doc.active_page_index = b;
        }
        if let Some((b, _)) = self.multipage_view {
            doc.multipage_view = b;
        }
        if let Some((b, _)) = &self.guides {
            doc.guides = b.clone();
        }
        if let Some((b, _)) = self.next_id {
            doc.next_id = b;
        }
        for d in &self.objects {
            match &d.before {
                Some(obj) => {
                    doc.objects.insert(d.id, obj.clone());
                }
                None => {
                    doc.objects.remove(&d.id);
                }
            }
        }
        doc.touch();
    }

    fn apply_after(&self, doc: &mut Document) {
        if let Some((_, a)) = &self.title {
            doc.title = a.clone();
        }
        if let Some((_, a)) = &self.pages {
            doc.pages = a.clone();
        }
        if let Some((_, a)) = self.active_page_index {
            doc.active_page_index = a;
        }
        if let Some((_, a)) = self.multipage_view {
            doc.multipage_view = a;
        }
        if let Some((_, a)) = &self.guides {
            doc.guides = a.clone();
        }
        if let Some((_, a)) = self.next_id {
            doc.next_id = a;
        }
        for d in &self.objects {
            match &d.after {
                Some(obj) => {
                    doc.objects.insert(d.id, obj.clone());
                }
                None => {
                    doc.objects.remove(&d.id);
                }
            }
        }
        doc.touch();
    }
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    label: String,
    op: CommandOp,
    delta: DocumentDelta,
}

#[derive(Debug, Clone)]
struct ActiveTransaction {
    label: String,
    before: DocumentStateSnapshot,
    ops: Vec<CommandOp>,
}

/// Transactional Undo/Redo stack, deterministic command journal, and Rhai macro recorder.
#[derive(Debug, Clone)]
pub struct CommandHistory {
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    active_tx: Option<ActiveTransaction>,
    max_history: usize,
    /// When `true`, every committed command is recorded for Rhai macro export.
    pub recording_macro: bool,
    recorded_ops: Vec<CommandOp>,
}

impl Default for CommandHistory {
    fn default() -> Self {
        Self::new(200)
    }
}

impl CommandHistory {
    /// Creates an empty command history with the given undo retention limit.
    #[must_use]
    pub fn new(max_history: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            active_tx: None,
            max_history: max_history.max(1),
            recording_macro: false,
            recorded_ops: Vec::new(),
        }
    }

    /// Executes `op` on `doc`, recording it in the undo history (or active transaction).
    pub fn execute(&mut self, doc: &mut Document, op: CommandOp) -> CoreResult<()> {
        if let Some(tx) = self.active_tx.as_mut() {
            let rollback = DocumentStateSnapshot::capture(doc);
            if let Err(err) = apply_command(doc, &op) {
                rollback.restore_into(doc);
                return Err(err);
            }
            tx.ops.push(op);
            return Ok(());
        }

        let before = DocumentStateSnapshot::capture(doc);
        if let Err(err) = apply_command(doc, &op) {
            // Roll back partial mutation on error (e.g. failed batch or locked object)
            before.restore_into(doc);
            return Err(err);
        }

        let delta = before.into_delta(doc);
        if self.recording_macro {
            self.recorded_ops.push(op.clone());
        }
        let label = op.default_label();
        self.push_entry(HistoryEntry { label, op, delta });
        Ok(())
    }

    /// Begins a multi-command transaction (e.g. interactive drag or multi-step wizard)
    /// that commits as a single merged undo step.
    pub fn begin_transaction(
        &mut self,
        doc: &Document,
        label: impl Into<String>,
    ) -> CoreResult<()> {
        if self.active_tx.is_some() {
            return Err(CoreError::TransactionAlreadyActive);
        }
        self.active_tx = Some(ActiveTransaction {
            label: label.into(),
            before: DocumentStateSnapshot::capture(doc),
            ops: Vec::new(),
        });
        Ok(())
    }

    /// Commits the active transaction, merging all executed commands into a single history entry.
    pub fn commit_transaction(&mut self, doc: &Document) -> CoreResult<()> {
        let tx = self
            .active_tx
            .take()
            .ok_or(CoreError::TransactionNotActive)?;
        if tx.ops.is_empty() {
            return Ok(());
        }
        let merged_op = if tx.ops.len() == 1 {
            tx.ops.into_iter().next().expect("len == 1")
        } else {
            CommandOp::Batch(tx.ops)
        };
        if self.recording_macro {
            self.recorded_ops.push(merged_op.clone());
        }
        let delta = tx.before.into_delta(doc);
        self.push_entry(HistoryEntry {
            label: tx.label,
            op: merged_op,
            delta,
        });
        Ok(())
    }

    /// Cancels the active transaction and rolls `doc` back to its pre-transaction state.
    pub fn cancel_transaction(&mut self, doc: &mut Document) -> CoreResult<()> {
        let tx = self
            .active_tx
            .take()
            .ok_or(CoreError::TransactionNotActive)?;
        tx.before.restore_into(doc);
        Ok(())
    }

    fn push_entry(&mut self, entry: HistoryEntry) {
        self.redo_stack.clear();
        self.undo_stack.push(entry);
        if self.undo_stack.len() > self.max_history {
            self.undo_stack.remove(0);
        }
    }

    /// Undoes the most recent command or transaction.
    ///
    /// Fails with [`CoreError::TransactionAlreadyActive`] if called while a transaction is open.
    pub fn undo(&mut self, doc: &mut Document) -> CoreResult<String> {
        if self.active_tx.is_some() {
            return Err(CoreError::TransactionAlreadyActive);
        }
        let entry = self.undo_stack.pop().ok_or(CoreError::EmptyUndoStack)?;
        let label = entry.label.clone();
        entry.delta.apply_before(doc);
        self.redo_stack.push(entry);
        Ok(label)
    }

    /// Redoes the most recently undone command or transaction.
    ///
    /// Fails with [`CoreError::TransactionAlreadyActive`] if called while a transaction is open.
    pub fn redo(&mut self, doc: &mut Document) -> CoreResult<String> {
        if self.active_tx.is_some() {
            return Err(CoreError::TransactionAlreadyActive);
        }
        let entry = self.redo_stack.pop().ok_or(CoreError::EmptyRedoStack)?;
        let label = entry.label.clone();
        entry.delta.apply_after(doc);
        self.undo_stack.push(entry);
        Ok(label)
    }

    /// Returns `true` if at least one command can be undone.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Returns `true` if at least one command can be redone.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Labels of the undo stack from oldest to most recent.
    #[must_use]
    pub fn undo_labels(&self) -> Vec<&str> {
        self.undo_stack.iter().map(|e| e.label.as_str()).collect()
    }

    /// Labels of the redo stack from oldest to next-to-redo.
    #[must_use]
    pub fn redo_labels(&self) -> Vec<&str> {
        self.redo_stack.iter().map(|e| e.label.as_str()).collect()
    }

    /// Returns the ordered deterministic command log currently applied to the document.
    #[must_use]
    pub fn command_log(&self) -> Vec<CommandOp> {
        self.undo_stack.iter().map(|e| e.op.clone()).collect()
    }

    /// Exports the applied command log as formatted JSON.
    pub fn export_log_json(&self) -> CoreResult<String> {
        Ok(serde_json::to_string_pretty(&self.command_log())?)
    }

    /// Parses a JSON command log previously exported by [`Self::export_log_json`].
    pub fn import_log_json(json: &str) -> CoreResult<Vec<CommandOp>> {
        Ok(serde_json::from_str(json)?)
    }

    /// Deterministically replays a sequence of [`CommandOp`]s onto `doc`.
    pub fn replay_log(doc: &mut Document, ops: &[CommandOp]) -> CoreResult<()> {
        for (index, op) in ops.iter().enumerate() {
            if let Err(err) = apply_command(doc, op) {
                return Err(CoreError::CommandReplayFailed {
                    index,
                    reason: err.to_string(),
                });
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Record Macro -> Rhai Script Engine
    // -------------------------------------------------------------------------

    /// Starts recording subsequent executed commands into a Rhai macro buffer.
    pub fn start_macro_recording(&mut self) {
        self.recorded_ops.clear();
        self.recording_macro = true;
    }

    /// Stops macro recording and emits an executable Rhai script reproducing every recorded command.
    pub fn stop_macro_to_rhai(&mut self) -> String {
        self.recording_macro = false;
        let ops = std::mem::take(&mut self.recorded_ops);
        let mut script = String::from("// coree recorded macro (Rhai)\n");
        for op in &ops {
            emit_op_rhai(op, &mut script);
        }
        script
    }
}

fn format_id_array(ids: &[ObjectId]) -> String {
    let items: Vec<String> = ids.iter().map(|id| id.0.to_string()).collect();
    format!("[{}]", items.join(", "))
}

fn escape_rhai_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn emit_op_rhai(op: &CommandOp, out: &mut String) {
    match op {
        CommandOp::Batch(sub_ops) => {
            out.push_str("// begin batch\n");
            for sub in sub_ops {
                emit_op_rhai(sub, out);
            }
            out.push_str("// end batch\n");
        }
        CommandOp::TransformObjects { ids, affine } => {
            let [a, b, c, d, tx, ty] = affine.m;
            out.push_str(&format!(
                "transform_objects({}, [{a:?}, {b:?}, {c:?}, {d:?}, {tx:?}, {ty:?}]);\n",
                format_id_array(ids)
            ));
        }
        CommandOp::SetFill {
            ids,
            fill: Fill::Solid(color @ Color::Rgba { .. }),
        } => {
            let hex = color.to_hex_rgba();
            out.push_str(&format!(
                "set_fill_hex({}, \"{hex}\");\n",
                format_id_array(ids)
            ));
        }
        CommandOp::AlignObjects { ids, op } => {
            let mode = match op {
                AlignOp::Left => "left",
                AlignOp::CenterH => "center_h",
                AlignOp::Right => "right",
                AlignOp::Top => "top",
                AlignOp::CenterV => "center_v",
                AlignOp::Bottom => "bottom",
                AlignOp::CenterPage => "center_page",
            };
            out.push_str(&format!(
                "align_objects({}, \"{mode}\");\n",
                format_id_array(ids)
            ));
        }
        CommandOp::DeleteObjects { ids } => {
            out.push_str(&format!("delete_objects({});\n", format_id_array(ids)));
        }
        CommandOp::Group { ids, name } => {
            out.push_str(&format!(
                "group_objects({}, \"{}\");\n",
                format_id_array(ids),
                escape_rhai_str(name)
            ));
        }
        CommandOp::ConvertToCurves { id } => {
            out.push_str(&format!("convert_to_curves({});\n", id.0));
        }
        CommandOp::AddPage {
            name,
            width_mm,
            height_mm,
            bleed_mm,
        } => {
            out.push_str(&format!(
                "add_page(\"{}\", {width_mm:?}, {height_mm:?}, {bleed_mm:?});\n",
                escape_rhai_str(name)
            ));
        }
        other => {
            if let Ok(json) = serde_json::to_string(other) {
                out.push_str(&format!(
                    "exec_command_json(\"{}\");\n",
                    escape_rhai_str(&json)
                ));
            }
        }
    }
}

/// Converts a Rhai integer array into typed [`ObjectId`] values (skipping negatives).
#[must_use]
pub fn rhai_array_to_ids(arr: rhai::Array) -> Vec<ObjectId> {
    arr.into_iter()
        .filter_map(|v: Dynamic| v.as_int().ok().map(|n| ObjectId(n.max(0) as u64)))
        .collect()
}

/// Runs a Rhai macro script (such as one emitted by [`CommandHistory::stop_macro_to_rhai`])
/// inside a sandboxed `rhai::Engine`, returning the number of commands executed.
pub fn run_rhai_macro(
    doc: &mut Document,
    history: &mut CommandHistory,
    script: &str,
) -> CoreResult<usize> {
    let queued: Rc<RefCell<Vec<CommandOp>>> = Rc::new(RefCell::new(Vec::new()));
    let mut engine = Engine::new();
    // Sandbox safety limits against infinite loops or runaway memory
    engine.set_max_operations(100_000);
    engine.set_max_expr_depths(32, 32);
    engine.set_max_string_size(1_048_576);

    {
        let q = Rc::clone(&queued);
        engine.register_fn(
            "transform_objects",
            move |ids: rhai::Array, m: rhai::Array| -> Result<(), Box<EvalAltResult>> {
                if m.len() != 6 {
                    return Err(format!(
                        "transform_objects expects a 6-element affine array, got {}",
                        m.len()
                    )
                    .into());
                }
                let mut coeffs = [0.0_f64; 6];
                for (i, v) in m.into_iter().enumerate() {
                    coeffs[i] = v
                        .as_float()
                        .or_else(|_| v.as_int().map(|n| n as f64))
                        .map_err(|_| -> Box<EvalAltResult> {
                            format!("transform_objects matrix entry {i} must be numeric").into()
                        })?;
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
                    .map_err(|e| -> Box<EvalAltResult> { format!("set_fill_hex: {e}").into() })?;
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
                let op = match mode {
                    "left" => AlignOp::Left,
                    "center_h" => AlignOp::CenterH,
                    "right" => AlignOp::Right,
                    "top" => AlignOp::Top,
                    "center_v" => AlignOp::CenterV,
                    "bottom" => AlignOp::Bottom,
                    "center_page" => AlignOp::CenterPage,
                    other => {
                        return Err(format!("align_objects: unknown align mode '{other}'").into());
                    }
                };
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
                    return Err(format!("convert_to_curves: invalid object id {id}").into());
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
        let dyn_to_f64 = |v: Dynamic, field: &str| -> Result<f64, Box<EvalAltResult>> {
            v.as_float()
                .or_else(|_| v.as_int().map(|n| n as f64))
                .map_err(|_| format!("add_page: '{field}' must be numeric").into())
        };
        engine.register_fn(
            "add_page",
            move |name: &str,
                  width_mm: Dynamic,
                  height_mm: Dynamic,
                  bleed_mm: Dynamic|
                  -> Result<(), Box<EvalAltResult>> {
                q.borrow_mut().push(CommandOp::AddPage {
                    name: name.to_owned(),
                    width_mm: dyn_to_f64(width_mm, "width_mm")?,
                    height_mm: dyn_to_f64(height_mm, "height_mm")?,
                    bleed_mm: dyn_to_f64(bleed_mm, "bleed_mm")?,
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
                let op: CommandOp =
                    serde_json::from_str(json).map_err(|e| -> Box<EvalAltResult> {
                        format!("exec_command_json: invalid CommandOp JSON: {e}").into()
                    })?;
                q.borrow_mut().push(op);
                Ok(())
            },
        );
    }

    engine
        .run(script)
        .map_err(|e| CoreError::MacroError(e.to_string()))?;

    let ops = queued.borrow().clone();
    let count = ops.len();
    if count > 0 {
        history.begin_transaction(doc, "Run Macro")?;
        for op in ops {
            if let Err(err) = history.execute(doc, op) {
                let _ = history.cancel_transaction(doc);
                return Err(err);
            }
        }
        history.commit_transaction(doc)?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{CornerStyle, ParametricShape};
    use crate::lens::{LensConfig, LensKind};
    use crate::math::Point2D;

    fn sample_rect(name: &str, x: f64, y: f64) -> SceneObject {
        SceneObject::new(
            ObjectId(0),
            name,
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 10.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_transform(Affine2D::translate(x, y))
    }

    #[test]
    fn undo_redo_transaction_merging_and_deterministic_json_replay() {
        let initial = Document::new_a4("Replay Test");
        let mut doc = initial.clone();
        let mut hist = CommandHistory::default();

        // Execute commands inside a merged transaction
        hist.begin_transaction(&doc, "Create & Style Pair").unwrap();
        hist.execute(
            &mut doc,
            CommandOp::create_object(sample_rect("R1", 0.0, 0.0), None),
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::create_object(sample_rect("R2", 10.0, 0.0), None),
        )
        .unwrap();
        hist.commit_transaction(&doc).unwrap();
        assert_eq!(hist.undo_labels(), vec!["Create & Style Pair"]);
        assert_eq!(doc.objects.len(), 2);

        let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
        hist.execute(
            &mut doc,
            CommandOp::SetLens {
                ids: vec![ids[0]],
                lens: Some(LensConfig::new(LensKind::Invert)),
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::BooleanOp {
                op: BooleanOp::Weld,
                ids: ids.clone(),
            },
        )
        .unwrap();
        assert_eq!(doc.objects.len(), 1);

        // Undo boolean + lens + merged transaction
        hist.undo(&mut doc).unwrap();
        assert_eq!(doc.objects.len(), 2);
        hist.undo(&mut doc).unwrap();
        assert!(doc.objects[&ids[0]].lens.is_none());
        hist.undo(&mut doc).unwrap();
        assert!(doc.objects.is_empty());
        assert!(hist.undo(&mut doc).is_err());

        // Redo everything back
        while hist.can_redo() {
            hist.redo(&mut doc).unwrap();
        }
        assert_eq!(doc.objects.len(), 1);

        // Export JSON log and deterministically replay onto a clean clone of initial
        let json = hist.export_log_json().unwrap();
        let parsed = CommandHistory::import_log_json(&json).unwrap();
        let mut replayed = initial;
        CommandHistory::replay_log(&mut replayed, &parsed).unwrap();

        assert_eq!(replayed.objects, doc.objects);
        assert_eq!(replayed.pages, doc.pages);
        assert_eq!(replayed.next_id, doc.next_id);
    }

    #[test]
    fn record_macro_to_rhai_and_execute() {
        let mut doc = Document::new_a4("Macro Test");
        let mut hist = CommandHistory::default();

        let id = doc
            .add_object(sample_rect("Target", 5.0, 5.0), None)
            .unwrap();

        hist.start_macro_recording();
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![id],
                affine: Affine2D::translate(15.0, 25.0),
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::SetFill {
                ids: vec![id],
                fill: Fill::Solid(Color::from_hex("#ff3366").unwrap()),
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::AddGuide {
                orientation: GuideOrientation::Vertical(42.0),
                locked: false,
            },
        )
        .unwrap();

        let rhai_script = hist.stop_macro_to_rhai();
        assert!(rhai_script.contains("transform_objects"));
        assert!(rhai_script.contains("set_fill_hex"));
        assert!(rhai_script.contains("exec_command_json"));

        // Re-run recorded Rhai script on a fresh document with same object ID
        let mut doc2 = Document::new_a4("Macro Target");
        let mut obj = sample_rect("Target", 5.0, 5.0);
        obj.id = id;
        doc2.add_object(obj, None).unwrap();
        let mut hist2 = CommandHistory::default();

        let executed = run_rhai_macro(&mut doc2, &mut hist2, &rhai_script).unwrap();
        assert_eq!(executed, 3);
        let bb = doc2.object_bounds(id).unwrap();
        assert!((bb.min_x - 20.0).abs() < 1e-6);
        assert!((bb.min_y - 30.0).abs() < 1e-6);
        assert_eq!(doc2.guides.len(), 1);
        assert_eq!(
            doc2.objects[&id].fill,
            Fill::Solid(Color::from_hex("#ff3366").unwrap())
        );

        // Sandbox blocks infinite loop scripts cleanly
        let err = run_rhai_macro(&mut doc2, &mut hist2, "loop {}");
        assert!(err.is_err());

        // Non-finite transform fails without mutating document
        let before_bb = doc2.object_bounds(id).unwrap();
        let bad = hist2.execute(
            &mut doc2,
            CommandOp::TransformObjects {
                ids: vec![id],
                affine: Affine2D::translate(f64::NAN, Point2D::ZERO.y),
            },
        );
        assert!(bad.is_err());
        assert_eq!(doc2.object_bounds(id).unwrap(), before_bb);
    }

    #[test]
    fn delta_history_powerclip_unlock_and_desktop_layer_roundtrip() {
        let mut doc = Document::new_a4("Delta & Desktop");
        doc.add_page("Page 2", 210.0, 297.0, 3.0);
        let mut hist = CommandHistory::default();

        // Populate 200 background objects to verify delta storage does not bloat per command
        let mut created_ids = Vec::with_capacity(200);
        for i in 0..200 {
            let mut r = sample_rect(&format!("R{i}"), 20.0, 20.0);
            r.transform = Affine2D::translate(f64::from(i), 10.0);
            let obj_id = doc.add_object(r, None).unwrap();
            created_ids.push(obj_id);
        }

        // Single object move -> compact ObjectDelta only tracks that single changed ID
        let moved_id = created_ids[0];
        let orig_x = doc.object_bounds(moved_id).unwrap().min_x;
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![moved_id],
                affine: Affine2D::translate(25.0, 0.0),
            },
        )
        .unwrap();
        assert_eq!(hist.undo_stack.len(), 1);
        assert_eq!(hist.undo_stack[0].delta.objects.len(), 1);
        assert!((doc.object_bounds(moved_id).unwrap().min_x - (orig_x + 25.0)).abs() < 1e-6);

        hist.undo(&mut doc).unwrap();
        assert!((doc.object_bounds(moved_id).unwrap().min_x - orig_x).abs() < 1e-6);

        // Move object to cross-page Desktop layer -> visible in page 1 spatial index too
        let desktop_obj = created_ids[1];
        hist.execute(&mut doc, CommandOp::MoveToDesktopLayer { id: desktop_obj })
            .unwrap();
        let desktop_lid = doc.desktop_layer_id().expect("desktop layer created");
        assert_eq!(doc.layer_of_object(desktop_obj), Some(desktop_lid));
        let idx_page1 = doc.build_spatial_index(1);
        assert!(!idx_page1.is_empty());
        doc.active_page_index = 1;
        assert_eq!(
            doc.hit_test(doc.object_bounds(desktop_obj).unwrap().center(), 1.0),
            Some(desktop_obj)
        );
        doc.active_page_index = 0;

        // PowerClip lock_contents = false keeps child anchored in world space when container moves
        let child_id = created_ids[2];
        let container_id = created_ids[3];
        let child_bb_before = doc.world_object_bounds(child_id).unwrap();
        let child_xf_before = doc.world_transform_of(child_id).unwrap();
        hist.execute(
            &mut doc,
            CommandOp::CreatePowerClip {
                container_id,
                content_ids: vec![child_id],
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::SetPowerClipLock {
                id: container_id,
                lock_contents: false,
            },
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![container_id],
                affine: Affine2D::translate(40.0, 15.0),
            },
        )
        .unwrap();
        let child_bb_after = doc.world_object_bounds(child_id).unwrap();
        let child_xf_after = doc.world_transform_of(child_id).unwrap();
        assert!((child_bb_after.min_x - child_bb_before.min_x).abs() < 1e-4);
        assert!((child_bb_after.min_y - child_bb_before.min_y).abs() < 1e-4);
        assert!((child_xf_after.m[4] - child_xf_before.m[4]).abs() < 1e-4);
        assert!((child_xf_after.m[5] - child_xf_before.m[5]).abs() < 1e-4);
    }
}
