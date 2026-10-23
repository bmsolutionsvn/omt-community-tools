//! OMT Desktop Capture (egui/eframe).

// Hide the console window for release GUI launches on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod catalog;
#[cfg(windows)]
mod clock;
#[cfg(windows)]
mod frame;
#[cfg(windows)]
mod runtime;

#[cfg(not(windows))]
fn main() {
    println!("OMT Desktop Capture is Windows-only.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    suite_core::init_tracing();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--selftest") {
        let seconds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
        return selftest(seconds);
    }

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

/// Headless check: send the primary monitor as `Screen 1` for `seconds`.
#[cfg(windows)]
fn selftest(seconds: u64) -> anyhow::Result<()> {
    let entry = catalog::monitors()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no monitors found"))?;
    let cfg = suite_core::DesktopSourceConfig {
        id: entry.id.clone(),
        name: catalog::default_name(&entry),
        ..Default::default()
    };
    let mut runtime = runtime::SourceRuntime::start(
        &cfg,
        &entry,
        suite_core::DesktopQuality::default(),
        suite_core::DesktopCodec::default(),
    )?;
    println!("selftest: sending {} as {:?}", runtime.address(), cfg.id);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let stats = runtime.stats();
        println!(
            "{}: {:.1} fps, {:.2} Mbps, {} conn, {:.1} ms",
            stats.state, stats.fps, stats.mbps, stats.connections, stats.encode_ms
        );
    }
    runtime.stop();
    println!("selftest finished");
    Ok(())
}
