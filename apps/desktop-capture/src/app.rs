//! egui/eframe Desktop Capture application.

use std::time::Duration;

use eframe::egui;
use suite_core::{
    DESKTOP_FPS_PRESETS, DesktopCaptureConfig, DesktopCodec, DesktopQuality, DesktopSourceConfig,
    DesktopSourceId, SUITE_VERSION, save_desktop_capture_config,
};

use crate::audio::{self, AudioDevice};
use crate::catalog::{self, Entry, Reattach};
use crate::runtime::{SourceRuntime, SourceState};

/// Everything the Add menu can pick from, enumerated when the app opens or refreshes.
#[derive(Default)]
pub struct Catalog {
    monitors: Vec<Entry>,
    windows: Vec<Entry>,
    audio_devices: Vec<AudioDevice>,
    /// What enumeration failed, shown in the toolbar.
    error: Option<String>,
}

impl Catalog {
    /// Enumerate monitors, windows and render endpoints.
    pub fn enumerate() -> Self {
        let mut catalog = Self::default();
        let mut errors = Vec::new();
        match catalog::monitors() {
            Ok(entries) => catalog.monitors = entries,
            Err(err) => errors.push(err.to_string()),
        }
        match catalog::windows(std::process::id()) {
            Ok(entries) => catalog.windows = entries,
            Err(err) => errors.push(err.to_string()),
        }
        match audio::devices() {
            Ok(devices) => catalog.audio_devices = devices,
            Err(err) => errors.push(err.to_string()),
        }
        catalog.error = (!errors.is_empty()).then(|| errors.join("; "));
        catalog
    }
}

/// One row of the source table: saved settings, what it points at, and its runtime.
struct SourceRow {
    cfg: DesktopSourceConfig,
    /// `None` when the saved source is not on screen any more.
    entry: Option<Entry>,
    runtime: Option<SourceRuntime>,
    /// Why the last start failed, shown under the row name.
    error: Option<String>,
}

/// Desktop Capture main window: pick sources, then start and stop them.
pub struct DesktopCaptureApp {
    catalog: Catalog,
    rows: Vec<SourceRow>,
    /// Footer settings; each row keeps its own.
    config: DesktopCaptureConfig,
    /// Minimise once, on the first frame.
    minimise: bool,
}

impl DesktopCaptureApp {
    /// Restore `config`, reattaching each saved source to what is on screen now.
    pub fn new(config: DesktopCaptureConfig, catalog: Catalog) -> Self {
        let config = config.sanitized();
        let rows = config
            .sources
            .iter()
            .map(|cfg| {
                let mut cfg = cfg.clone();
                // R6: an empty endpoint id from an older config means no audio.
                if cfg.audio_device.as_deref() == Some("") {
                    cfg.audio_device = None;
                }
                SourceRow {
                    entry: resolve(&catalog, &cfg.id),
                    cfg,
                    runtime: None,
                    error: None,
                }
            })
            .collect();
        let mut app = Self {
            catalog,
            rows,
            minimise: config.minimise_on_start,
            config,
        };
        if app.config.start_on_launch {
            app.start_all();
        }
        app
    }

    /// Persist the current rows and footer settings.
    pub fn save(&self) {
        if let Err(err) = save_desktop_capture_config(&self.config()) {
            tracing::warn!("saving desktop capture settings failed: {err}");
        }
    }

    /// Current settings in the shape that is written to disk.
    fn config(&self) -> DesktopCaptureConfig {
        DesktopCaptureConfig {
            sources: self.rows.iter().map(|row| row.cfg.clone()).collect(),
            ..self.config.clone()
        }
    }

    fn summary(&self) -> String {
        match &self.catalog.error {
            Some(error) => error.clone(),
            None => format!(
                "{} screens · {} windows",
                self.catalog.monitors.len(),
                self.catalog.windows.len()
            ),
        }
    }

    fn add_source(&mut self, entry: Entry) {
        let taken: Vec<String> = self.rows.iter().map(|row| row.cfg.name.clone()).collect();
        let name = catalog::unique_name(&catalog::default_name(&entry), &taken);
        self.rows.push(SourceRow {
            cfg: DesktopSourceConfig {
                id: entry.id.clone(),
                name,
                ..DesktopSourceConfig::default()
            },
            entry: Some(entry),
            runtime: None,
            error: None,
        });
    }

    /// Look for the row's saved source again, after the user reopened it.
    fn reattach(&mut self, index: usize) {
        self.catalog = Catalog::enumerate();
        let Some(id) = self.rows.get(index).map(|row| row.cfg.id.clone()) else {
            return;
        };
        let entry = resolve(&self.catalog, &id);
        if let Some(row) = self.rows.get_mut(index) {
            row.entry = entry;
            row.error = None;
        }
    }

