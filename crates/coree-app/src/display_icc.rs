// SPDX-License-Identifier: MIT OR Apache-2.0

//! OS Monitor / Display ICC profile discovery (`IMP-03`):
//! - `COREE_DISPLAY_ICC` environment override
//! - Linux: Wayland `wp_color_manager_v1`, X11 `_ICC_PROFILE`, and `colormgr` CLI fallback
//! - Windows: ICM/WCS registry/spool query (zero `unsafe`)

use coree_core::{IccColorSpace, IccProfile, MAX_ICC_PROFILE_BYTES};
use std::path::Path;

/// Loaded OS monitor ICC profile metadata and validated raw ICC bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayIccInfo {
    pub bytes: Vec<u8>,
    pub description: String,
    pub source: String,
}

/// Cross-platform OS Display ICC profile provider (`IMP-03`).
pub trait DisplayIccProvider: Send + Sync {
    /// Queries and validates the active display's RGB ICC profile, if available.
    fn load_display_icc(&self) -> Option<DisplayIccInfo>;
}

/// Null provider used in deterministic headless mode (`StudioApp::new_headless()`).
#[derive(Debug, Default)]
pub struct NullDisplayIccProvider;

impl DisplayIccProvider for NullDisplayIccProvider {
    fn load_display_icc(&self) -> Option<DisplayIccInfo> {
        load_display_icc_from_env()
    }
}

/// In-memory display ICC provider for unit & integration tests.
#[derive(Debug, Clone)]
pub struct MemoryDisplayIccProvider {
    info: Option<DisplayIccInfo>,
}

impl MemoryDisplayIccProvider {
    #[must_use]
    pub fn new(bytes: Vec<u8>, description: impl Into<String>, source: impl Into<String>) -> Self {
        let info = validate_display_icc_bytes(bytes, description.into(), source.into());
        Self { info }
    }
}

impl DisplayIccProvider for MemoryDisplayIccProvider {
    fn load_display_icc(&self) -> Option<DisplayIccInfo> {
        self.info.clone()
    }
}

/// Production OS Display ICC provider (`COREE_DISPLAY_ICC`, Wayland CM, X11 `_ICC_PROFILE`, Windows ICM).
#[derive(Debug, Default)]
pub struct OsDisplayIccProvider;

impl DisplayIccProvider for OsDisplayIccProvider {
    fn load_display_icc(&self) -> Option<DisplayIccInfo> {
        if let Some(env_prof) = load_display_icc_from_env() {
            return Some(env_prof);
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(prof) = load_linux_wayland_display_icc() {
                return Some(prof);
            }
            if let Some(prof) = load_linux_x11_display_icc() {
                return Some(prof);
            }
            if let Some(prof) = load_linux_colord_display_icc() {
                return Some(prof);
            }
        }
        #[cfg(windows)]
        {
            if let Some(prof) = load_windows_display_icc() {
                return Some(prof);
            }
        }
        None
    }
}

/// Validates raw ICC bytes (bounded size, valid header, RGB color space).
#[must_use]
pub fn validate_display_icc_bytes(
    bytes: Vec<u8>,
    fallback_desc: String,
    source: String,
) -> Option<DisplayIccInfo> {
    if bytes.is_empty() || bytes.len() > MAX_ICC_PROFILE_BYTES {
        return None;
    }
    let prof = IccProfile::from_bytes(&bytes).ok()?;
    if prof.header.color_space != IccColorSpace::Rgb {
        return None;
    }
    let description = if prof.description.trim().is_empty() {
        fallback_desc
    } else {
        prof.description.clone()
    };
    Some(DisplayIccInfo {
        bytes,
        description,
        source,
    })
}

/// Loads & validates a display ICC profile from a filesystem path.
#[must_use]
pub fn load_display_icc_from_path(path: &Path, source: &str) -> Option<DisplayIccInfo> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_ICC_PROFILE_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let fallback = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Display ICC")
        .to_owned();
    validate_display_icc_bytes(bytes, fallback, source.to_owned())
}

fn load_display_icc_from_env() -> Option<DisplayIccInfo> {
    let raw = std::env::var_os("COREE_DISPLAY_ICC")?;
    if raw.is_empty() {
        return None;
    }
    load_display_icc_from_path(Path::new(&raw), "COREE_DISPLAY_ICC")
}

