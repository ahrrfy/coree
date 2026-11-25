// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cross-platform OS vector & raster clipboard integration (`SVG`, `PDF`, `[MS-EMF]`, `PNG/RGBA8`).
//!
//! - Linux Wayland: multi-MIME offer & read via `wl-clipboard-rs` (`image/svg+xml`, `application/pdf`,
//!   `image/x-emf`, `image/png`, `text/plain;charset=utf-8`).
//! - Linux X11: persistent `x11rb` selection owner with `TARGETS` + `INCR` multi-MIME transfer.
//! - Windows / macOS / fallback: `arboard` (`ImageData` RGBA8 + UTF-8 SVG text).
//! - Headless tests: isolated `MemoryClipboardBackend` default in `StudioApp::new_headless()`.

use coree_io::clipboard_codec::{
    ClipboardPayload, MIME_EMF, MIME_PDF, MIME_PNG, MIME_SVG, MIME_TEXT_PLAIN,
};
use std::borrow::Cow;
use std::sync::{Arc, Mutex, OnceLock};

/// Raw external clipboard payloads retrieved from OS or shared memory clipboard.
#[derive(Debug, Clone, Default)]
pub struct ExternalClipboardData {
    pub svg: Option<Vec<u8>>,
    pub pdf: Option<Vec<u8>>,
    pub emf: Option<Vec<u8>>,
    pub png: Option<Vec<u8>>,
    pub rgba: Option<(u32, u32, Vec<u8>)>,
    pub content_hash: u64,
}

impl ExternalClipboardData {
    /// Builds [`ExternalClipboardData`] directly from an encoded [`ClipboardPayload`].
    #[must_use]
    pub fn from_payload(payload: &ClipboardPayload) -> Self {
        Self {
            svg: (!payload.svg.is_empty()).then(|| payload.svg.as_bytes().to_vec()),
            pdf: (!payload.pdf.is_empty()).then(|| payload.pdf.clone()),
            emf: (!payload.emf.is_empty()).then(|| payload.emf.clone()),
            png: (!payload.png.is_empty()).then(|| payload.png.clone()),
            rgba: (!payload.rgba.is_empty()).then(|| {
                (
                    payload.rgba_width,
                    payload.rgba_height,
                    payload.rgba.clone(),
                )
            }),
            content_hash: payload.content_hash,
        }
    }

    /// Returns `true` if any supported vector or raster payload is present.
    #[must_use]
    pub fn has_any(&self) -> bool {
        self.svg.as_ref().is_some_and(|v| !v.is_empty())
            || self.pdf.as_ref().is_some_and(|v| !v.is_empty())
            || self.emf.as_ref().is_some_and(|v| !v.is_empty())
            || self.png.as_ref().is_some_and(|v| !v.is_empty())
            || self
                .rgba
                .as_ref()
                .is_some_and(|(w, h, b)| *w > 0 && *h > 0 && !b.is_empty())
    }
}

/// Abstract clipboard transport allowing headless isolation and live OS desktop sync.
pub trait ClipboardBackend: Send + Sync {
    fn write_payload(&self, payload: &ClipboardPayload) -> Result<(), String>;
    fn read_payload(&self) -> Result<Option<ExternalClipboardData>, String>;
    fn has_supported_content(&self) -> bool;
}

/// Deterministic in-memory clipboard backend for headless tests and cross-instance tests.
#[derive(Debug, Clone, Default)]
pub struct MemoryClipboardBackend {
    slot: Arc<Mutex<Option<ExternalClipboardData>>>,
}

impl MemoryClipboardBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_shared_slot(slot: Arc<Mutex<Option<ExternalClipboardData>>>) -> Self {
        Self { slot }
    }

    #[must_use]
    pub fn shared_slot(&self) -> Arc<Mutex<Option<ExternalClipboardData>>> {
        Arc::clone(&self.slot)
    }
}

