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
use std::collections::BTreeMap;

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
    PrintDocument,
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
    ConvertOutlineToObject,
    BreakApart,
    Combine,
    TogglePowerClipLock,
    MoveToDesktop,
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
            (Key::P, false) => return Some(ShortcutAction::PrintDocument),
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
            (Key::Q, true) if !text_focus => return Some(ShortcutAction::ConvertOutlineToObject),
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

/// Polls all key-press events from `ctx` using optional user shortcut overrides before
/// falling back to `profile` defaults.
#[must_use]
pub fn poll_egui_shortcuts_custom(
    ctx: &egui::Context,
    profile: ShortcutProfile,
    custom: &CustomShortcutMap,
) -> Vec<ShortcutAction> {
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
                if let Some(act) = resolve_shortcut_with_custom(
                    profile,
                    custom,
                    effective_key,
                    modifiers,
                    text_focus,
                ) {
                    actions.push(act);
                }
            }
        }
    });
    actions
}

/// Polls all key-press events from `ctx` for the current frame and resolves matching shortcuts.
#[must_use]
pub fn poll_egui_shortcuts(ctx: &egui::Context, profile: ShortcutProfile) -> Vec<ShortcutAction> {
    poll_egui_shortcuts_custom(ctx, profile, &CustomShortcutMap::default())
}

/// Returns the display shortcut string checking `custom` user overrides before `profile` defaults.
#[must_use]
pub fn shortcut_label_for(
    profile: ShortcutProfile,
    custom: &CustomShortcutMap,
    action: ShortcutAction,
) -> String {
    if let Some(combo) = custom.combo_for(action) {
        combo.to_owned()
    } else {
        shortcut_label(profile, action).to_owned()
    }
}

/// User-customizable shortcut override map (`action_id -> normalized_key_combo`)
/// persisted in local SQLite storage and portable `.coreews` workspace bundles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomShortcutMap {
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

impl CustomShortcutMap {
    /// Returns `true` when no custom key overrides are defined.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Assigns a custom key combo (e.g. `"Ctrl+Alt+K"`) to `action`, clearing any
    /// conflicting override on another action cleanly.
    pub fn set_binding(
        &mut self,
        action: ShortcutAction,
        combo_raw: &str,
    ) -> Result<String, String> {
        let Some(action_id) = action.action_id() else {
            return Err("this action cannot be remapped".to_owned());
        };
        let (ctrl, alt, shift, key) = parse_key_combo(combo_raw)?;
        let normalized = format_key_combo(ctrl, alt, shift, key);
        self.bindings.retain(|_, v| v != &normalized);
        self.bindings
            .insert(action_id.to_owned(), normalized.clone());
        Ok(normalized)
    }

    /// Removes a custom override for `action` so the profile default applies again.
    pub fn clear_binding(&mut self, action: ShortcutAction) {
        if let Some(id) = action.action_id() {
            self.bindings.remove(id);
        }
    }

    /// Returns the custom key combo bound to `action`, if any.
    #[must_use]
    pub fn combo_for(&self, action: ShortcutAction) -> Option<&str> {
        let id = action.action_id()?;
        self.bindings.get(id).map(String::as_str)
    }

    /// Resolves `(key, mods)` against custom overrides first. Returns `None` if no override matches.
    #[must_use]
    pub fn resolve_override(
        &self,
        key: Key,
        mods: Modifiers,
        text_focus: bool,
    ) -> Option<ShortcutAction> {
        if self.bindings.is_empty() {
            return None;
        }
        let ctrl = mods.ctrl || mods.command;
        let has_modifier = ctrl || mods.alt;
        if text_focus && !has_modifier && key != Key::Escape {
            return None;
        }
        let needle = format_key_combo(ctrl, mods.alt, mods.shift, key);
        for (id, combo) in &self.bindings {
            if combo == &needle {
                if let Some(action) = ShortcutAction::from_id(id) {
                    return Some(action);
                }
            }
        }
        None
    }
}

/// Resolves a key press against `custom` overrides first, then falls back to `profile`.
#[must_use]
pub fn resolve_shortcut_with_custom(
    profile: ShortcutProfile,
    custom: &CustomShortcutMap,
    key: Key,
    mods: Modifiers,
    text_focus: bool,
) -> Option<ShortcutAction> {
    if let Some(overridden) = custom.resolve_override(key, mods, text_focus) {
        return Some(overridden);
    }
    resolve_shortcut(profile, key, mods, text_focus)
}

