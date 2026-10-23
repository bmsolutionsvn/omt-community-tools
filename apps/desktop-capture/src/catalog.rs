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

/// Longest default source name before a ` (2)` uniqueness suffix is added.
const NAME_MAX_CHARS: usize = 64;

/// All monitors, primary first: `Screen 1` is always the primary.
pub fn monitors() -> Result<Vec<Entry>, CatalogError> {
    let primary = Monitor::primary().ok();
    let mut found = Monitor::enumerate()?;
    found.sort_by_key(|monitor| Some(*monitor) != primary);
    let mut entries = Vec::with_capacity(found.len());
    for (index, monitor) in found.into_iter().enumerate() {
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
        let process = strip_exe(&window.process_name().unwrap_or_default()).to_string();
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

/// Default saved name: `<process> — <title>` for windows, `Screen N` for monitors.
///
/// Display-only annotations such as ` (Primary)` and a trailing `.exe` are dropped,
/// and the base is truncated to 64 characters before `unique_name` adds a suffix.
pub fn default_name(entry: &Entry) -> String {
    let raw = match &entry.id {
        DesktopSourceId::Monitor { .. } => entry
            .label
            .strip_suffix(" (Primary)")
            .unwrap_or(&entry.label)
            .to_string(),
        DesktopSourceId::Window { process, title } => {
            format!("{} — {title}", strip_exe(process))
        }
    };
    raw.chars().take(NAME_MAX_CHARS).collect()
}

/// Executable name without a trailing `.exe`, in any case.
fn strip_exe(process: &str) -> &str {
    process
        .rsplit_once('.')
        .filter(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("exe"))
        .map_or(process, |(stem, _)| stem)
}

#[cfg(test)]
fn window_entry(process: &str, title: &str) -> Entry {
    Entry {
        id: DesktopSourceId::Window {
            process: process.into(),
            title: title.into(),
        },
        label: title.into(),
        detail: String::new(),
    }
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

#[test]
fn window_names_join_process_and_title() {
    assert_eq!(
        default_name(&window_entry("notepad", "Untitled")),
        "notepad — Untitled"
    );
    assert_eq!(
        default_name(&window_entry("notepad.exe", "Untitled")),
        "notepad — Untitled"
    );
    assert_eq!(
        default_name(&window_entry("NOTEPAD.EXE", "Untitled")),
        "NOTEPAD — Untitled"
    );
}

#[test]
fn names_are_truncated_before_numbering() {
    let title = "t".repeat(80);
    let base = default_name(&window_entry("app", &title));
    assert_eq!(base.chars().count(), 64);
    let name = unique_name(&base, std::slice::from_ref(&base));
    assert_eq!(name, format!("{base} (2)"));
}

#[test]
fn monitor_names_drop_the_primary_annotation() {
    let entry = Entry {
        id: DesktopSourceId::Monitor {
            device: r"\\.\DISPLAY1".into(),
        },
        label: "Screen 1 (Primary)".into(),
        detail: "1920×1080".into(),
    };
    assert_eq!(default_name(&entry), "Screen 1");
}
