//! Enumerate capturable desktop sources.

use suite_core::DesktopSourceId;
use windows_capture::WindowsCaptureGraphicsCaptureItem;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{CaptureItemTypes, TryIntoCaptureItemWithType};
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
    /// The saved source is not on screen any more.
    #[error("source not found: {0}")]
    NotFound(String),
    /// A saved window is gone or cannot be told apart from another one.
    #[error("window not attached: {0}")]
    WindowNotAttached(String),
}

/// Longest default source name before a ` (2)` uniqueness suffix is added.
const NAME_MAX_CHARS: usize = 64;

/// One capturable thing, as the capture API wants it.
#[derive(Debug, Clone, Copy)]
pub enum CaptureItem {
    /// A whole monitor.
    Monitor(Monitor),
    /// A single window.
    Window(Window),
}

impl TryIntoCaptureItemWithType for CaptureItem {
    fn try_into_capture_item(
        self,
    ) -> Result<(WindowsCaptureGraphicsCaptureItem, CaptureItemTypes), windows::core::Error> {
        match self {
            Self::Monitor(monitor) => monitor.try_into_capture_item(),
            Self::Window(window) => window.try_into_capture_item(),
        }
    }
}

/// Result of looking for the window a saved source points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reattach {
    /// Same process and the same title.
    Exact(Entry),
    /// Same process, and the found title extends the saved one.
    Prefix(Entry),
    /// Same process, and this is its only window.
    Process(Entry),
    /// More than one window could be the saved one.
    Ambiguous,
    /// Nothing of that process is on screen.
    Missing,
}

/// Find `saved` among `entries`: exact window, then title prefix, then the sole window
/// of the process. Never picks one of several windows; an empty process matches nothing.
pub fn reattach(saved: &DesktopSourceId, entries: &[Entry]) -> Reattach {
    let DesktopSourceId::Window { process, title } = saved else {
        return Reattach::Missing;
    };
    let candidates: Vec<&Entry> = entries
        .iter()
        .filter(|entry| window_id(entry).is_some_and(|(found, _)| same_process(found, process)))
        .collect();

    let exact: Vec<&Entry> = candidates
        .iter()
        .copied()
        .filter(|entry| window_id(entry).is_some_and(|(_, found)| found == title))
        .collect();
    match exact.as_slice() {
        [only] => return Reattach::Exact((*only).clone()),
        [] => {}
        _ => return Reattach::Ambiguous,
    }

    let prefix: Vec<&Entry> = candidates
        .iter()
        .copied()
        .filter(|entry| {
            window_id(entry).is_some_and(|(_, found)| found.starts_with(title.as_str()))
        })
        .collect();
    match prefix.as_slice() {
        [only] => return Reattach::Prefix((*only).clone()),
        [] => {}
        _ => return Reattach::Ambiguous,
    }

    match candidates.as_slice() {
        [only] => Reattach::Process((*only).clone()),
        [] => Reattach::Missing,
        _ => Reattach::Ambiguous,
    }
}

/// Process and title of a window entry; `None` for a monitor.
fn window_id(entry: &Entry) -> Option<(&str, &str)> {
    match &entry.id {
        DesktopSourceId::Window { process, title } => Some((process, title)),
        DesktopSourceId::Monitor { .. } => None,
    }
}

/// Same executable, ignoring case and a trailing `.exe`; an empty name never matches.
fn same_process(found: &str, saved: &str) -> bool {
    let (found, saved) = (strip_exe(found), strip_exe(saved));
    !found.trim().is_empty() && !saved.trim().is_empty() && found.eq_ignore_ascii_case(saved)
}

/// Re-find the source saved in `entry`.
pub fn resolve_item(entry: &Entry) -> Result<CaptureItem, CatalogError> {
    match &entry.id {
        DesktopSourceId::Monitor { device } => Monitor::enumerate()?
            .into_iter()
            .find(|monitor| monitor.device_name().is_ok_and(|name| name == *device))
            .map(CaptureItem::Monitor)
            .ok_or_else(|| CatalogError::NotFound(entry.label.clone())),
        DesktopSourceId::Window { .. } => {
            let pairs = window_pairs(std::process::id())?;
            let entries: Vec<Entry> = pairs.iter().map(|(entry, _)| entry.clone()).collect();
            let found = match reattach(&entry.id, &entries) {
                Reattach::Exact(found) | Reattach::Prefix(found) | Reattach::Process(found) => {
                    found
                }
                Reattach::Ambiguous => {
                    return Err(CatalogError::WindowNotAttached(format!(
                        "{} matches several windows",
                        entry.label
                    )));
                }
                Reattach::Missing => {
                    return Err(CatalogError::WindowNotAttached(format!(
                        "{} is gone",
                        entry.label
                    )));
                }
            };
            pairs
                .into_iter()
                .find(|(candidate, _)| *candidate == found)
                .map(|(_, window)| CaptureItem::Window(window))
                .ok_or_else(|| CatalogError::NotFound(entry.label.clone()))
        }
    }
}

