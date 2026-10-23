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

/// Seconds the headless check sends for when `--seconds` is not given.
const SELFTEST_SECONDS: u32 = 10;

/// Command-line options.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// Run the headless check for this many seconds; no window is opened.
    pub selftest: Option<u32>,
}

/// Parse program arguments, without the executable name.
pub fn parse_args(args: &[&str]) -> anyhow::Result<Args> {
    let mut selftest = false;
    let mut seconds = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "--selftest" => selftest = true,
            "--seconds" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--seconds needs a value"))?;
                let parsed: u32 = value.parse().map_err(|_| {
                    anyhow::anyhow!("--seconds must be a whole number, got {value}")
                })?;
                anyhow::ensure!(parsed > 0, "--seconds must be at least 1");
                seconds = Some(parsed);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    anyhow::ensure!(
        seconds.is_none() || selftest,
        "--seconds only applies to --selftest"
    );
    Ok(Args {
        selftest: selftest.then(|| seconds.unwrap_or(SELFTEST_SECONDS)),
    })
}

#[cfg(not(windows))]
fn main() {
    println!("OMT Desktop Capture is Windows-only.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    suite_core::init_tracing();

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = raw.iter().map(String::as_str).collect();
    if let Some(seconds) = parse_args(&args)?.selftest {
        return selftest(seconds);
    }

    let config = suite_core::load_desktop_capture_config().unwrap_or_else(|err| {
        tracing::warn!("loading desktop capture settings failed, using defaults: {err}");
        Default::default()
    });
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
        Box::new(move |_cc| {
            Ok(Box::new(app::DesktopCaptureApp::new(
                config,
                app::Catalog::enumerate(),
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

/// Headless check: send the primary monitor as `Screen 1` for `seconds`.
#[cfg(windows)]
fn selftest(seconds: u32) -> anyhow::Result<()> {
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

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds.into());
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

#[test]
fn selftest_defaults_to_ten_seconds() {
    assert_eq!(parse_args(&["--selftest"]).unwrap().selftest, Some(10));
    assert_eq!(
        parse_args(&["--selftest", "--seconds", "8"])
            .unwrap()
            .selftest,
        Some(8)
    );
    assert!(parse_args(&["--selftest", "--seconds", "0"]).is_err());
}
