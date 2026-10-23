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

    let (config, startup_warning) = match suite_core::load_desktop_capture_config() {
        Ok(config) => (config, None),
        Err(err) => {
            tracing::warn!("loading desktop capture settings failed: {err}");
            let kept = suite_core::app_config_path("desktop-capture")
                .map_err(|path_err| path_err.to_string())
                .and_then(|path| archive_unreadable(&path).map_err(|err| err.to_string()));
            let warning = match kept {
                Ok(path) => format!(
                    "settings could not be read ({err}); kept the old file as {}",
                    path.display()
                ),
                Err(keep_err) => format!(
                    "settings could not be read ({err}); could not keep the old file: {keep_err}"
                ),
            };
            (Default::default(), Some(warning))
        }
    };
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
            let mut app = app::DesktopCaptureApp::new(config, app::Catalog::enumerate());
            if let Some(warning) = startup_warning {
                app.warn(warning);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

/// Move an unreadable settings file aside so the next save cannot overwrite it.
///
/// Returns the `{stem}.bad.json` sibling it was renamed to; an older archive is replaced.
#[cfg(any(windows, test))]
fn archive_unreadable(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let target = path.with_file_name(format!("{stem}.bad.json"));
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    std::fs::rename(path, &target)?;
    Ok(target)
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

#[test]
fn unreadable_settings_are_kept_as_bad_json() {
    let dir = std::env::temp_dir().join(format!(
        "omt-desktop-capture-archive-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("desktop-capture.json");
    std::fs::write(&path, "{ not json").unwrap();
    let stale = dir.join("desktop-capture.bad.json");
    std::fs::write(&stale, "an older broken file").unwrap();

    let kept = archive_unreadable(&path).unwrap();

    assert_eq!(kept, stale);
    assert!(!path.exists());
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "{ not json");
    let _ = std::fs::remove_dir_all(dir);
}