#[cfg(target_os = "linux")]
fn load_linux_wayland_display_icc() -> Option<DisplayIccInfo> {
    use std::io::Read;
    use wayland_client::protocol::{wl_output, wl_registry};
    use wayland_client::{Connection, Dispatch, QueueHandle};
    use wayland_protocols::wp::color_management::v1::client::{
        wp_color_management_output_v1::WpColorManagementOutputV1,
        wp_color_manager_v1::WpColorManagerV1,
        wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
        wp_image_description_v1::{self, WpImageDescriptionV1},
    };

    std::env::var_os("WAYLAND_DISPLAY")?;
    let conn = Connection::connect_to_env().ok()?;

    #[derive(Default)]
    struct CmState {
        cm_name: Option<(u32, u32)>,
        output_name: Option<(u32, u32)>,
        icc_bytes: Option<Vec<u8>>,
        done: bool,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for CmState {
        fn event(
            state: &mut Self,
            _proxy: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            {
                if interface == "wp_color_manager_v1" && state.cm_name.is_none() {
                    state.cm_name = Some((name, version.min(1)));
                } else if interface == "wl_output" && state.output_name.is_none() {
                    state.output_name = Some((name, version.min(4)));
                }
            }
        }
    }

    impl Dispatch<wl_output::WlOutput, ()> for CmState {
        fn event(
            _: &mut Self,
            _: &wl_output::WlOutput,
            _: wl_output::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WpColorManagerV1, ()> for CmState {
        fn event(
            _: &mut Self,
            _: &WpColorManagerV1,
            _: wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WpColorManagementOutputV1, ()> for CmState {
        fn event(
            _: &mut Self,
            _: &WpColorManagementOutputV1,
            _: wayland_protocols::wp::color_management::v1::client::wp_color_management_output_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WpImageDescriptionV1, ()> for CmState {
        fn event(
            state: &mut Self,
            _proxy: &WpImageDescriptionV1,
            event: wp_image_description_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wp_image_description_v1::Event::Failed { .. } = event {
                state.done = true;
            }
        }
    }

    impl Dispatch<WpImageDescriptionInfoV1, ()> for CmState {
        fn event(
            state: &mut Self,
            _proxy: &WpImageDescriptionInfoV1,
            event: wp_image_description_info_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                wp_image_description_info_v1::Event::IccFile { icc, icc_size } => {
                    let sz = usize::try_from(icc_size).unwrap_or(0);
                    if sz > 0 && sz <= MAX_ICC_PROFILE_BYTES {
                        let mut file = std::fs::File::from(icc);
                        let mut buf = vec![0u8; sz];
                        if file.read_exact(&mut buf).is_ok() {
                            state.icc_bytes = Some(buf);
                        }
                    }
                }
                wp_image_description_info_v1::Event::Done => {
                    state.done = true;
                }
                _ => {}
            }
        }
    }

    let mut queue = conn.new_event_queue::<CmState>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut state = CmState::default();
    queue.roundtrip(&mut state).ok()?;

    let (cm_id, cm_ver) = state.cm_name?;
    let (out_id, out_ver) = state.output_name?;
    let cm: WpColorManagerV1 = registry.bind(cm_id, cm_ver, &qh, ());
    let output: wl_output::WlOutput = registry.bind(out_id, out_ver, &qh, ());
    let cm_out = cm.get_output(&output, &qh, ());
    let img_desc = cm_out.get_image_description(&qh, ());
    let _info = img_desc.get_information(&qh, ());
    let _ = queue.roundtrip(&mut state);
    if !state.done {
        let _ = queue.roundtrip(&mut state);
    }

    state.icc_bytes.and_then(|bytes| {
        validate_display_icc_bytes(
            bytes,
            "Wayland Color Management ICC".to_owned(),
            "Wayland wp_color_manager_v1".to_owned(),
        )
    })
}

#[cfg(target_os = "linux")]
fn load_linux_x11_display_icc() -> Option<DisplayIccInfo> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

    std::env::var_os("DISPLAY")?;
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let screen = conn.setup().roots.get(screen_num)?;
    let atom = conn
        .intern_atom(false, b"_ICC_PROFILE")
        .ok()?
        .reply()
        .ok()?
        .atom;
    if atom == 0 {
        return None;
    }
    let max_u32_words = (MAX_ICC_PROFILE_BYTES / 4) as u32;
    let reply = conn
        .get_property(
            false,
            screen.root,
            atom,
            AtomEnum::CARDINAL,
            0,
            max_u32_words,
        )
        .ok()?
        .reply()
        .ok()?;
    if reply.value.is_empty() {
        return None;
    }
    validate_display_icc_bytes(
        reply.value,
        "X11 _ICC_PROFILE".to_owned(),
        "X11 _ICC_PROFILE".to_owned(),
    )
}

#[cfg(target_os = "linux")]
fn load_linux_colord_display_icc() -> Option<DisplayIccInfo> {
    let out = std::process::Command::new("colormgr")
        .arg("get-devices")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut in_display = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Type:") {
            in_display = rest.trim().eq_ignore_ascii_case("display");
        } else if in_display {
            if let Some(rest) = trimmed.strip_prefix("Filename:") {
                let path = Path::new(rest.trim());
                if path.is_file() {
                    if let Some(info) = load_display_icc_from_path(path, "colord") {
                        return Some(info);
                    }
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn load_windows_display_icc() -> Option<DisplayIccInfo> {
    // Query Windows ICM default display profile association without `unsafe` FFI
    let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let color_dir = Path::new(&sys_root)
        .join("System32")
        .join("spool")
        .join("drivers")
        .join("color");

    if let Ok(out) = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows NT\CurrentVersion\ICM\ProfileAssociations\Display",
            "/s",
        ])
        .output()
    {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                for tok in line.split_whitespace() {
                    let lower = tok.to_ascii_lowercase();
                    if lower.ends_with(".icc") || lower.ends_with(".icm") {
                        let candidate = if Path::new(tok).is_absolute() {
                            std::path::PathBuf::from(tok)
                        } else {
                            color_dir.join(tok)
                        };
                        if let Some(info) =
                            load_display_icc_from_path(&candidate, "Windows WCS/ICM")
                        {
                            return Some(info);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::generate_minimal_display_p3_icc_v2;

    #[test]
    fn display_icc_provider_validates_and_rejects_invalid_bytes() {
        let p3 = generate_minimal_display_p3_icc_v2("Test Monitor P3");
        let mem = MemoryDisplayIccProvider::new(p3.clone(), "Fallback", "Memory");
        let loaded = mem.load_display_icc().expect("valid RGB ICC must load");
        assert_eq!(loaded.bytes, p3);
        assert!(loaded.description.contains("Test Monitor P3"));

        let invalid = MemoryDisplayIccProvider::new(vec![1, 2, 3, 4], "Bad", "Memory");
        assert!(invalid.load_display_icc().is_none());
    }
}
