// SPDX-License-Identifier: MIT OR Apache-2.0

//! Object and node selection state, dual-click transform handle modes, and movable rotation pivot.

use crate::document::Document;
use crate::ids::ObjectId;
use crate::math::{Point2D, Rect2D};
use serde::{Deserialize, Serialize};

/// Dual-click interactive transform handle mode on the Pick tool.
///
/// First click selects an object in [`TransformMode::ScaleAndMove`]; clicking the
/// already-selected object a second time toggles into [`TransformMode::RotateAndSkew`]
/// exposing corner rotation arcs, side skew arrows, and the draggable rotation pivot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TransformMode {
    #[default]
    ScaleAndMove,
    RotateAndSkew,
}

impl TransformMode {
    /// Toggles between `ScaleAndMove` and `RotateAndSkew`.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::ScaleAndMove => Self::RotateAndSkew,
            Self::RotateAndSkew => Self::ScaleAndMove,
        }
    }
}

/// Reference to a specific Bézier anchor node on a specific scene object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeRef {
    pub object_id: ObjectId,
    pub subpath_index: usize,
    pub node_index: usize,
}

/// Active node selection for the Shape/Node editing tool.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeSelection {
    pub nodes: Vec<NodeRef>,
}

impl NodeSelection {
    /// Creates an empty node selection.
    #[must_use]
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Replaces the selection with a single node.
    pub fn select_single(&mut self, node: NodeRef) {
        self.nodes.clear();
        self.nodes.push(node);
    }

    /// Toggles membership of `node` (Shift+click behavior).
    pub fn toggle(&mut self, node: NodeRef) {
        if let Some(pos) = self.nodes.iter().position(|&n| n == node) {
            self.nodes.remove(pos);
        } else {
            self.nodes.push(node);
        }
    }

    /// Returns `true` if `node` is currently selected.
    #[must_use]
    pub fn contains(&self, node: NodeRef) -> bool {
        self.nodes.contains(&node)
    }

    /// Clears all selected nodes.
    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Returns `true` when no nodes are selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Complete canvas selection state tracking selected objects, dual-click transform mode,
/// movable rotation/skew pivot, and active Bézier node selection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SelectionState {
    pub objects: Vec<ObjectId>,
    pub transform_mode: TransformMode,
    /// Current rotation/skew pivot in canvas millimeter coordinates.
    pub pivot: Point2D,
    /// When `true`, `pivot` has been dragged by the user and stays sticky until selection changes.
    pub custom_pivot: bool,
    pub node_selection: NodeSelection,
}

impl SelectionState {
    /// Creates an empty selection state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of selected objects.
    #[must_use]
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Returns `true` if no objects are selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Returns `true` if `id` is currently selected.
    #[must_use]
    pub fn contains(&self, id: ObjectId) -> bool {
        self.objects.contains(&id)
    }

    /// Clears both object and node selections and resets transform mode to `ScaleAndMove`.
    pub fn clear(&mut self) {
        self.objects.clear();
        self.node_selection.clear();
        self.transform_mode = TransformMode::ScaleAndMove;
        self.custom_pivot = false;
        self.pivot = Point2D::ZERO;
    }

    /// Selects a single object cleanly, resetting transform mode to `ScaleAndMove` and
    /// centering the pivot on its bounding box.
    pub fn select_single(&mut self, doc: &Document, id: ObjectId) {
        self.objects.clear();
        if doc.objects.contains_key(&id) {
            self.objects.push(id);
        }
        self.transform_mode = TransformMode::ScaleAndMove;
        self.custom_pivot = false;
        self.node_selection.clear();
        self.sync_pivot_to_bounds(doc);
    }

    /// Handles Pick-tool clicking on `id`:
    /// - Without `shift`: clicking the sole already-selected object toggles
    ///   `ScaleAndMove <-> RotateAndSkew`; clicking any other object selects it in `ScaleAndMove`.
    /// - With `shift`: toggles `id` in/out of the multi-selection.
    pub fn click_object(&mut self, doc: &Document, id: ObjectId, shift: bool) {
        if !doc.objects.contains_key(&id) {
            return;
        }
        if shift {
            self.toggle_object(doc, id);
            return;
        }
        if self.objects.len() == 1 && self.objects[0] == id {
            self.transform_mode = self.transform_mode.toggled();
            if !self.custom_pivot {
                self.sync_pivot_to_bounds(doc);
            }
        } else {
            self.select_single(doc, id);
        }
    }

