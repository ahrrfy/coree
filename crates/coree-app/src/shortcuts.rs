// SPDX-License-Identifier: MIT OR Apache-2.0

//! Switchable keyboard shortcut profiles:
//! - **`ClassicStudio`** (default muscle memory: `Space` toggle Pick, `F2`/`Z` Zoom, `F3` Zoom Out,
//!   `F4` Zoom to All, `Shift+F4` Zoom to Page, `H` Pan, `F5` Freehand, `F6` Rectangle,
//!   `F7` Ellipse, `Y` Polygon, `A` Spiral, `D` Graph Paper, `F8` Artistic Text, `F10` Shape/Node,
//!   `G` Interactive Fill, `M` Mesh Fill, `F12` Outline Pen, `Ctrl+D` Duplicate, `Ctrl+G`/`Ctrl+U`
//!   Group/Ungroup, `Ctrl+Q` Convert to Curves, `Ctrl+K` Break Apart, `Ctrl+L` Combine,
//!   `Ctrl+Z`/`Ctrl+Shift+Z` Undo/Redo, `Shift+PgUp`/`Shift+PgDn` Z-Order, `C/E/L/R/T/B/P` Align
//!   & Center to Page, `Ctrl+Space` / `Ctrl+Shift+P` Command Palette)
//! - **`StandardVector`** (universal vector-editor shortcut layout: `V` Pick, `N` Node, `R` Rect,
//!   `O` Ellipse, `T` Text, `P` Pen, `G` Fill, `Z` Zoom, `H` Pan, `Ctrl+K` Command Palette).

use coree_core::{AlignOp, ZOrderOp};
use egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};

/// Active keyboard shortcut profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ShortcutProfile {
    /// Classic studio muscle-memory shortcuts (default).
    #[default]
    ClassicStudio,
    /// Standard vector-tool shortcuts (V/N/R/O/T/P/G/Z).
    StandardVector,
}

impl ShortcutProfile {
    /// Display name for menus and Preferences.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ClassicStudio => "Classic Studio",
            Self::StandardVector => "Standard Vector",
        }
    }
}

/// High-level studio action triggered by a keyboard shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    // Tool switching
    TogglePickSpace,
    SelectPick,
    SelectShapeNode,
    SelectZoom,
    SelectOneShotZoom,
    SelectPan,
    SelectFreehand,
    SelectPen,
    SelectRectangle,
    SelectEllipse,
    SelectPolygon,
    SelectSpiral,
    SelectGraphPaper,
    SelectArtisticText,
    SelectInteractiveFill,
    SelectMeshFill,
    SelectEyedropper,
    OpenEditFill,
    FocusOutlinePen,

    // Viewport navigation
    ZoomIn,
    ZoomOut,
    ZoomToAll,
    ZoomToPage,
    ZoomToSelected,
    ZoomActual,
    ToggleFullScreenPreview,
    PrevPage,
    NextPage,

    // File & app actions
    NewDocument,
    OpenDocument,
    SaveDocument,
    SaveDocumentAs,
    ImportFile,
    ExportFile,
    ToggleCommandPalette,
    OpenFindReplace,
    OpenOptions,
    ToggleDockerObjects,
    ToggleDockerProperties,
    ToggleDockerLens,

    // Editing & Scene ops
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Duplicate,
    DuplicateInPlace,
    RepeatLastCommand,
    DeleteSelection,
    SelectAll,
    EscapeCancel,
    Group,
    Ungroup,
    ConvertToCurves,
    BreakApart,
    Combine,
    ZOrder(ZOrderOp),
    Align(AlignOp),
    Nudge {
        dx_steps: i32,
        dy_steps: i32,
        mult: i32,
    },
    MicroNudge {
        dx_steps: i32,
        dy_steps: i32,
    },
}

