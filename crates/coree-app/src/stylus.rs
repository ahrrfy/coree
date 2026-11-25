// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hardware stylus / pen tablet input providers (`zwp_tablet_v2` / `evdev` on Linux,
//! `WM_POINTER` / `egui::Event::Touch` on Windows, zero `unsafe` across all platforms).

use coree_core::StylusSample;
use std::sync::{Arc, Mutex};

/// Cross-platform hardware stylus input provider (`IMP-03`).
pub trait StylusProvider: Send {
    /// Samples the current frame's stylus state, merging any `egui::Event::Touch` force events.
    fn sample_for_frame(&mut self, egui_ctx: &egui::Context) -> StylusSample;

    /// Human-readable summary of detected tablet/stylus capabilities.
    fn device_summary(&self) -> String;

    /// Returns `true` when hardware tablet/stylus input or active touch-force is present.
    fn is_hardware_active(&self) -> bool;
}

/// No-op stylus provider for headless tests (`StudioApp::new_headless()`).
#[derive(Debug, Default)]
pub struct NullStylusProvider;

impl StylusProvider for NullStylusProvider {
    fn sample_for_frame(&mut self, egui_ctx: &egui::Context) -> StylusSample {
        extract_egui_touch_stylus(egui_ctx).unwrap_or(StylusSample::DEFAULT)
    }

    fn device_summary(&self) -> String {
        "Headless / Mouse".to_owned()
    }

    fn is_hardware_active(&self) -> bool {
        false
    }
}

/// Deterministic mock stylus provider for unit & integration tests.
#[derive(Debug, Clone)]
pub struct MockStylusProvider {
    sample: StylusSample,
    label: String,
}

impl MockStylusProvider {
    /// Creates a mock provider returning `sample` on every frame.
    #[must_use]
    pub fn new(sample: StylusSample, label: impl Into<String>) -> Self {
        Self {
            sample,
            label: label.into(),
        }
    }

    /// Updates the sample returned on subsequent frames.
    pub fn set_sample(&mut self, sample: StylusSample) {
        self.sample = sample;
    }
}

impl StylusProvider for MockStylusProvider {
    fn sample_for_frame(&mut self, egui_ctx: &egui::Context) -> StylusSample {
        extract_egui_touch_stylus(egui_ctx).unwrap_or(self.sample)
    }

    fn device_summary(&self) -> String {
        self.label.clone()
    }

    fn is_hardware_active(&self) -> bool {
        self.sample.has_pressure
    }
}

/// Extracts stylus/touch force from `egui::Event::Touch` events in the current frame.
#[must_use]
pub fn extract_egui_touch_stylus(egui_ctx: &egui::Context) -> Option<StylusSample> {
    egui_ctx.input(|input| {
        let mut best: Option<StylusSample> = None;
        for ev in &input.events {
            if let egui::Event::Touch {
                force: Some(f),
                phase,
                ..
            } = ev
            {
                if matches!(
                    phase,
                    egui::TouchPhase::Start | egui::TouchPhase::Move | egui::TouchPhase::End
                ) && f.is_finite()
                {
                    best = Some(StylusSample::with_pressure(*f));
                }
            }
        }
        best
    })
}

/// Constructs the default OS [`StylusProvider`] for desktop sessions.
#[must_use]
pub fn platform_stylus_provider() -> Box<dyn StylusProvider> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxStylusProvider::new())
    }
    #[cfg(windows)]
    {
        Box::new(WindowsStylusProvider::new())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Box::new(NullStylusProvider)
    }
}

// =============================================================================
// Linux Stylus Provider (`zwp_tablet_v2` discovery + safe `evdev` + `egui` force)
// =============================================================================

#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone)]
struct LinuxStylusShared {
    pressure: f32,
    has_pressure: bool,
    pressure_max: i32,
    tilt_x: f32,
    tilt_y: f32,
    twist_deg: f32,
    is_eraser: bool,
    pen_in_proximity: bool,
    wayland_tablet_detected: bool,
    evdev_device_names: Vec<String>,
}

/// Pure-safe Linux stylus provider layering Wayland `zwp_tablet_v2` discovery,
/// `/dev/input/event*` hardware `evdev` ABS/KEY packet reading, and `egui::Event::Touch`.
#[cfg(target_os = "linux")]
pub struct LinuxStylusProvider {
    shared: Arc<Mutex<LinuxStylusShared>>,
    summary: String,
    has_device: bool,
}