/// All monitors, primary first: `Screen 1` is always the primary.
pub fn monitors() -> Result<Vec<Entry>, CatalogError> {
    let primary = Monitor::primary().ok();
    let mut found = Monitor::enumerate()?;
    found.sort_by_key(|monitor| Some(*monitor) != primary);
    let mut entries = Vec::with_capacity(found.len());
    for monitor in found.into_iter() {
        // A monitor without a device name cannot be saved and reattached.
        let Ok(device) = monitor.device_name() else {
            continue;
        };
        // Numbered after filtering, so a skipped monitor never shifts the rest.
        let mut label = format!("Screen {}", entries.len() + 1);
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
    Ok(window_pairs(self_pid)?
        .into_iter()
        .map(|(entry, _)| entry)
        .collect())
}

/// Every capturable window with its live handle, excluding `self_pid`'s own windows.
fn window_pairs(self_pid: u32) -> Result<Vec<(Entry, Window)>, CatalogError> {
    let mut pairs = Vec::new();
    for window in Window::enumerate()? {
        let pid = window.process_id().unwrap_or(0);
        let title = window.title().unwrap_or_default();
        let process = strip_exe(&window.process_name().unwrap_or_default()).to_string();
        if !is_capturable(&process, &title, window.is_valid(), pid, self_pid) {
            continue;
        }
        pairs.push((
            Entry {
                id: DesktopSourceId::Window {
                    process: process.clone(),
                    title: title.clone(),
                },
                label: title,
                detail: process,
            },
            window,
        ));
    }
    Ok(pairs)
}

/// Whether a window is worth listing: visible to the capture API, not ours, titled,
/// and from a process we can name (an empty process can never be matched again).
pub fn is_capturable(process: &str, title: &str, valid: bool, pid: u32, self_pid: u32) -> bool {
    valid && pid != self_pid && !title.trim().is_empty() && !process.trim().is_empty()
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
fn entry(process: &str, title: &str) -> Entry {
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
fn reattach_prefers_exact_then_prefix_then_sole_process() {
    let saved = DesktopSourceId::Window {
        process: "chrome".into(),
        title: "Inbox".into(),
    };
    assert!(matches!(
        reattach(&saved, &[entry("chrome", "Inbox")]),
        Reattach::Exact(_)
    ));
    assert!(matches!(
        reattach(&saved, &[entry("chrome", "Inbox - Gmail")]),
        Reattach::Prefix(_)
    ));
    assert!(matches!(
        reattach(&saved, &[entry("chrome", "Other")]),
        Reattach::Process(_)
    ));
}

#[test]
fn reattach_refuses_to_guess() {
    let saved = DesktopSourceId::Window {
        process: "chrome".into(),
        title: "Inbox".into(),
    };
    let two = [entry("chrome", "A"), entry("chrome", "B")];
    assert!(matches!(reattach(&saved, &two), Reattach::Ambiguous));
    assert!(matches!(reattach(&saved, &[]), Reattach::Missing));
}

#[test]
fn duplicate_window_titles_are_ambiguous() {
    let saved = DesktopSourceId::Window {
        process: "chrome".into(),
        title: "Inbox".into(),
    };
    let twins = [entry("chrome", "Inbox"), entry("chrome", "Inbox")];
    assert!(matches!(reattach(&saved, &twins), Reattach::Ambiguous));
}

#[test]
fn reattach_means_its_rules() {
    // Process matching ignores case and a trailing `.exe`.
    let saved = DesktopSourceId::Window {
        process: "Chrome.EXE".into(),
        title: "Inbox".into(),
    };
    assert!(matches!(
        reattach(&saved, &[entry("chrome", "Inbox")]),
        Reattach::Exact(_)
    ));

    // An empty process never matches, on either side.
    let empty = DesktopSourceId::Window {
        process: String::new(),
        title: "Inbox".into(),
    };
    assert!(matches!(
        reattach(&empty, &[entry("", "Inbox")]),
        Reattach::Missing
    ));
    assert!(matches!(
        reattach(&saved, &[entry("", "Inbox")]),
        Reattach::Missing
    ));

    // Two windows that both extend the saved title are not guessed between.
    let short = DesktopSourceId::Window {
        process: "chrome".into(),
        title: "In".into(),
    };
    assert!(matches!(
        reattach(
            &short,
            &[entry("chrome", "Inbox"), entry("chrome", "Index")]
        ),
        Reattach::Ambiguous
    ));
}

#[test]
fn filters_windows_we_cannot_capture() {
    assert!(is_capturable("notepad", "Notepad", true, 42, 7));
    assert!(!is_capturable("notepad", "", true, 42, 7));
    assert!(!is_capturable("notepad", "   ", true, 42, 7));
    assert!(!is_capturable("notepad", "Notepad", false, 42, 7));
    assert!(!is_capturable("notepad", "OMT Desktop Capture", true, 7, 7));
    // A window whose process name cannot be read could never be matched again.
    assert!(!is_capturable("", "Notepad", true, 42, 7));
    assert!(!is_capturable("  ", "Notepad", true, 42, 7));
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
        default_name(&entry("notepad", "Untitled")),
        "notepad — Untitled"
    );
    assert_eq!(
        default_name(&entry("notepad.exe", "Untitled")),
        "notepad — Untitled"
    );
    assert_eq!(
        default_name(&entry("NOTEPAD.EXE", "Untitled")),
        "NOTEPAD — Untitled"
    );
}

#[test]
fn names_are_truncated_before_numbering() {
    let title = "t".repeat(80);
    let base = default_name(&entry("app", &title));
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
