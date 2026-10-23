//! OMT Desktop Capture (egui/eframe).

// Hide the console window for release GUI launches on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod catalog;

#[cfg(not(windows))]
fn main() {
    println!("OMT Desktop Capture is Windows-only.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    suite_core::init_tracing();

    let title = "OMT Desktop Capture";
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_title(title),
        ..Default::default()
    };
    eframe::run_native(
        title,
        options,
        Box::new(|_cc| Ok(Box::new(app::DesktopCaptureApp::default()))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