    fn remove(&mut self, index: usize) {
        if index < self.rows.len() {
            self.rows.remove(index);
        }
    }

    fn start(&mut self, index: usize) {
        let (quality, codec) = (self.config.quality, self.config.codec);
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        let Some(entry) = row.entry.clone() else {
            return;
        };
        match SourceRuntime::start(&row.cfg, &entry, quality, codec) {
            Ok(runtime) => {
                row.runtime = Some(runtime);
                row.error = None;
            }
            Err(err) => row.error = Some(err.to_string()),
        }
    }

    fn stop(&mut self, index: usize) {
        if let Some(row) = self.rows.get_mut(index)
            && let Some(mut runtime) = row.runtime.take()
        {
            runtime.stop();
        }
    }

    fn start_all(&mut self) {
        for index in 0..self.rows.len() {
            let row = &self.rows[index];
            if row.cfg.enabled && row.entry.is_some() && row.runtime.is_none() {
                self.start(index);
            }
        }
    }

    fn stop_all(&mut self) {
        for index in 0..self.rows.len() {
            self.stop(index);
        }
    }
}

/// The saved source, if it can still be told apart on screen.
fn resolve(catalog: &Catalog, id: &DesktopSourceId) -> Option<Entry> {
    match id {
        DesktopSourceId::Monitor { device } => catalog
            .monitors
            .iter()
            .find(|entry| {
                matches!(&entry.id, DesktopSourceId::Monitor { device: found } if found == device)
            })
            .cloned(),
        DesktopSourceId::Window { .. } => match catalog::reattach(id, &catalog.windows) {
            Reattach::Exact(entry) | Reattach::Prefix(entry) | Reattach::Process(entry) => {
                Some(entry)
            }
            Reattach::Ambiguous | Reattach::Missing => None,
        },
    }
}

/// Text for a frame rate: `60` for whole numbers, `29.97` for drop-frame presets.
pub fn fps_label(fps_n: u32, fps_d: u32) -> String {
    if fps_d <= 1 {
        fps_n.to_string()
    } else {
        format!("{:.2}", f64::from(fps_n) / f64::from(fps_d))
    }
}

/// Whether two enabled sources send the same, non-empty audio endpoint.
pub fn duplicate_audio(sources: &[DesktopSourceConfig]) -> bool {
    let mut seen = std::collections::HashSet::new();
    sources
        .iter()
        .filter(|source| source.enabled)
        .filter_map(|source| source.audio_device.as_deref())
        .filter(|device| !device.is_empty())
        .any(|device| !seen.insert(device))
}

/// Selected audio device name, or what the combo should say when it is absent.
fn audio_label(selected: &Option<String>, devices: &[AudioDevice]) -> String {
    match selected {
        None => "No audio".into(),
        Some(id) => devices
            .iter()
            .find(|device| &device.id == id)
            .map(|device| device.name.clone())
            .unwrap_or_else(|| "Missing device".into()),
    }
}

/// Pill colour for a row's state.
fn state_colour(ui: &egui::Ui, state: &SourceState) -> egui::Color32 {
    match state {
        SourceState::Live => ui.visuals().hyperlink_color,
        SourceState::Offline(_) => ui.visuals().error_fg_color,
        SourceState::Connecting | SourceState::Ready => ui.visuals().weak_text_color(),
    }
}

