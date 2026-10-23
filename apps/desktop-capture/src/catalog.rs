//! Enumerate capturable desktop sources.

use suite_core::DesktopSourceId;
use windows_capture::monitor::Monitor;
use windows_capture::window::Window;

/// One selectable desktop source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Identity saved so the source can be reattached later.
    pub id: DesktopSourceId,
    /// Name shown in the source list.
    pub label: String,
    /// Secondary line shown in the source list.
    pub detail: String,
}

/// Failure to enumerate desktop sources.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// Monitor enumeration failed.
    #[error("failed to enumerate monitors: {0}")]
    Monitors(#[from] windows_capture::monitor::Error),
    /// Window enumeration failed.
    #[error("failed to enumerate windows: {0}")]
    Windows(#[from] windows_capture::window::Error),
}

/// All monitors, primary first in label order (`Screen N (Primary)`).
pub fn monitors() -> Result<Vec<Entry>, CatalogError> {
    let primary = Monitor::primary().ok();
    let mut entries = Vec::new();
    for (index, monitor) in Monitor::enumerate()?.into_iter().enumerate() {
        // A monitor without a device name cannot be saved and reattached.
        let Ok(device) = monitor.device_name() else {
            continue;
        };
        let mut label = format!("Screen {}", index + 1);
        if primary == Some(monitor) {
            label.push_str(" (Primary)");
        }
        let detail = match (monitor.width(), monitor.height()) {
            (Ok(w), Ok(h)) => format!("{w}×{h}"),
            _ => String::new(),
        };
        entries.push(Entry {
            id: DesktopSourceId::Monitor { device },
            label,
            detail,
        });
    }
    Ok(entries)
}

/// All windows we can capture, excluding `self_pid`'s own windows.
pub fn windows(self_pid: u32) -> Result<Vec<Entry>, CatalogError> {
    let mut entries = Vec::new();
    for window in Window::enumerate()? {
        let pid = window.process_id().unwrap_or(0);
        let title = window.title().unwrap_or_default();
        if !is_capturable(&title, window.is_valid(), pid, self_pid) {
            continue;
        }
        let process = window.process_name().unwrap_or_default();
        // `DesktopSourceId::Window::process` is stored without the extension.
        let process = process.strip_suffix(".exe").unwrap_or(&process).to_string();
        entries.push(Entry {
            id: DesktopSourceId::Window {
                process: process.clone(),
                title: title.clone(),
            },
            label: title,
            detail: process,
        });
    }
    Ok(entries)
}

/// Whether a window is worth listing: visible to the capture API, not ours, and titled.
pub fn is_capturable(title: &str, valid: bool, pid: u32, self_pid: u32) -> bool {
    valid && pid != self_pid && !title.trim().is_empty()
}

/// `base`, or `base (2)`, `base (3)`, … — the first name not already in `taken`.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|name| name == base) {
        return base.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{base} ({suffix})");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("suffixes are unbounded")
}

#[test]
fn filters_windows_we_cannot_capture() {
    assert!(is_capturable("Notepad", true, 42, 7));
    assert!(!is_capturable("", true, 42, 7));
    assert!(!is_capturable("   ", true, 42, 7));
    assert!(!is_capturable("Notepad", false, 42, 7));
    assert!(!is_capturable("OMT Desktop Capture", true, 7, 7));
}

#[test]
fn names_are_made_unique() {
    assert_eq!(unique_name("Screen 1", &[]), "Screen 1");
    assert_eq!(
        unique_name("Screen 1", &["Screen 1".into()]),
        "Screen 1 (2)"
    );
    assert_eq!(
        unique_name("Screen 1", &["Screen 1".into(), "Screen 1 (2)".into()]),
        "Screen 1 (3)"
    );
}