    /// Selects multiple objects (deduplicated, filtered against `doc`).
    pub fn select_many(&mut self, doc: &Document, ids: &[ObjectId]) {
        self.objects.clear();
        for &id in ids {
            if doc.objects.contains_key(&id) && !self.objects.contains(&id) {
                self.objects.push(id);
            }
        }
        self.transform_mode = TransformMode::ScaleAndMove;
        self.custom_pivot = false;
        self.node_selection.clear();
        self.sync_pivot_to_bounds(doc);
    }

    /// Adds or removes `id` from the selection.
    pub fn toggle_object(&mut self, doc: &Document, id: ObjectId) {
        if let Some(pos) = self.objects.iter().position(|&oid| oid == id) {
            self.objects.remove(pos);
        } else if doc.objects.contains_key(&id) {
            self.objects.push(id);
        }
        self.transform_mode = TransformMode::ScaleAndMove;
        self.custom_pivot = false;
        self.sync_pivot_to_bounds(doc);
    }

    /// Removes any IDs that no longer exist in `doc` (e.g. after Delete or Undo).
    pub fn prune_missing(&mut self, doc: &Document) {
        self.objects.retain(|id| doc.objects.contains_key(id));
        self.node_selection
            .nodes
            .retain(|n| doc.objects.contains_key(&n.object_id));
        if self.objects.is_empty() {
            self.transform_mode = TransformMode::ScaleAndMove;
            self.custom_pivot = false;
        } else if !self.custom_pivot {
            self.sync_pivot_to_bounds(doc);
        }
    }

    /// Combined world-space bounding box of the current selection.
    #[must_use]
    pub fn bounding_box(&self, doc: &Document) -> Option<Rect2D> {
        doc.combined_bounds(&self.objects)
    }

    /// Moves the rotation/skew pivot to an explicit canvas position.
    pub fn set_pivot(&mut self, pivot: Point2D) {
        if pivot.is_finite() {
            self.pivot = pivot;
            self.custom_pivot = true;
        }
    }

    /// Resets the rotation/skew pivot back to the selection's bounding box center.
    pub fn reset_pivot(&mut self, doc: &Document) {
        self.custom_pivot = false;
        self.sync_pivot_to_bounds(doc);
    }

    /// Toggles `TransformMode` directly.
    pub fn toggle_transform_mode(&mut self) {
        if !self.objects.is_empty() {
            self.transform_mode = self.transform_mode.toggled();
        }
    }

    fn sync_pivot_to_bounds(&mut self, doc: &Document) {
        if let Some(bb) = self.bounding_box(doc) {
            self.pivot = bb.center();
        } else {
            self.pivot = Point2D::ZERO;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{NodePayload, SceneObject};
    use crate::geometry::{CornerStyle, ParametricShape};
    use crate::math::Affine2D;

    #[test]
    fn dual_click_transform_mode_and_pivot() {
        let mut doc = Document::new_a4("Sel");
        let id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Box",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 40.0,
                        height: 20.0,
                        corner_radii: [0.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_transform(Affine2D::translate(10.0, 20.0)),
                None,
            )
            .unwrap();

        let mut sel = SelectionState::new();
        // First click -> ScaleAndMove, pivot centered at (30, 30)
        sel.click_object(&doc, id, false);
        assert_eq!(sel.transform_mode, TransformMode::ScaleAndMove);
        assert_eq!(sel.pivot, Point2D::new(30.0, 30.0));

        // Second click on same object -> RotateAndSkew
        sel.click_object(&doc, id, false);
        assert_eq!(sel.transform_mode, TransformMode::RotateAndSkew);

        // Move pivot manually
        sel.set_pivot(Point2D::new(10.0, 20.0));
        assert!(sel.custom_pivot);
        assert_eq!(sel.pivot, Point2D::new(10.0, 20.0));

        sel.reset_pivot(&doc);
        assert!(!sel.custom_pivot);
        assert_eq!(sel.pivot, Point2D::new(30.0, 30.0));
    }
}