/// Resolves a key press + modifiers under `profile` into a [`ShortcutAction`].
///
/// When `text_focus` is `true`, unmodified single-letter/space/delete shortcuts are
/// suppressed so typing inside text fields works uninterrupted.
#[must_use]
pub fn resolve_shortcut(
    profile: ShortcutProfile,
    key: Key,
    mods: Modifiers,
    text_focus: bool,
) -> Option<ShortcutAction> {
    let ctrl = mods.ctrl || mods.command;
    let shift = mods.shift;
    let alt = mods.alt;

    // Always-available command-modified & function shortcuts
    if ctrl && !alt {
        match (key, shift) {
            (Key::Space, false) | (Key::P, true) => {
                return Some(ShortcutAction::ToggleCommandPalette);
            }
            (Key::N, false) => return Some(ShortcutAction::NewDocument),
            (Key::O, false) => return Some(ShortcutAction::OpenDocument),
            (Key::S, false) => return Some(ShortcutAction::SaveDocument),
            (Key::S, true) => return Some(ShortcutAction::SaveDocumentAs),
            (Key::I, false) => return Some(ShortcutAction::ImportFile),
            (Key::E, false) => return Some(ShortcutAction::ExportFile),
            (Key::J, false) => return Some(ShortcutAction::OpenOptions),
            (Key::Z, false) if !text_focus => return Some(ShortcutAction::Undo),
            (Key::Z, true) | (Key::Y, false) if !text_focus => return Some(ShortcutAction::Redo),
            (Key::X, false) if !text_focus => return Some(ShortcutAction::Cut),
            (Key::C, false) if !text_focus => return Some(ShortcutAction::Copy),
            (Key::V, false) if !text_focus => return Some(ShortcutAction::Paste),
            (Key::D, false) if !text_focus => return Some(ShortcutAction::Duplicate),
            (Key::R, false) if !text_focus => return Some(ShortcutAction::RepeatLastCommand),
            (Key::G, false) if !text_focus => return Some(ShortcutAction::Group),
            (Key::U, false) if !text_focus => return Some(ShortcutAction::Ungroup),
            (Key::G, true) if !text_focus && profile == ShortcutProfile::StandardVector => {
                return Some(ShortcutAction::Ungroup);
            }
            (Key::Q, false) if !text_focus => return Some(ShortcutAction::ConvertToCurves),
            (Key::K, false) => match profile {
                ShortcutProfile::ClassicStudio if !text_focus => {
                    return Some(ShortcutAction::BreakApart);
                }
                ShortcutProfile::StandardVector => {
                    return Some(ShortcutAction::ToggleCommandPalette);
                }
                _ => {}
            },
            (Key::L, false) if !text_focus => return Some(ShortcutAction::Combine),
            (Key::F, false) => return Some(ShortcutAction::OpenFindReplace),
            (Key::A, false) if !text_focus => return Some(ShortcutAction::SelectAll),
            (Key::Plus, _) | (Key::Equals, _) => return Some(ShortcutAction::ZoomIn),
            (Key::Minus, _) => return Some(ShortcutAction::ZoomOut),
            (Key::Num0, false) => return Some(ShortcutAction::ZoomActual),
            (Key::PageUp, false) => return Some(ShortcutAction::ZOrder(ZOrderOp::BringForward)),
            (Key::PageDown, false) => return Some(ShortcutAction::ZOrder(ZOrderOp::SendBackward)),
            // Ctrl+Arrow = Micro-Nudge (0.5x)
            (Key::ArrowLeft, false) if !text_focus => {
                return Some(ShortcutAction::MicroNudge {
                    dx_steps: -1,
                    dy_steps: 0,
                });
            }
            (Key::ArrowRight, false) if !text_focus => {
                return Some(ShortcutAction::MicroNudge {
                    dx_steps: 1,
                    dy_steps: 0,
                });
            }
            (Key::ArrowUp, false) if !text_focus => {
                return Some(ShortcutAction::MicroNudge {
                    dx_steps: 0,
                    dy_steps: -1,
                });
            }
            (Key::ArrowDown, false) if !text_focus => {
                return Some(ShortcutAction::MicroNudge {
                    dx_steps: 0,
                    dy_steps: 1,
                });
            }
            _ => {}
        }
    }

    // Alt-modified docker toggles
    if alt && !ctrl {
        match (key, shift) {
            (Key::O, true) => return Some(ShortcutAction::ToggleDockerObjects),
            (Key::Enter, false) => return Some(ShortcutAction::ToggleDockerProperties),
            (Key::F3, false) => return Some(ShortcutAction::ToggleDockerLens),
            _ => {}
        }
    }

    if key == Key::Escape {
        return Some(ShortcutAction::EscapeCancel);
    }

    // Suppress canvas single-key & arrow shortcuts while typing in text inputs
    if text_focus {
        return None;
    }

    // Z-Order Shift+PageUp / Shift+PageDown, page navigation, Numpad + duplicate, Full-screen F9, and Nudge
    if !ctrl && !alt {
        match (key, shift) {
            (Key::PageUp, true) => return Some(ShortcutAction::ZOrder(ZOrderOp::BringToFront)),
            (Key::PageDown, true) => return Some(ShortcutAction::ZOrder(ZOrderOp::SendToBack)),
            (Key::PageUp, false) => return Some(ShortcutAction::PrevPage),
            (Key::PageDown, false) => return Some(ShortcutAction::NextPage),
            (Key::F9, false) => return Some(ShortcutAction::ToggleFullScreenPreview),
            (Key::F2, true) => return Some(ShortcutAction::ZoomToSelected),
            (Key::Plus, false) => return Some(ShortcutAction::DuplicateInPlace),
            (Key::Delete, false) | (Key::Backspace, false) => {
                return Some(ShortcutAction::DeleteSelection);
            }
            (Key::ArrowLeft, _) => {
                return Some(ShortcutAction::Nudge {
                    dx_steps: -1,
                    dy_steps: 0,
                    mult: if shift { 2 } else { 1 },
                });
            }
            (Key::ArrowRight, _) => {
                return Some(ShortcutAction::Nudge {
                    dx_steps: 1,
                    dy_steps: 0,
                    mult: if shift { 2 } else { 1 },
                });
            }
            (Key::ArrowUp, _) => {
                return Some(ShortcutAction::Nudge {
                    dx_steps: 0,
                    dy_steps: -1,
                    mult: if shift { 2 } else { 1 },
                });
            }
            (Key::ArrowDown, _) => {
                return Some(ShortcutAction::Nudge {
                    dx_steps: 0,
                    dy_steps: 1,
                    mult: if shift { 2 } else { 1 },
                });
            }
            _ => {}
        }
    }

    // Profile-specific unmodified / Shift+Fn tool & alignment shortcuts
    if !ctrl && !alt {
        match profile {
            ShortcutProfile::ClassicStudio => match (key, shift) {
                (Key::Space, false) => Some(ShortcutAction::TogglePickSpace),
                (Key::F2, false) => Some(ShortcutAction::SelectOneShotZoom),
                (Key::Z, false) => Some(ShortcutAction::SelectZoom),
                (Key::F3, false) => Some(ShortcutAction::ZoomOut),
                (Key::F4, false) => Some(ShortcutAction::ZoomToAll),
                (Key::F4, true) => Some(ShortcutAction::ZoomToPage),
                (Key::H, false) => Some(ShortcutAction::SelectPan),
                (Key::F5, false) => Some(ShortcutAction::SelectFreehand),
                (Key::F6, false) => Some(ShortcutAction::SelectRectangle),
                (Key::F7, false) => Some(ShortcutAction::SelectEllipse),
                (Key::Y, false) => Some(ShortcutAction::SelectPolygon),
                (Key::A, false) => Some(ShortcutAction::SelectSpiral),
                (Key::D, false) => Some(ShortcutAction::SelectGraphPaper),
                (Key::F8, false) => Some(ShortcutAction::SelectArtisticText),
                (Key::F10, false) => Some(ShortcutAction::SelectShapeNode),
                (Key::G, false) => Some(ShortcutAction::SelectInteractiveFill),
                (Key::M, false) => Some(ShortcutAction::SelectMeshFill),
                (Key::F11, false) => Some(ShortcutAction::OpenEditFill),
                (Key::F12, false) => Some(ShortcutAction::FocusOutlinePen),
                (Key::I, false) => Some(ShortcutAction::SelectEyedropper),
                // Classic Studio single-key Alignment & Center to Page!
                (Key::C, false) => Some(ShortcutAction::Align(AlignOp::CenterH)),
                (Key::E, false) => Some(ShortcutAction::Align(AlignOp::CenterV)),
                (Key::L, false) => Some(ShortcutAction::Align(AlignOp::Left)),
                (Key::R, false) => Some(ShortcutAction::Align(AlignOp::Right)),
                (Key::T, false) => Some(ShortcutAction::Align(AlignOp::Top)),
                (Key::B, false) => Some(ShortcutAction::Align(AlignOp::Bottom)),
                (Key::P, false) => Some(ShortcutAction::Align(AlignOp::CenterPage)),
                _ => None,
            },
            ShortcutProfile::StandardVector => match (key, shift) {
                (Key::V, false) => Some(ShortcutAction::SelectPick),
                (Key::Space, false) => Some(ShortcutAction::TogglePickSpace),
                (Key::N, false) | (Key::A, false) => Some(ShortcutAction::SelectShapeNode),
                (Key::Z, false) => Some(ShortcutAction::SelectZoom),
                (Key::F2, false) => Some(ShortcutAction::SelectOneShotZoom),
                (Key::H, false) => Some(ShortcutAction::SelectPan),
                (Key::P, false) => Some(ShortcutAction::SelectPen),
                (Key::B, false) => Some(ShortcutAction::SelectFreehand),
                (Key::R, false) => Some(ShortcutAction::SelectRectangle),
                (Key::O, false) | (Key::E, false) => Some(ShortcutAction::SelectEllipse),
                (Key::Y, false) => Some(ShortcutAction::SelectPolygon),
                (Key::T, false) => Some(ShortcutAction::SelectArtisticText),
                (Key::G, false) => Some(ShortcutAction::SelectInteractiveFill),
                (Key::U, false) => Some(ShortcutAction::SelectMeshFill),
                (Key::I, false) => Some(ShortcutAction::SelectEyedropper),
                (Key::F4, false) => Some(ShortcutAction::ZoomToAll),
                (Key::F4, true) => Some(ShortcutAction::ZoomToPage),
                (Key::F11, false) => Some(ShortcutAction::OpenEditFill),
                (Key::F12, false) => Some(ShortcutAction::FocusOutlinePen),
                _ => None,
            },
        }
    } else {
        None
    }
}