impl ShortcutAction {
    /// Remappable actions exposed in `Options -> Shortcuts` and `.coreews` exports.
    pub const REMAPPABLE_ACTIONS: &'static [(ShortcutAction, &'static str)] = &[
        (ShortcutAction::NewDocument, "File: New Document"),
        (ShortcutAction::OpenDocument, "File: Open Document"),
        (ShortcutAction::SaveDocument, "File: Save"),
        (ShortcutAction::SaveDocumentAs, "File: Save As"),
        (ShortcutAction::ImportFile, "File: Import"),
        (ShortcutAction::ExportFile, "File: Export"),
        (ShortcutAction::PrintDocument, "File: Print"),
        (ShortcutAction::Undo, "Edit: Undo"),
        (ShortcutAction::Redo, "Edit: Redo"),
        (ShortcutAction::Cut, "Edit: Cut"),
        (ShortcutAction::Copy, "Edit: Copy"),
        (ShortcutAction::Paste, "Edit: Paste"),
        (ShortcutAction::Duplicate, "Edit: Duplicate"),
        (ShortcutAction::DuplicateInPlace, "Edit: Duplicate in Place"),
        (
            ShortcutAction::RepeatLastCommand,
            "Edit: Repeat Last Command",
        ),
        (ShortcutAction::DeleteSelection, "Edit: Delete"),
        (ShortcutAction::SelectAll, "Edit: Select All"),
        (ShortcutAction::Group, "Object: Group"),
        (ShortcutAction::Ungroup, "Object: Ungroup"),
        (ShortcutAction::ConvertToCurves, "Object: Convert to Curves"),
        (
            ShortcutAction::ConvertOutlineToObject,
            "Object: Convert Outline to Object",
        ),
        (ShortcutAction::BreakApart, "Object: Break Apart"),
        (ShortcutAction::Combine, "Object: Combine"),
        (
            ShortcutAction::TogglePowerClipLock,
            "Object: PowerClip Lock Contents",
        ),
        (
            ShortcutAction::MoveToDesktop,
            "Object: Move to Desktop Layer",
        ),
        (ShortcutAction::OpenFindReplace, "Edit: Find & Replace"),
        (ShortcutAction::OpenOptions, "Tools: Options"),
        (
            ShortcutAction::ToggleCommandPalette,
            "Window: Command Palette",
        ),
        (
            ShortcutAction::ToggleDockerObjects,
            "Window: Objects Docker",
        ),
        (
            ShortcutAction::ToggleDockerProperties,
            "Window: Properties Docker",
        ),
        (ShortcutAction::ToggleDockerLens, "Window: Lens Docker"),
        (ShortcutAction::OpenEditFill, "Fill: Edit Fill Dialog"),
        (
            ShortcutAction::FocusOutlinePen,
            "Outline: Outline Pen Dialog",
        ),
        (ShortcutAction::ZoomToAll, "View: Zoom to All"),
        (ShortcutAction::ZoomToPage, "View: Zoom to Page"),
        (ShortcutAction::ZoomToSelected, "View: Zoom to Selected"),
        (
            ShortcutAction::ToggleFullScreenPreview,
            "View: Full-Screen Preview",
        ),
        (ShortcutAction::SelectPick, "Tool: Pick"),
        (ShortcutAction::SelectShapeNode, "Tool: Shape / Node"),
        (ShortcutAction::SelectRectangle, "Tool: Rectangle"),
        (ShortcutAction::SelectEllipse, "Tool: Ellipse"),
        (ShortcutAction::SelectArtisticText, "Tool: Artistic Text"),
        (
            ShortcutAction::SelectInteractiveFill,
            "Tool: Interactive Fill",
        ),
    ];

    /// Stable identifier used in `shortcuts_json` and `.coreews` exports.
    #[must_use]
    pub const fn action_id(self) -> Option<&'static str> {
        Some(match self {
            Self::NewDocument => "file.new",
            Self::OpenDocument => "file.open",
            Self::SaveDocument => "file.save",
            Self::SaveDocumentAs => "file.save_as",
            Self::ImportFile => "file.import",
            Self::ExportFile => "file.export",
            Self::PrintDocument => "file.print",
            Self::Undo => "edit.undo",
            Self::Redo => "edit.redo",
            Self::Cut => "edit.cut",
            Self::Copy => "edit.copy",
            Self::Paste => "edit.paste",
            Self::Duplicate => "edit.duplicate",
            Self::DuplicateInPlace => "edit.duplicate_in_place",
            Self::RepeatLastCommand => "edit.repeat",
            Self::DeleteSelection => "edit.delete",
            Self::SelectAll => "edit.select_all",
            Self::Group => "object.group",
            Self::Ungroup => "object.ungroup",
            Self::ConvertToCurves => "object.convert_to_curves",
            Self::ConvertOutlineToObject => "object.convert_outline_to_object",
            Self::BreakApart => "object.break_apart",
            Self::Combine => "object.combine",
            Self::TogglePowerClipLock => "object.powerclip_lock",
            Self::MoveToDesktop => "object.move_to_desktop",
            Self::OpenFindReplace => "edit.find_replace",
            Self::OpenOptions => "tools.options",
            Self::ToggleCommandPalette => "window.command_palette",
            Self::ToggleDockerObjects => "window.docker_objects",
            Self::ToggleDockerProperties => "window.docker_properties",
            Self::ToggleDockerLens => "window.docker_lens",
            Self::OpenEditFill => "fill.edit",
            Self::FocusOutlinePen => "outline.pen",
            Self::ZoomToAll => "view.zoom_all",
            Self::ZoomToPage => "view.zoom_page",
            Self::ZoomToSelected => "view.zoom_selected",
            Self::ToggleFullScreenPreview => "view.fullscreen",
            Self::SelectPick => "tool.pick",
            Self::SelectShapeNode => "tool.shape",
            Self::SelectRectangle => "tool.rectangle",
            Self::SelectEllipse => "tool.ellipse",
            Self::SelectArtisticText => "tool.text",
            Self::SelectInteractiveFill => "tool.interactive_fill",
            _ => return None,
        })
    }

    /// Parses a persisted action identifier back into a [`ShortcutAction`].
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::REMAPPABLE_ACTIONS
            .iter()
            .find_map(|&(act, _)| (act.action_id() == Some(id)).then_some(act))
    }
}