#[cfg(target_os = "linux")]
impl Default for LinuxStylusProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
impl LinuxStylusProvider {
    /// Probes Wayland `zwp_tablet_manager_v2` and Linux `/sys/class/input/event*` tablets.
    #[must_use]
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(LinuxStylusShared {
            pressure: 1.0,
            pressure_max: 8192,
            ..Default::default()
        }));

        let wayland_found = probe_wayland_tablet_v2(&shared);
        let evdev_nodes = discover_linux_evdev_tablets();
        let mut names = Vec::new();
        for (path, name, p_max) in evdev_nodes {
            names.push(name);
            if let Ok(mut guard) = shared.lock() {
                guard.pressure_max = p_max.max(1);
                guard.evdev_device_names.push(path.clone());
            }
            spawn_evdev_reader(path, p_max.max(1), Arc::clone(&shared));
        }

        let has_device = wayland_found || !names.is_empty();
        let summary = if !names.is_empty() {
            format!("Linux Stylus (evdev: {})", names.join(", "))
        } else if wayland_found {
            "Linux Stylus (Wayland zwp_tablet_v2 + egui force)".to_owned()
        } else {
            "Linux Pointer / egui Touch Force".to_owned()
        };

        Self {
            shared,
            summary,
            has_device,
        }
    }
}

#[cfg(target_os = "linux")]
impl StylusProvider for LinuxStylusProvider {
    fn sample_for_frame(&mut self, egui_ctx: &egui::Context) -> StylusSample {
        let egui_sample = extract_egui_touch_stylus(egui_ctx);
        let hw = self.shared.lock().ok().map(|s| s.clone());

        if let Some(mut s) = egui_sample {
            if let Some(ref h) = hw {
                if h.pen_in_proximity {
                    s.tilt_deg = [h.tilt_x, h.tilt_y];
                    s.twist_deg = h.twist_deg;
                    s.is_eraser = h.is_eraser;
                }
            }
            return s;
        }

        if let Some(h) = hw {
            if h.pen_in_proximity && h.has_pressure {
                return StylusSample {
                    pressure: h.pressure.clamp(0.0, 1.0),
                    has_pressure: true,
                    tilt_deg: [h.tilt_x, h.tilt_y],
                    twist_deg: h.twist_deg,
                    is_eraser: h.is_eraser,
                };
            }
            if h.pen_in_proximity && h.is_eraser {
                return StylusSample {
                    is_eraser: true,
                    ..StylusSample::DEFAULT
                };
            }
        }
        StylusSample::DEFAULT
    }

    fn device_summary(&self) -> String {
        self.summary.clone()
    }

    fn is_hardware_active(&self) -> bool {
        self.has_device
            || self
                .shared
                .lock()
                .ok()
                .is_some_and(|s| s.pen_in_proximity || s.has_pressure)
    }
}

#[cfg(target_os = "linux")]
fn probe_wayland_tablet_v2(shared: &Arc<Mutex<LinuxStylusShared>>) -> bool {
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};
    use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;

    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }

    #[derive(Default)]
    struct State {
        has_tablet_mgr: bool,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            _proxy: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { interface, .. } = event {
                if interface == "zwp_tablet_manager_v2" {
                    state.has_tablet_mgr = true;
                }
            }
        }
    }

    impl Dispatch<ZwpTabletManagerV2, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpTabletManagerV2,
            _: wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let display = conn.display();
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let _registry = display.get_registry(&qh, ());
    let mut state = State::default();
    let _ = queue.roundtrip(&mut state);
    if state.has_tablet_mgr {
        if let Ok(mut guard) = shared.lock() {
            guard.wayland_tablet_detected = true;
        }
    }
    state.has_tablet_mgr
}

#[cfg(target_os = "linux")]
fn discover_linux_evdev_tablets() -> Vec<(String, String, i32)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/input") else {
        return out;
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(ev_name) = fname.to_str() else {
            continue;
        };
        if !ev_name.starts_with("event") {
            continue;
        }
        let sys_dev = entry.path().join("device");
        let Ok(abs_hex) = std::fs::read_to_string(sys_dev.join("capabilities/abs")) else {
            continue;
        };
        // Bit 24 (`ABS_PRESSURE` = 0x18) in the lowest 64-bit word of capabilities/abs
        let last_word = abs_hex
            .split_whitespace()
            .last()
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0);
        let has_abs_pressure = (last_word & (1u64 << 0x18)) != 0;
        if !has_abs_pressure {
            continue;
        }
        let dev_path = format!("/dev/input/{ev_name}");
        if std::fs::File::open(&dev_path).is_err() {
            continue;
        }
        let name = std::fs::read_to_string(sys_dev.join("name"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| ev_name.to_owned());
        out.push((dev_path, name, 8192));
    }
    out
}