/// Polls all key-press events from `ctx` for the current frame and resolves matching shortcuts.
#[must_use]
pub fn poll_egui_shortcuts(ctx: &egui::Context, profile: ShortcutProfile) -> Vec<ShortcutAction> {
    let text_focus = ctx.egui_wants_keyboard_input();
    let mut actions = Vec::new();
    ctx.input(|input| {
        for ev in &input.events {
            if let egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } = *ev
            {
                let effective_key = physical_key.unwrap_or(key);
                if let Some(act) = resolve_shortcut(profile, effective_key, modifiers, text_focus) {
                    actions.push(act);
                }
            }
        }
    });
    actions
}

/// Human-readable shortcut keycap label for menus, tooltips, and the Command Palette.
#[must_use]
pub const fn shortcut_label(profile: ShortcutProfile, action: ShortcutAction) -> &'static str {
    match (profile, action) {
        (_, ShortcutAction::TogglePickSpace) => "Space",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectPick) => "Space",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectPick) => "V",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectShapeNode) => "F10",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectShapeNode) => "N",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectZoom) => "Z",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectZoom) => "Z",
        (_, ShortcutAction::SelectOneShotZoom) => "F2",
        (_, ShortcutAction::SelectPan) => "H",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectFreehand) => "F5",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectFreehand) => "B",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectPen) => "Pen",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectPen) => "P",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectRectangle) => "F6",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectRectangle) => "R",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectEllipse) => "F7",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectEllipse) => "O",
        (_, ShortcutAction::SelectPolygon) => "Y",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectSpiral) => "A",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectSpiral) => "",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectGraphPaper) => "D",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectGraphPaper) => "",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectArtisticText) => "F8",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectArtisticText) => "T",
        (_, ShortcutAction::SelectInteractiveFill) => "G",
        (ShortcutProfile::ClassicStudio, ShortcutAction::SelectMeshFill) => "M",
        (ShortcutProfile::StandardVector, ShortcutAction::SelectMeshFill) => "U",
        (_, ShortcutAction::SelectEyedropper) => "I",
        (_, ShortcutAction::OpenEditFill) => "F11",
        (ShortcutProfile::ClassicStudio, ShortcutAction::FocusOutlinePen) => "F12",
        (ShortcutProfile::StandardVector, ShortcutAction::FocusOutlinePen) => "",
        (_, ShortcutAction::ZoomIn) => "Ctrl++",
        (ShortcutProfile::ClassicStudio, ShortcutAction::ZoomOut) => "F3",
        (ShortcutProfile::StandardVector, ShortcutAction::ZoomOut) => "Ctrl+-",
        (_, ShortcutAction::ZoomToAll) => "F4",
        (_, ShortcutAction::ZoomToPage) => "Shift+F4",
        (_, ShortcutAction::ZoomToSelected) => "Shift+F2",
        (_, ShortcutAction::ZoomActual) => "Ctrl+0",
        (_, ShortcutAction::ToggleFullScreenPreview) => "F9",
        (_, ShortcutAction::PrevPage) => "PgUp",
        (_, ShortcutAction::NextPage) => "PgDn",
        (_, ShortcutAction::NewDocument) => "Ctrl+N",
        (_, ShortcutAction::OpenDocument) => "Ctrl+O",
        (_, ShortcutAction::SaveDocument) => "Ctrl+S",
        (_, ShortcutAction::SaveDocumentAs) => "Ctrl+Shift+S",
        (_, ShortcutAction::ImportFile) => "Ctrl+I",
        (_, ShortcutAction::ExportFile) => "Ctrl+E",
        (ShortcutProfile::ClassicStudio, ShortcutAction::ToggleCommandPalette) => "Ctrl+Space",
        (ShortcutProfile::StandardVector, ShortcutAction::ToggleCommandPalette) => "Ctrl+K",
        (_, ShortcutAction::OpenFindReplace) => "Ctrl+F",
        (_, ShortcutAction::OpenOptions) => "Ctrl+J",
        (_, ShortcutAction::ToggleDockerObjects) => "Alt+Shift+O",
        (_, ShortcutAction::ToggleDockerProperties) => "Alt+Enter",
        (_, ShortcutAction::ToggleDockerLens) => "Alt+F3",
        (_, ShortcutAction::Undo) => "Ctrl+Z",
        (_, ShortcutAction::Redo) => "Ctrl+Shift+Z",
        (_, ShortcutAction::Cut) => "Ctrl+X",
        (_, ShortcutAction::Copy) => "Ctrl+C",
        (_, ShortcutAction::Paste) => "Ctrl+V",
        (_, ShortcutAction::Duplicate) => "Ctrl+D",
        (_, ShortcutAction::DuplicateInPlace) => "Num +",
        (_, ShortcutAction::RepeatLastCommand) => "Ctrl+R",
        (_, ShortcutAction::DeleteSelection) => "Del",
        (_, ShortcutAction::SelectAll) => "Ctrl+A",
        (_, ShortcutAction::EscapeCancel) => "Esc",
        (_, ShortcutAction::Group) => "Ctrl+G",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Ungroup) => "Ctrl+U",
        (ShortcutProfile::StandardVector, ShortcutAction::Ungroup) => "Ctrl+Shift+G",
        (_, ShortcutAction::ConvertToCurves) => "Ctrl+Q",
        (ShortcutProfile::ClassicStudio, ShortcutAction::BreakApart) => "Ctrl+K",
        (ShortcutProfile::StandardVector, ShortcutAction::BreakApart) => "",
        (_, ShortcutAction::Combine) => "Ctrl+L",
        (_, ShortcutAction::ZOrder(ZOrderOp::BringToFront)) => "Shift+PgUp",
        (_, ShortcutAction::ZOrder(ZOrderOp::SendToBack)) => "Shift+PgDn",
        (_, ShortcutAction::ZOrder(ZOrderOp::BringForward)) => "Ctrl+PgUp",
        (_, ShortcutAction::ZOrder(ZOrderOp::SendBackward)) => "Ctrl+PgDn",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Align(AlignOp::CenterH)) => "C",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Align(AlignOp::CenterV)) => "E",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Align(AlignOp::Left)) => "L",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Align(AlignOp::Right)) => "R",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Align(AlignOp::Top)) => "T",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Align(AlignOp::Bottom)) => "B",
        (ShortcutProfile::ClassicStudio, ShortcutAction::Align(AlignOp::CenterPage)) => "P",
        (ShortcutProfile::StandardVector, ShortcutAction::Align(_)) => "",
        (_, ShortcutAction::Nudge { .. }) => "Arrows",
        (_, ShortcutAction::MicroNudge { .. }) => "Ctrl+Arrows",
    }
}