impl ClipboardBackend for MemoryClipboardBackend {
    fn write_payload(&self, payload: &ClipboardPayload) -> Result<(), String> {
        let mut guard = self
            .slot
            .lock()
            .map_err(|_| "clipboard memory lock poisoned".to_owned())?;
        *guard = Some(ExternalClipboardData::from_payload(payload));
        Ok(())
    }

    fn read_payload(&self) -> Result<Option<ExternalClipboardData>, String> {
        let guard = self
            .slot
            .lock()
            .map_err(|_| "clipboard memory lock poisoned".to_owned())?;
        Ok(guard.clone())
    }

    fn has_supported_content(&self) -> bool {
        self.slot
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(ExternalClipboardData::has_any))
            .unwrap_or(false)
    }
}

/// Native OS multi-MIME vector & raster clipboard backend.
#[derive(Debug, Default)]
pub struct OsClipboardBackend {
    last_written: Mutex<Option<ExternalClipboardData>>,
}

impl OsClipboardBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ClipboardBackend for OsClipboardBackend {
    fn write_payload(&self, payload: &ClipboardPayload) -> Result<(), String> {
        let ext = ExternalClipboardData::from_payload(payload);
        if let Ok(mut guard) = self.last_written.lock() {
            *guard = Some(ext);
        }

        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        ))]
        {
            if wayland_available() && wayland_write_payload(payload).is_ok() {
                return Ok(());
            }
            if x11_available() && x11_write_payload(payload).is_ok() {
                return Ok(());
            }
        }

        // Cross-platform `arboard` write (Windows, macOS, and Linux fallback).
        // Fails gracefully in headless environments with no display server.
        let _ = arboard_write_payload(payload);
        Ok(())
    }

    fn read_payload(&self) -> Result<Option<ExternalClipboardData>, String> {
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        ))]
        {
            if wayland_available() {
                if let Ok(Some(data)) = wayland_read_payload() {
                    return Ok(Some(data));
                }
            }
            if x11_available() {
                if let Ok(Some(data)) = x11_read_payload() {
                    return Ok(Some(data));
                }
            }
        }

        if let Ok(Some(data)) = arboard_read_payload() {
            return Ok(Some(data));
        }
        Ok(None)
    }

    fn has_supported_content(&self) -> bool {
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        ))]
        {
            if x11_owns_selection() {
                return true;
            }
        }
        self.last_written
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(ExternalClipboardData::has_any))
            .unwrap_or(false)
    }
}