/// Parses a human-readable key combo such as `"Ctrl+Shift+K"` or `"Alt+Enter"`.
pub fn parse_key_combo(raw: &str) -> Result<(bool, bool, bool, Key), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("shortcut key combination cannot be empty".to_owned());
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut main_key: Option<Key> = None;

    for token_raw in trimmed.split('+') {
        let token = if token_raw.is_empty() {
            "+"
        } else {
            token_raw.trim()
        };
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" | "cmd" | "command" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            _ => {
                if main_key.is_some() {
                    return Err(format!("multiple main keys in '{trimmed}'"));
                }
                main_key = Some(parse_single_key(&lower)?);
            }
        }
    }

    let key = main_key.ok_or_else(|| format!("missing main key in '{trimmed}'"))?;
    Ok((ctrl, alt, shift, key))
}

/// Formats `(ctrl, alt, shift, key)` in canonical display order (`Ctrl+Alt+Shift+Key`).
#[must_use]
pub fn format_key_combo(ctrl: bool, alt: bool, shift: bool, key: Key) -> String {
    let mut parts = Vec::with_capacity(4);
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    parts.push(key_display_name(key));
    parts.join("+")
}

fn parse_single_key(lower: &str) -> Result<Key, String> {
    Ok(match lower {
        "a" => Key::A,
        "b" => Key::B,
        "c" => Key::C,
        "d" => Key::D,
        "e" => Key::E,
        "f" => Key::F,
        "g" => Key::G,
        "h" => Key::H,
        "i" => Key::I,
        "j" => Key::J,
        "k" => Key::K,
        "l" => Key::L,
        "m" => Key::M,
        "n" => Key::N,
        "o" => Key::O,
        "p" => Key::P,
        "q" => Key::Q,
        "r" => Key::R,
        "s" => Key::S,
        "t" => Key::T,
        "u" => Key::U,
        "v" => Key::V,
        "w" => Key::W,
        "x" => Key::X,
        "y" => Key::Y,
        "z" => Key::Z,
        "0" | "num0" => Key::Num0,
        "1" | "num1" => Key::Num1,
        "2" | "num2" => Key::Num2,
        "3" | "num3" => Key::Num3,
        "4" | "num4" => Key::Num4,
        "5" | "num5" => Key::Num5,
        "6" | "num6" => Key::Num6,
        "7" | "num7" => Key::Num7,
        "8" | "num8" => Key::Num8,
        "9" | "num9" => Key::Num9,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "space" => Key::Space,
        "enter" | "return" => Key::Enter,
        "esc" | "escape" => Key::Escape,
        "tab" => Key::Tab,
        "del" | "delete" => Key::Delete,
        "backspace" => Key::Backspace,
        "pgup" | "pageup" => Key::PageUp,
        "pgdn" | "pagedown" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "insert" => Key::Insert,
        "left" | "arrowleft" => Key::ArrowLeft,
        "right" | "arrowright" => Key::ArrowRight,
        "up" | "arrowup" => Key::ArrowUp,
        "down" | "arrowdown" => Key::ArrowDown,
        "+" | "plus" => Key::Plus,
        "-" | "minus" => Key::Minus,
        "=" | "equals" => Key::Equals,
        other => return Err(format!("unknown key '{other}'")),
    })
}

