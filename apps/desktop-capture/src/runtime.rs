//! Running one source: capture, sender thread, and row stats.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omthx::{Codec, Mode, Quality, Sender, SenderStats, VideoFormat};
use parking_lot::Mutex;
use suite_core::{DesktopCodec, DesktopQuality, DesktopSourceConfig, DesktopSourceId};
use windows_capture::capture::{
    CaptureControl, GraphicsCaptureApiError, GraphicsCaptureApiHandler,
};
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::audio::AudioRuntime;
use crate::capture::{CaptureError, CaptureFlags, CaptureHandler};
use crate::catalog::{self, Entry};
use crate::clock;
use crate::frame::{FrameData, LatestFrame};

/// Failure to start one source.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// `omthx.dll` (or its FFmpeg siblings) is not next to the app.
    #[error("OMT-HX library missing: put omthx.dll and its FFmpeg DLLs beside the app")]
    LibraryMissing,
    /// Opening the OMT source failed.
    #[error("OMT-HX: {0}")]
    Library(#[from] omthx::Error),
    /// The saved source no longer exists.
    #[error(transparent)]
    Catalog(#[from] catalog::CatalogError),
    /// The capture API refused to start.
    #[error("capture: {0}")]
    Capture(#[from] GraphicsCaptureApiError<CaptureError>),
}

/// Where one source is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourceState {
    /// Opening the sender and the capture.
    #[default]
    Connecting,
    /// Open, nobody connected.
    Ready,
    /// Open, at least one receiver connected.
    Live,
    /// Capture ended or failed.
    Offline(String),
}

impl std::fmt::Display for SourceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting => f.write_str("Connecting"),
            Self::Ready => f.write_str("Ready"),
            Self::Live => f.write_str("Live"),
            Self::Offline(reason) => write!(f, "Offline: {reason}"),
        }
    }
}

/// One row of the source table.
#[derive(Debug, Clone, Default)]
pub struct RowStats {
    /// Frames per second actually sent, sampled over the last second.
    pub fps: f64,
    /// Megabits per second actually sent, sampled over the last second.
    pub mbps: f64,
    /// OMT connections; each receiver uses two.
    pub connections: i32,
    /// Encode time of the last frame in milliseconds (HX only).
    pub encode_ms: f64,
    /// Why system audio is not running, if it failed or stopped.
    pub audio_error: Option<String>,
    /// Lifecycle state.
    pub state: SourceState,
}

/// Supplies the sender thread with a frame every tick.
///
/// A static desktop produces no new captures, so the last frame is re-sent with
/// a fresh timestamp; receivers need the keepalive to keep showing the picture.
#[derive(Default)]
pub struct FramePump {
    last: Option<Arc<FrameData>>,
}

impl FramePump {
    /// Newest captured frame, or the previous one re-stamped with `now_ticks`.
    pub fn next(&mut self, sink: &LatestFrame, now_ticks: i64) -> Option<FrameData> {
        let fresh = if let Some(frame) = sink.take_latest() {
            self.last = Some(frame);
            true
        } else {
            false
        };
        let last = self.last.as_ref()?;
        Some(FrameData {
            timestamp: if fresh { last.timestamp } else { now_ticks },
            ..(**last).clone()
        })
    }
}