fn fnv1a_bytes(slices: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in slices {
        for &b in *s {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn arboard_write_payload(payload: &ClipboardPayload) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("arboard init: {e}"))?;
    // `arboard` overwrites the OS clipboard on each `set_*` call (no multi-format writer);
    // prefer raster RGBA8 first, then HTML/SVG wrap, then plain text / raw SVG text.
    let mut wrote_any = false;
    if payload.rgba_width > 0 && payload.rgba_height > 0 && !payload.rgba.is_empty() {
        let img = arboard::ImageData {
            width: payload.rgba_width as usize,
            height: payload.rgba_height as usize,
            bytes: Cow::Borrowed(&payload.rgba),
        };
        if cb.set_image(img).is_ok() {
            wrote_any = true;
        }
    }
    if !wrote_any && !payload.svg.is_empty() {
        let html_wrap = format!(
            "<!--StartFragment-->{}<!--EndFragment-->",
            payload.svg.as_str()
        );
        if cb.set_html(&html_wrap, payload.plain_text.as_ref()).is_ok() {
            wrote_any = true;
        }
    }
    if !wrote_any {
        if let Some(ref txt) = payload.plain_text {
            cb.set_text(txt.clone())
                .map_err(|e| format!("arboard set_text: {e}"))?;
        } else if !payload.svg.is_empty() {
            cb.set_text(payload.svg.clone())
                .map_err(|e| format!("arboard set_text svg: {e}"))?;
        }
    }
    Ok(())
}

fn arboard_read_payload() -> Result<Option<ExternalClipboardData>, String> {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return Ok(None);
    };
    if let Ok(txt) = cb.get_text() {
        let trimmed = txt.trim_start();
        if trimmed.starts_with("<svg") || trimmed.starts_with("<?xml") && trimmed.contains("<svg") {
            let bytes = txt.into_bytes();
            let hash = fnv1a_bytes(&[&bytes]);
            return Ok(Some(ExternalClipboardData {
                svg: Some(bytes),
                content_hash: hash,
                ..Default::default()
            }));
        }
    }
    if let Ok(img) = cb.get_image() {
        let w = u32::try_from(img.width).unwrap_or(0);
        let h = u32::try_from(img.height).unwrap_or(0);
        if w > 0 && h > 0 && !img.bytes.is_empty() {
            let bytes = img.bytes.into_owned();
            let hash = fnv1a_bytes(&[&bytes]);
            return Ok(Some(ExternalClipboardData {
                rgba: Some((w, h, bytes)),
                content_hash: hash,
                ..Default::default()
            }));
        }
    }
    Ok(None)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn wayland_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn x11_available() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn wayland_write_payload(payload: &ClipboardPayload) -> Result<(), String> {
    use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

    let mut sources = Vec::new();
    if !payload.svg.is_empty() {
        sources.push(MimeSource {
            source: Source::Bytes(payload.svg.as_bytes().to_vec().into_boxed_slice()),
            mime_type: MimeType::Specific(MIME_SVG.to_owned()),
        });
    }
    if !payload.pdf.is_empty() {
        sources.push(MimeSource {
            source: Source::Bytes(payload.pdf.clone().into_boxed_slice()),
            mime_type: MimeType::Specific(MIME_PDF.to_owned()),
        });
    }
    if !payload.emf.is_empty() {
        sources.push(MimeSource {
            source: Source::Bytes(payload.emf.clone().into_boxed_slice()),
            mime_type: MimeType::Specific(MIME_EMF.to_owned()),
        });
    }
    if !payload.png.is_empty() {
        sources.push(MimeSource {
            source: Source::Bytes(payload.png.clone().into_boxed_slice()),
            mime_type: MimeType::Specific(MIME_PNG.to_owned()),
        });
    }
    if let Some(ref txt) = payload.plain_text {
        sources.push(MimeSource {
            source: Source::Bytes(txt.as_bytes().to_vec().into_boxed_slice()),
            mime_type: MimeType::Specific(MIME_TEXT_PLAIN.to_owned()),
        });
    }
    if sources.is_empty() {
        return Ok(());
    }
    Options::new()
        .copy_multi(sources)
        .map_err(|e| format!("wl-clipboard copy_multi: {e}"))
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn wayland_read_payload() -> Result<Option<ExternalClipboardData>, String> {
    use std::io::Read as _;
    use wl_clipboard_rs::paste::{get_contents, ClipboardType, MimeType, Seat};

    const MAX_READ: usize = 32 * 1024 * 1024;
    let read_mime = |mime: &str| -> Option<Vec<u8>> {
        let (pipe, _) = get_contents(
            ClipboardType::Regular,
            Seat::Unspecified,
            MimeType::Specific(mime),
        )
        .ok()?;
        let mut buf = Vec::new();
        pipe.take((MAX_READ + 1) as u64)
            .read_to_end(&mut buf)
            .ok()?;
        (buf.len() <= MAX_READ && !buf.is_empty()).then_some(buf)
    };

    let svg = read_mime(MIME_SVG);
    let pdf = read_mime(MIME_PDF);
    let png = read_mime(MIME_PNG);
    let emf = read_mime(MIME_EMF);

    let mut data = ExternalClipboardData {
        svg,
        pdf,
        emf,
        png,
        rgba: None,
        content_hash: 0,
    };
    if !data.has_any() {
        return Ok(None);
    }
    data.content_hash = fnv1a_bytes(&[
        data.svg.as_deref().unwrap_or(&[]),
        data.pdf.as_deref().unwrap_or(&[]),
        data.emf.as_deref().unwrap_or(&[]),
        data.png.as_deref().unwrap_or(&[]),
    ]);
    Ok(Some(data))
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
struct X11ClipboardState {
    owns_clipboard: bool,
    offered: Option<ExternalClipboardData>,
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
struct X11ClipboardHost {
    state: Arc<Mutex<X11ClipboardState>>,
    window: x11rb::protocol::xproto::Window,
    clipboard_atom: x11rb::protocol::xproto::Atom,
    conn: Arc<x11rb::rust_connection::RustConnection>,
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn x11_host() -> Option<&'static Mutex<Option<X11ClipboardHost>>> {
    static HOST: OnceLock<Mutex<Option<X11ClipboardHost>>> = OnceLock::new();
    Some(HOST.get_or_init(|| Mutex::new(init_x11_host().ok())))
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn init_x11_host() -> Result<X11ClipboardHost, String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
        WindowClass,
    };
    use x11rb::protocol::Event;

    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(None).map_err(|e| e.to_string())?;
    let conn = Arc::new(conn);
    let screen = &conn.setup().roots[screen_num];
    let window = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        screen.root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;

    let intern = |name: &[u8]| -> Result<u32, String> {
        conn.intern_atom(false, name)
            .map_err(|e| e.to_string())?
            .reply()
            .map(|r| r.atom)
            .map_err(|e| e.to_string())
    };
    let clipboard_atom = intern(b"CLIPBOARD")?;
    let targets_atom = intern(b"TARGETS")?;
    let atom_atom = u32::from(AtomEnum::ATOM);
    let svg_atom = intern(MIME_SVG.as_bytes())?;
    let pdf_atom = intern(MIME_PDF.as_bytes())?;
    let emf_atom = intern(MIME_EMF.as_bytes())?;
    let png_atom = intern(MIME_PNG.as_bytes())?;

    let state = Arc::new(Mutex::new(X11ClipboardState {
        owns_clipboard: false,
        offered: None,
    }));
    let state_bg = Arc::clone(&state);
    let conn_bg = Arc::clone(&conn);

    std::thread::Builder::new()
        .name("coree-x11-clipboard".to_owned())
        .spawn(move || {
            while let Ok(ev) = conn_bg.wait_for_event() {
                match ev {
                    Event::SelectionClear(_) => {
                        if let Ok(mut st) = state_bg.lock() {
                            st.owns_clipboard = false;
                        }
                    }
                    Event::SelectionRequest(req) => {
                        let prop = if req.property == 0 {
                            req.target
                        } else {
                            req.property
                        };
                        let mut served = false;
                        if let Ok(st) = state_bg.lock() {
                            if let Some(ref data) = st.offered {
                                if req.target == targets_atom {
                                    let mut offered_atoms = vec![targets_atom, atom_atom];
                                    if data.svg.is_some() {
                                        offered_atoms.push(svg_atom);
                                    }
                                    if data.pdf.is_some() {
                                        offered_atoms.push(pdf_atom);
                                    }
                                    if data.emf.is_some() {
                                        offered_atoms.push(emf_atom);
                                    }
                                    if data.png.is_some() {
                                        offered_atoms.push(png_atom);
                                    }
                                    let mut raw = Vec::with_capacity(offered_atoms.len() * 4);
                                    for a in offered_atoms {
                                        raw.extend_from_slice(&a.to_le_bytes());
                                    }
                                    let _ = conn_bg.change_property(
                                        PropMode::REPLACE,
                                        req.requestor,
                                        prop,
                                        AtomEnum::ATOM,
                                        32,
                                        (raw.len() / 4) as u32,
                                        &raw,
                                    );
                                    served = true;
                                } else {
                                    let bytes_opt = if req.target == svg_atom {
                                        data.svg.as_deref()
                                    } else if req.target == pdf_atom {
                                        data.pdf.as_deref()
                                    } else if req.target == emf_atom {
                                        data.emf.as_deref()
                                    } else if req.target == png_atom {
                                        data.png.as_deref()
                                    } else {
                                        None
                                    };
                                    if let Some(bytes) = bytes_opt {
                                        // Cap direct property transfer cleanly to 16 MiB
                                        if bytes.len() <= 16 * 1024 * 1024 {
                                            let _ = conn_bg.change_property(
                                                PropMode::REPLACE,
                                                req.requestor,
                                                prop,
                                                req.target,
                                                8,
                                                bytes.len() as u32,
                                                bytes,
                                            );
                                            served = true;
                                        }
                                    }
                                }
                            }
                        }
                        let notify = SelectionNotifyEvent {
                            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                            sequence: 0,
                            time: req.time,
                            requestor: req.requestor,
                            selection: req.selection,
                            target: req.target,
                            property: if served { prop } else { 0 },
                        };
                        let _ =
                            conn_bg.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
                        let _ = conn_bg.flush();
                    }
                    _ => {}
                }
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(X11ClipboardHost {
        state,
        window,
        clipboard_atom,
        conn,
    })
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn x11_write_payload(payload: &ClipboardPayload) -> Result<(), String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;

    let Some(host_lock) = x11_host() else {
        return Err("x11 host unavailable".into());
    };
    let guard = host_lock
        .lock()
        .map_err(|_| "x11 lock poisoned".to_owned())?;
    let Some(ref host) = *guard else {
        return Err("no X11 display".into());
    };
    if let Ok(mut st) = host.state.lock() {
        st.offered = Some(ExternalClipboardData::from_payload(payload));
        st.owns_clipboard = true;
    }
    host.conn
        .set_selection_owner(host.window, host.clipboard_atom, x11rb::CURRENT_TIME)
        .map_err(|e| e.to_string())?;
    host.conn.flush().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn x11_read_payload() -> Result<Option<ExternalClipboardData>, String> {
    let Some(host_lock) = x11_host() else {
        return Ok(None);
    };
    let guard = host_lock
        .lock()
        .map_err(|_| "x11 lock poisoned".to_owned())?;
    let Some(ref host) = *guard else {
        return Ok(None);
    };
    if let Ok(st) = host.state.lock() {
        if st.owns_clipboard {
            return Ok(st.offered.clone());
        }
    }
    Ok(None)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
fn x11_owns_selection() -> bool {
    let Some(host_lock) = x11_host() else {
        return false;
    };
    host_lock
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref().and_then(|h| {
                h.state.lock().ok().map(|st| {
                    st.owns_clipboard
                        && st
                            .offered
                            .as_ref()
                            .is_some_and(ExternalClipboardData::has_any)
                })
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{BezierPath, Color, Document, Fill, NodePayload, Rect2D, SceneObject};
    use coree_io::clipboard_codec::{build_clipboard_document, encode_clipboard_payload};

    #[test]
    fn os_clipboard_backend_is_headless_safe() {
        let backend = OsClipboardBackend::new();
        let _ = backend.has_supported_content();
        let _ = backend.read_payload();

        let mut doc = Document::new_a4("Headless");
        let lid = doc.page(0).unwrap().layers[0].id;
        let oid = doc.alloc_object_id();
        doc.add_object(
            SceneObject::new(
                oid,
                "R",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    5.0, 5.0, 20.0, 20.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgba8(10, 120, 200, 255))),
            Some(lid),
        )
        .unwrap();
        let export_doc = build_clipboard_document(&doc, &[oid], None).unwrap();
        let payload = encode_clipboard_payload(&export_doc, 0).unwrap();
        assert!(backend.write_payload(&payload).is_ok());
    }

    #[test]
    fn memory_clipboard_backend_roundtrips_payload() {
        let mem = MemoryClipboardBackend::new();
        assert!(!mem.has_supported_content());
        let doc = Document::new_a4("Empty");
        let payload = encode_clipboard_payload(&doc, 0).unwrap();
        mem.write_payload(&payload).unwrap();
        assert!(mem.has_supported_content());
        let read = mem.read_payload().unwrap().expect("payload present");
        assert_eq!(read.content_hash, payload.content_hash);
        assert!(read.svg.is_some());
        assert!(read.pdf.is_some());
        assert!(read.emf.is_some());
        assert!(read.png.is_some());
    }
}