const fn key_display_name(key: Key) -> &'static str {
    match key {
        Key::A => "A",
        Key::B => "B",
        Key::C => "C",
        Key::D => "D",
        Key::E => "E",
        Key::F => "F",
        Key::G => "G",
        Key::H => "H",
        Key::I => "I",
        Key::J => "J",
        Key::K => "K",
        Key::L => "L",
        Key::M => "M",
        Key::N => "N",
        Key::O => "O",
        Key::P => "P",
        Key::Q => "Q",
        Key::R => "R",
        Key::S => "S",
        Key::T => "T",
        Key::U => "U",
        Key::V => "V",
        Key::W => "W",
        Key::X => "X",
        Key::Y => "Y",
        Key::Z => "Z",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        Key::Space => "Space",
        Key::Enter => "Enter",
        Key::Escape => "Esc",
        Key::Tab => "Tab",
        Key::Delete => "Del",
        Key::Backspace => "Backspace",
        Key::PageUp => "PgUp",
        Key::PageDown => "PgDn",
        Key::Home => "Home",
        Key::End => "End",
        Key::Insert => "Ins",
        Key::ArrowLeft => "Left",
        Key::ArrowRight => "Right",
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        Key::Plus => "Plus",
        Key::Minus => "-",
        Key::Equals => "=",
        _ => "Key",
    }
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
        (_, ShortcutAction::PrintDocument) => "Ctrl+P",
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
        (_, ShortcutAction::ConvertOutlineToObject) => "Ctrl+Shift+Q",
        (ShortcutProfile::ClassicStudio, ShortcutAction::BreakApart) => "Ctrl+K",
        (ShortcutProfile::StandardVector, ShortcutAction::BreakApart) => "",
        (_, ShortcutAction::Combine) => "Ctrl+L",
        (_, ShortcutAction::TogglePowerClipLock) => "",
        (_, ShortcutAction::MoveToDesktop) => "",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_shortcut_map_overrides_and_json_roundtrip_work() {
        let mut custom = CustomShortcutMap::default();
        let norm = custom
            .set_binding(ShortcutAction::TogglePowerClipLock, "shift+alt+l")
            .expect("valid combo");
        assert_eq!(norm, "Alt+Shift+L");
        assert_eq!(
            shortcut_label_for(
                ShortcutProfile::ClassicStudio,
                &custom,
                ShortcutAction::TogglePowerClipLock
            ),
            "Alt+Shift+L"
        );

        let mods = Modifiers {
            alt: true,
            shift: true,
            ctrl: false,
            mac_cmd: false,
            command: false,
        };
        assert_eq!(
            resolve_shortcut_with_custom(
                ShortcutProfile::ClassicStudio,
                &custom,
                Key::L,
                mods,
                false
            ),
            Some(ShortcutAction::TogglePowerClipLock)
        );

        // Ctrl+P prints without clobbering unmodified P (Align CenterPage)
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            resolve_shortcut(ShortcutProfile::ClassicStudio, Key::P, ctrl, false),
            Some(ShortcutAction::PrintDocument)
        );
        assert_eq!(
            resolve_shortcut(
                ShortcutProfile::ClassicStudio,
                Key::P,
                Modifiers::NONE,
                false
            ),
            Some(ShortcutAction::Align(AlignOp::CenterPage))
        );

        let json = serde_json::to_string(&custom).unwrap();
        let decoded: CustomShortcutMap = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, custom);
    }
}