/// One running source: the sender, its capture, and the stats the UI reads.
///
/// The sender is shared so an audio task can send on the same source.
pub struct SourceRuntime {
    sender: Arc<Sender>,
    capture: Mutex<Option<CaptureControl<CaptureHandler, CaptureError>>>,
    audio: Option<AudioRuntime>,
    flags: Arc<CaptureFlags>,
    closed_reason: &'static str,
    stats: Arc<Mutex<RowStats>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SourceRuntime {
    /// Open the sender and start capturing `entry` into it.
    pub fn start(
        cfg: &DesktopSourceConfig,
        entry: &Entry,
        quality: DesktopQuality,
        codec: DesktopCodec,
    ) -> Result<Self, RuntimeError> {
        if !omthx::available() {
            return Err(RuntimeError::LibraryMissing);
        }
        let sender = Arc::new(Sender::open(
            &cfg.name,
            Mode::Hx,
            hx_quality(quality),
            hx_codec(codec),
        )?);
        let item = catalog::resolve_item(entry)?;

        let fps_n = cfg.fps_n.max(1);
        let fps_d = cfg.fps_d.max(1);
        let interval = Duration::from_secs_f64(f64::from(fps_d) / f64::from(fps_n));
        let cursor = if cfg.show_cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        };
        let sink = Arc::new(LatestFrame::default());
        let flags = Arc::new(CaptureFlags {
            sink: sink.clone(),
            closed: AtomicBool::new(false),
        });
        let settings = Settings::new(
            item,
            cursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Custom(interval),
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags.clone(),
        );
        let capture = CaptureHandler::start_free_threaded(settings)?;

        let stats = Arc::new(Mutex::new(RowStats {
            state: SourceState::Ready,
            ..RowStats::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let closed_reason = if matches!(entry.id, DesktopSourceId::Window { .. }) {
            "window closed"
        } else {
            "capture ended"
        };
        let thread = thread::spawn({
            let sender = Arc::clone(&sender);
            let flags = Arc::clone(&flags);
            let stats = Arc::clone(&stats);
            let stop = Arc::clone(&stop);
            move || {
                send_loop(
                    sender,
                    sink,
                    flags,
                    stats,
                    stop,
                    closed_reason,
                    fps_n,
                    fps_d,
                    interval,
                )
            }
        });

        // `Some("")` is how older saved configs spell "no audio", the same as `None`.
        let mut audio = None;
        let mut audio_error = None;
        if let Some(device) = cfg.audio_device.as_deref().filter(|id| !id.is_empty()) {
            match AudioRuntime::start(Some(device), Arc::clone(&sender)) {
                Ok(runtime) => audio = Some(runtime),
                Err(err) => {
                    tracing::warn!("{err}");
                    audio_error = Some(err.to_string());
                }
            }
        }
        stats.lock().audio_error = audio_error;

        Ok(Self {
            sender,
            capture: Mutex::new(Some(capture)),
            audio,
            flags,
            closed_reason,
            stats,
            stop,
            thread: Some(thread),
        })
    }

    /// Current counters, marking the source offline once its capture has ended.
    ///
    /// A closed window is noticed by the sender thread, so this is only needed for
    /// other capture failures.
    ///
    /// shortcut: a capture that fails is noticed only when this is polled; add a
    /// watcher thread if a run can sit without polling.
    pub fn stats(&self) -> RowStats {
        let audio_error = merged_audio_error(
            self.audio.as_ref().and_then(AudioRuntime::error),
            self.stats.lock().audio_error.clone(),
        );
        let finished = self
            .capture
            .lock()
            .as_ref()
            .is_some_and(CaptureControl::is_finished);
        if finished && let Some(control) = self.capture.lock().take() {
            let reason = match control.wait() {
                Ok(()) if self.flags.closed.load(Ordering::Relaxed) => {
                    self.closed_reason.to_string()
                }
                Ok(()) => "capture ended".to_string(),
                Err(err) => err.to_string(),
            };
            self.stop.store(true, Ordering::Relaxed);
            if let Some(audio) = &self.audio {
                audio.request_stop();
            }
            *self.stats.lock() = RowStats {
                state: SourceState::Offline(reason),
                audio_error: audio_error.clone(),
                ..RowStats::default()
            };
        }
        let mut row = self.stats.lock().clone();
        row.audio_error = audio_error;
        row
    }

    /// Source address a receiver connects to: `HOSTNAME (name)`.
    pub fn address(&self) -> String {
        self.sender.address()
    }

    /// Stop audio, the sender thread and the capture. Safe to call more than once.
    pub fn stop(&mut self) {
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(control) = self.capture.lock().take()
            && let Err(err) = control.stop()
        {
            *self.stats.lock() = RowStats {
                state: SourceState::Offline(err.to_string()),
                ..RowStats::default()
            };
        }
    }
}

impl Drop for SourceRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Sends one frame per tick until stopped, refreshing [`RowStats`] every second.
///
/// `on_closed` sets the shared `closed` flag; the loop marks the row offline and
/// returns on its next tick, so a closed window never keeps sending its last frame.
#[allow(clippy::too_many_arguments)]
fn send_loop(
    sender: Arc<Sender>,
    sink: Arc<LatestFrame>,
    flags: Arc<CaptureFlags>,
    stats: Arc<Mutex<RowStats>>,
    stop: Arc<AtomicBool>,
    closed_reason: &'static str,
    fps_n: u32,
    fps_d: u32,
    interval: Duration,
) {
    let mut pump = FramePump::default();
    let mut prev: SenderStats = sender.stats();
    let mut sampled_at = Instant::now();
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if flags.closed.load(Ordering::Relaxed) {
            *stats.lock() = RowStats {
                state: SourceState::Offline(closed_reason.to_string()),
                ..RowStats::default()
            };
            return;
        }
        next += interval;
        if let Some(frame) = pump.next(&sink, clock::now()) {
            let format = VideoFormat {
                width: frame.width,
                height: frame.height,
                fps_n,
                fps_d,
                alpha: false,
            };
            let stride = frame.width as usize * 4;
            if let Err(err) = sender.send_bgra(&format, &frame.bgra, stride, frame.timestamp) {
                tracing::warn!("sending a frame failed: {err}");
            }
        }
        let elapsed = sampled_at.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            let now = sender.stats();
            let mut row = stats.lock();
            if !matches!(row.state, SourceState::Offline(_)) {
                row.fps = now.frames_sent.saturating_sub(prev.frames_sent) as f64 / elapsed;
                row.mbps =
                    now.bytes_sent.saturating_sub(prev.bytes_sent) as f64 * 8.0 / elapsed / 1e6;
                row.connections = now.connections;
                row.encode_ms = now.encode_ms;
                row.state = if now.connections > 0 {
                    SourceState::Live
                } else {
                    SourceState::Ready
                };
            }
            drop(row);
            prev = now;
            sampled_at = Instant::now();
        }
        thread::sleep(next.saturating_duration_since(Instant::now()));
        if next < Instant::now() {
            next = Instant::now();
        }
    }
}

/// Live audio error, else the one stored when audio failed to start.
fn merged_audio_error(live: Option<String>, stored: Option<String>) -> Option<String> {
    live.or(stored)
}

fn hx_quality(quality: DesktopQuality) -> Quality {
    match quality {
        DesktopQuality::Low => Quality::Low,
        DesktopQuality::Medium => Quality::Medium,
        DesktopQuality::High => Quality::High,
    }
}

fn hx_codec(codec: DesktopCodec) -> Codec {
    match codec {
        DesktopCodec::Hevc => Codec::Hevc,
        DesktopCodec::H264 => Codec::H264,
    }
}

#[test]
fn static_source_repeats_the_last_frame() {
    let sink = LatestFrame::default();
    let mut pump = FramePump::default();
    assert!(pump.next(&sink, 100).is_none());

    sink.store_from_arc(Arc::new(FrameData {
        width: 2,
        height: 2,
        timestamp: 10,
        bgra: vec![1; 16],
    }));
    let first = pump.next(&sink, 100).unwrap();
    assert_eq!(first.timestamp, 10);

    let repeat = pump.next(&sink, 200).expect("must resend");
    assert_eq!(repeat.timestamp, 200);
    assert_eq!(repeat.bgra, first.bgra);
}

#[test]
fn missing_library_has_one_message() {
    assert_eq!(
        RuntimeError::LibraryMissing.to_string(),
        "OMT-HX library missing: put omthx.dll and its FFmpeg DLLs beside the app"
    );
}

#[test]
fn start_time_audio_failure_stays_on_the_row() {
    let stored = Some("audio unavailable: no endpoint".to_string());
    assert_eq!(merged_audio_error(None, stored.clone()), stored);
    assert_eq!(
        merged_audio_error(Some("audio unavailable: device removed".into()), stored),
        Some("audio unavailable: device removed".to_string())
    );
}

#[test]
fn quality_and_codec_map_to_omthx() {
    assert_eq!(hx_quality(DesktopQuality::Low), Quality::Low);
    assert_eq!(hx_quality(DesktopQuality::High), Quality::High);
    assert_eq!(hx_codec(DesktopCodec::H264), Codec::H264);
}
