//! egui/eframe Desktop Capture application.

use std::time::Duration;

use eframe::egui;
use suite_core::{DesktopCodec, DesktopQuality, DesktopSourceConfig};

use crate::catalog::{self, Entry};
use crate::runtime::SourceRuntime;

/// A source the user has added to the list.
struct Source {
    entry: Entry,
    name: String,
    runtime: Option<SourceRuntime>,
    /// Why the last start failed, shown under the row name.
    error: Option<String>,
}

/// Desktop Capture main window: pick sources, then start and stop them.
pub struct DesktopCaptureApp {
    monitors: Vec<Entry>,
    windows: Vec<Entry>,
    sources: Vec<Source>,
    quality: DesktopQuality,
    codec: DesktopCodec,
    status: String,
}

impl Default for DesktopCaptureApp {
    fn default() -> Self {
        let mut app = Self {
            monitors: Vec::new(),
            windows: Vec::new(),
            sources: Vec::new(),
            quality: DesktopQuality::default(),
            codec: DesktopCodec::default(),
            status: String::new(),
        };
        app.refresh();
        app
    }
}

impl DesktopCaptureApp {
    fn refresh(&mut self) {
        let mut errors = Vec::new();
        match catalog::monitors() {
            Ok(entries) => self.monitors = entries,
            Err(err) => errors.push(err.to_string()),
        }
        match catalog::windows(std::process::id()) {
            Ok(entries) => self.windows = entries,
            Err(err) => errors.push(err.to_string()),
        }
        self.status = if errors.is_empty() {
            format!(
                "{} screens · {} windows",
                self.monitors.len(),
                self.windows.len()
            )
        } else {
            errors.join("; ")
        };
    }

    fn add_source(&mut self, entry: Entry) {
        let taken: Vec<String> = self.sources.iter().map(|s| s.name.clone()).collect();
        let name = catalog::unique_name(&catalog::default_name(&entry), &taken);
        self.sources.push(Source {
            entry,
            name,
            runtime: None,
            error: None,
        });
    }
}

impl eframe::App for DesktopCaptureApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Rows show live states; keep repainting so a closed window reaches Offline up front.
        if self.sources.iter().any(|source| source.runtime.is_some()) {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("OMT Desktop Capture");
                let mut picked = None;
                ui.menu_button("Add source ▾", |ui| {
                    ui.menu_button("Screens", |ui| {
                        if self.monitors.is_empty() {
                            ui.weak("No screens found");
                        }
                        for entry in &self.monitors {
                            let text = format!("{}  {}", entry.label, entry.detail);
                            if ui.button(text).clicked() {
                                picked = Some(entry.clone());
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Applications", |ui| {
                        if self.windows.is_empty() {
                            ui.weak("No windows found");
                        }
                        for entry in &self.windows {
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
                }
                if ui.button("Refresh").clicked() {
                    self.refresh();
                }
                ui.weak(&self.status);
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.sources.is_empty() {
                ui.weak("No sources yet. Use Add source ▾ to pick a screen or an application.");
                return;
            }
            let (quality, codec) = (self.quality, self.codec);
            for source in &mut self.sources {
                let mut start = false;
                let mut stop = false;
                ui.horizontal(|ui| {
                    ui.label(&source.name);
                    ui.weak(&source.entry.detail);
                    match source.runtime.as_ref() {
                        Some(runtime) => {
                            let stats = runtime.stats();
                            ui.weak(stats.state.to_string());
                            ui.weak(format!(
                                "{:.0} fps · {:.2} Mbps · {} conn · {:.1} ms",
                                stats.fps, stats.mbps, stats.connections, stats.encode_ms
                            ));
                            stop = ui.button("Stop").clicked();
                        }
                        None => start = ui.button("Start").clicked(),
                    }
                });
                if let Some(error) = &source.error {
                    ui.colored_label(ui.visuals().error_fg_color, error.as_str());
                }
                if start {
                    let cfg = DesktopSourceConfig {
                        id: source.entry.id.clone(),
                        name: source.name.clone(),
                        ..DesktopSourceConfig::default()
                    };
                    match SourceRuntime::start(&cfg, &source.entry, quality, codec) {
                        Ok(runtime) => {
                            source.runtime = Some(runtime);
                            source.error = None;
                        }
                        Err(err) => source.error = Some(err.to_string()),
                    }
                } else if stop && let Some(mut runtime) = source.runtime.take() {
                    runtime.stop();
                }
            }
        });
    }
}