#[cfg(target_os = "linux")]
fn spawn_evdev_reader(dev_path: String, pressure_max: i32, shared: Arc<Mutex<LinuxStylusShared>>) {
    use std::io::Read;
    let _ = std::thread::Builder::new()
        .name("coree-evdev-stylus".to_string())
        .spawn(move || {
            let Ok(mut file) = std::fs::File::open(&dev_path) else {
                return;
            };
            // Linux `struct input_event` is 24 bytes on 64-bit (16-byte timeval + u16 type + u16 code + i32 value)
            let pkt_size = if std::mem::size_of::<usize>() >= 8 {
                24
            } else {
                16
            };
            let mut buf = [0u8; 24];
            let mut cur_pressure = 1.0_f32;
            let mut has_p = false;
            let mut max_seen = pressure_max.max(255);
            let mut tx = 0.0_f32;
            let mut ty = 0.0_f32;
            let mut twist = 0.0_f32;
            let mut eraser = false;
            let mut in_prox = false;

            while file.read_exact(&mut buf[..pkt_size]).is_ok() {
                let off = pkt_size - 8;
                let ev_type = u16::from_le_bytes([buf[off], buf[off + 1]]);
                let ev_code = u16::from_le_bytes([buf[off + 2], buf[off + 3]]);
                let ev_val =
                    i32::from_le_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]);

                const EV_SYN: u16 = 0x00;
                const EV_KEY: u16 = 0x01;
                const EV_ABS: u16 = 0x03;
                const ABS_Z: u16 = 0x02;
                const ABS_PRESSURE: u16 = 0x18;
                const ABS_TILT_X: u16 = 0x1a;
                const ABS_TILT_Y: u16 = 0x1b;
                const BTN_TOOL_PEN: u16 = 0x140;
                const BTN_TOOL_RUBBER: u16 = 0x141;

                match ev_type {
                    EV_ABS => match ev_code {
                        ABS_PRESSURE => {
                            if ev_val > max_seen {
                                max_seen = ev_val;
                            }
                            cur_pressure = (ev_val as f32 / max_seen as f32).clamp(0.0, 1.0);
                            has_p = true;
                        }
                        ABS_TILT_X => tx = (ev_val as f32).clamp(-90.0, 90.0),
                        ABS_TILT_Y => ty = (ev_val as f32).clamp(-90.0, 90.0),
                        ABS_Z => twist = (ev_val as f32).rem_euclid(360.0),
                        _ => {}
                    },
                    EV_KEY => match ev_code {
                        BTN_TOOL_PEN => {
                            in_prox = ev_val != 0;
                            if ev_val != 0 {
                                eraser = false;
                            }
                        }
                        BTN_TOOL_RUBBER => {
                            in_prox = ev_val != 0;
                            eraser = ev_val != 0;
                        }
                        _ => {}
                    },
                    EV_SYN => {
                        if let Ok(mut guard) = shared.lock() {
                            guard.pressure = cur_pressure;
                            guard.has_pressure = has_p;
                            guard.tilt_x = tx;
                            guard.tilt_y = ty;
                            guard.twist_deg = twist;
                            guard.is_eraser = eraser;
                            guard.pen_in_proximity = in_prox;
                        }
                    }
                    _ => {}
                }
            }
        });
}

// =============================================================================
// Windows Stylus Provider (Zero `unsafe`: `egui::Event::Touch` / `WM_POINTER`)
// =============================================================================

/// Zero-`unsafe` Windows stylus provider bridging `winit`/`WM_POINTER` touch & pen
/// pressure (`egui::Event::Touch`) and safe tablet capability queries.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct WindowsStylusProvider {
    last_active: bool,
}

#[cfg(windows)]
impl WindowsStylusProvider {
    #[must_use]
    pub fn new() -> Self {
        Self { last_active: false }
    }
}

#[cfg(windows)]
impl StylusProvider for WindowsStylusProvider {
    fn sample_for_frame(&mut self, egui_ctx: &egui::Context) -> StylusSample {
        if let Some(s) = extract_egui_touch_stylus(egui_ctx) {
            self.last_active = true;
            return s;
        }
        StylusSample::DEFAULT
    }

    fn device_summary(&self) -> String {
        "Windows Stylus (WM_POINTER / egui Touch Force)".to_owned()
    }

    fn is_hardware_active(&self) -> bool {
        self.last_active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stylus_provider_extracts_egui_touch_force_and_mock_samples() {
        let ctx = egui::Context::default();
        let mut mock = MockStylusProvider::new(StylusSample::with_pressure(0.42), "Test Tablet");
        assert!(mock.is_hardware_active());
        assert_eq!(mock.device_summary(), "Test Tablet");

        // Without egui Touch event, mock returns its configured sample
        let mut out0 = ctx.run_ui(egui::RawInput::default(), |ui| {
            let s = mock.sample_for_frame(ui.ctx());
            assert!(s.has_pressure);
            assert!((s.pressure - 0.42).abs() < 1e-6);
        });
        out0.textures_delta.clear();

        // With egui::Event::Touch force, NullStylusProvider & MockStylusProvider extract live pressure
        let mut null_p = NullStylusProvider;
        let raw = egui::RawInput {
            events: vec![egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase: egui::TouchPhase::Move,
                pos: egui::pos2(120.0, 120.0),
                force: Some(0.73),
            }],
            ..Default::default()
        };
        let mut out1 = ctx.run_ui(raw, |ui| {
            let s = null_p.sample_for_frame(ui.ctx());
            assert!(s.has_pressure);
            assert!((s.pressure - 0.73).abs() < 1e-6);
        });
        out1.textures_delta.clear();
    }
}