impl eframe::App for DesktopCaptureApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Rows show live states; keep repainting so a closed window reaches Offline up front.
        if self.rows.iter().any(|row| row.runtime.is_some()) {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        if std::mem::take(&mut self.minimise) {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        let mut changed = false;
        let mut start = None;
        let mut stop = None;
        let mut remove = None;
        let mut reattach = None;
        let mut start_all = false;
        let mut stop_all = false;

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("OMT Desktop Capture");
                let mut picked = None;
                ui.menu_button("Add source", |ui| {
                    ui.menu_button("Screens", |ui| {
                        if self.catalog.monitors.is_empty() {
                            ui.weak("No screens found");
                        }
                        for entry in &self.catalog.monitors {
                            let text = format!("{}  {}", entry.label, entry.detail);
                            if ui.button(text).clicked() {
                                picked = Some(entry.clone());
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Applications", |ui| {
                        if self.catalog.windows.is_empty() {
                            ui.weak("No windows found");
                        }
                        for entry in &self.catalog.windows {
                            let text = format!("{}  {}", entry.label, entry.detail);
                            if ui.button(text).clicked() {
                                picked = Some(entry.clone());
                                ui.close();
                            }
                        }
                    });
                });
                if let Some(entry) = picked {
                    self.add_source(entry);
                    changed = true;
                }
                if ui.button("Refresh").clicked() {
                    self.catalog = Catalog::enumerate();
                }
                ui.weak(self.summary());
            });
        });

        egui::Panel::bottom("footer").show(ui, |ui| {
            let live = self.rows.iter().any(|row| row.runtime.is_some());
            ui.horizontal_wrapped(|ui| {
                if ui.button("Start all").clicked() {
                    start_all = true;
                }
                if ui.button("Stop all").clicked() {
                    stop_all = true;
                }
                ui.separator();
                ui.label("Quality");
                for (quality, label) in [
                    (DesktopQuality::Low, "LQ"),
                    (DesktopQuality::Medium, "SQ"),
                    (DesktopQuality::High, "HQ"),
                ] {
                    if ui
                        .selectable_label(self.config.quality == quality, label)
                        .clicked()
                    {
                        self.config.quality = quality;
                        changed = true;
                    }
                }
                ui.separator();
                ui.label("Codec");
                for (codec, label) in [(DesktopCodec::Hevc, "H.265"), (DesktopCodec::H264, "H.264")]
                {
                    if ui
                        .selectable_label(self.config.codec == codec, label)
                        .clicked()
                    {
                        self.config.codec = codec;
                        changed = true;
                    }
                }
                if live {
                    ui.weak("applies on next start");
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.config.start_on_launch, "Start on launch")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(&mut self.config.minimise_on_start, "Minimise on start")
                    .changed()
                {
                    changed = true;
                }
                ui.separator();
                ui.weak(format!("v{SUITE_VERSION}"));
            });
            let devices: Vec<DesktopSourceConfig> =
                self.rows.iter().map(|row| row.cfg.clone()).collect();
            if duplicate_audio(&devices) {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Two sources send the same audio device.",
                );
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.rows.is_empty() {
                ui.weak("No sources yet. Use Add source to pick a screen or an application.");
                return;
            }
            let devices = &self.catalog.audio_devices;
            for (index, row) in self.rows.iter_mut().enumerate() {
                let stats = row.runtime.as_ref().map(SourceRuntime::stats);
                let (pill, colour) = match (&stats, &row.entry) {
                    (Some(stats), _) => (stats.state.label(), state_colour(ui, &stats.state)),
                    (None, None) => ("Offline", ui.visuals().error_fg_color),
                    (None, Some(_)) => ("Idle", ui.visuals().weak_text_color()),
                };
                ui.horizontal(|ui| {
                    let enabled = ui
                        .checkbox(&mut row.cfg.enabled, "")
                        .on_hover_text("Include in Start all");
                    if enabled.changed() {
                        changed = true;
                    }
                    if ui
                        .add(egui::TextEdit::singleline(&mut row.cfg.name).desired_width(180.0))
                        .changed()
                    {
                        changed = true;
                    }
                    if let Some(entry) = &row.entry {
                        ui.weak(&entry.detail);
                    }
                    ui.colored_label(colour, pill);
                    if let Some(stats) = &stats {
                        ui.weak(format!(
                            "{:.0} fps · {:.2} Mbps · {} conn · {:.1} ms",
                            stats.fps, stats.mbps, stats.connections, stats.encode_ms
                        ));
                    }
                    match (&row.runtime, &row.entry) {
                        (Some(_), _) => {
                            if ui.button("Stop").clicked() {
                                stop = Some(index);
                            }
                        }
                        (None, Some(_)) => {
                            if ui.button("Start").clicked() {
                                start = Some(index);
                            }
                        }
                        (None, None) => {
                            if ui.button("Reattach").clicked() {
                                reattach = Some(index);
                            }
                        }
                    }
                });
                if let Some(error) = &row.error {
                    ui.colored_label(ui.visuals().error_fg_color, error.as_str());
                }
                if let Some(error) = stats
                    .as_ref()
                    .and_then(|stats| stats.audio_error.as_deref())
                {
                    ui.colored_label(ui.visuals().warn_fg_color, error);
                }
                egui::CollapsingHeader::new("Settings")
                    .id_salt(("settings", index))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("FPS");
                            egui::ComboBox::from_id_salt(("fps", index))
                                .selected_text(fps_label(row.cfg.fps_n, row.cfg.fps_d))
                                .show_ui(ui, |ui| {
                                    for (fps_n, fps_d) in DESKTOP_FPS_PRESETS {
                                        let selected =
                                            row.cfg.fps_n == fps_n && row.cfg.fps_d == fps_d;
                                        if ui
                                            .selectable_label(selected, fps_label(fps_n, fps_d))
                                            .clicked()
                                        {
                                            row.cfg.fps_n = fps_n;
                                            row.cfg.fps_d = fps_d;
                                            changed = true;
                                        }
                                    }
                                });
                            if ui
                                .checkbox(&mut row.cfg.show_cursor, "Show cursor")
                                .changed()
                            {
                                changed = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Audio");
                            egui::ComboBox::from_id_salt(("audio", index))
                                .selected_text(audio_label(&row.cfg.audio_device, devices))
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_value(
                                            &mut row.cfg.audio_device,
                                            None,
                                            "No audio",
                                        )
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                    for device in devices {
                                        let label = if device.is_default {
                                            format!("{} (default)", device.name)
                                        } else {
                                            device.name.clone()
                                        };
                                        if ui
                                            .selectable_value(
                                                &mut row.cfg.audio_device,
                                                Some(device.id.clone()),
                                                label,
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                            if ui.button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                ui.separator();
            }
        });

        if start_all {
            self.start_all();
        }
        if stop_all {
            self.stop_all();
        }
        if let Some(index) = start {
            self.start(index);
        }
        if let Some(index) = stop {
            self.stop(index);
        }
        if let Some(index) = reattach {
            self.reattach(index);
        }
        if let Some(index) = remove {
            self.remove(index);
            changed = true;
        }
        if changed {
            // `save` reads better than the trait's `save(&mut dyn Storage)`, which shadows it here.
            DesktopCaptureApp::save(self);
        }
    }

    fn on_exit(&mut self) {
        DesktopCaptureApp::save(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_labels_match_presets() {
        assert_eq!(fps_label(30_000, 1_001), "29.97");
        assert_eq!(fps_label(60_000, 1_001), "59.94");
        assert_eq!(fps_label(60, 1), "60");
    }

    #[test]
    fn warns_when_two_sources_share_audio() {
        let source = |name: &str, device: Option<&str>| DesktopSourceConfig {
            name: name.into(),
            audio_device: device.map(str::to_string),
            enabled: true,
            ..Default::default()
        };
        assert!(duplicate_audio(&[
            source("A", Some("dev1")),
            source("B", Some("dev1"))
        ]));
        assert!(!duplicate_audio(&[
            source("A", Some("dev1")),
            source("B", Some("dev2"))
        ]));
        assert!(!duplicate_audio(&[source("A", None), source("B", None)]));
    }

    #[test]
    fn duplicate_audio_ignores_disabled_and_empty_devices() {
        let mut off = source_config("A", Some("dev1"));
        off.enabled = false;
        assert!(!duplicate_audio(&[off, source_config("B", Some("dev1"))]));
        assert!(!duplicate_audio(&[
            source_config("A", Some("")),
            source_config("B", Some(""))
        ]));
    }

    #[test]
    fn restored_sources_reattach_and_keep_settings() {
        let saved = DesktopCaptureConfig {
            sources: vec![
                DesktopSourceConfig {
                    id: monitor_id(),
                    name: "Board".into(),
                    fps_n: 60,
                    fps_d: 1,
                    show_cursor: false,
                    ..Default::default()
                },
                DesktopSourceConfig {
                    id: DesktopSourceId::Window {
                        process: "chrome".into(),
                        title: "Inbox".into(),
                    },
                    name: "Mail".into(),
                    ..Default::default()
                },
                DesktopSourceConfig {
                    id: DesktopSourceId::Window {
                        process: "gone".into(),
                        title: "Gone".into(),
                    },
                    name: "Gone".into(),
                    audio_device: Some(String::new()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let catalog = Catalog {
            monitors: vec![Entry {
                id: monitor_id(),
                label: "Screen 1".into(),
                detail: "1920×1080".into(),
            }],
            windows: vec![Entry {
                id: DesktopSourceId::Window {
                    process: "chrome".into(),
                    title: "Inbox - Gmail".into(),
                },
                label: "Inbox - Gmail".into(),
                detail: "chrome".into(),
            }],
            ..Default::default()
        };

        let app = DesktopCaptureApp::new(saved, catalog);

        assert_eq!(app.rows.len(), 3);
        assert_eq!(app.rows[0].cfg.name, "Board");
        assert_eq!((app.rows[0].cfg.fps_n, app.rows[0].cfg.fps_d), (60, 1));
        assert!(!app.rows[0].cfg.show_cursor);
        assert!(app.rows[0].entry.is_some());
        assert!(matches!(
            &app.rows[1].entry,
            Some(Entry { id: DesktopSourceId::Window { title, .. }, .. })
                if title == "Inbox - Gmail"
        ));
        assert!(app.rows[2].entry.is_none());
        assert_eq!(app.rows[2].cfg.audio_device, None);
    }

    fn monitor_id() -> DesktopSourceId {
        DesktopSourceId::Monitor {
            device: r"\\.\DISPLAY1".into(),
        }
    }

    fn source_config(name: &str, device: Option<&str>) -> DesktopSourceConfig {
        DesktopSourceConfig {
            name: name.into(),
            audio_device: device.map(str::to_string),
            ..Default::default()
        }
    }
}
