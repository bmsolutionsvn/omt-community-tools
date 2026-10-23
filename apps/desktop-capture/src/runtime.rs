//! Running one source: capture, sender thread, and row stats.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omthx::{Codec, Mode, Quality, Sender, VideoFormat};
use parking_lot::{Mutex, RwLock};
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

impl SourceState {
    /// Short name for the row's state pill.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Ready => "Ready",
            Self::Live => "Live",
            Self::Offline(_) => "Offline",
        }
    }
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
    /// Newest captured frame, or the previous one with the timestamp to send it under.
    pub fn next(&mut self, sink: &LatestFrame, now_ticks: i64) -> Option<(Arc<FrameData>, i64)> {
        let fresh = if let Some(frame) = sink.take_latest() {
            self.last = Some(frame);
            true
        } else {
            false
        };
        let last = Arc::clone(self.last.as_ref()?);
        let timestamp = if fresh { last.timestamp } else { now_ticks };
        Some((last, timestamp))
    }
}

/// Consecutive send failures before the row goes Offline.
const MAX_SEND_FAILURES: u32 = 3;

/// The sender, shared by the video and audio threads.
///
/// A reopen after a rejected size change replaces the sender in place, so both
/// threads switch to the new one; neither the runtime nor the UI keeps a clone,
/// so the OMT source closes as soon as the workers exit.
pub(crate) struct SharedSender {
    inner: RwLock<Arc<Sender>>,
}

impl SharedSender {
    fn new(sender: Sender) -> Self {
        Self {
            inner: RwLock::new(Arc::new(sender)),
        }
    }

    /// The sender to send with; audio re-reads it per packet so it follows a reopen.
    pub(crate) fn current(&self) -> Arc<Sender> {
        Arc::clone(&self.inner.read())
    }

    fn replace(&self, sender: Sender) {
        *self.inner.write() = Arc::new(sender);
    }
}

/// What the sender thread needs to reopen the sender after a size change.
struct SendSetup {
    name: String,
    quality: Quality,
    codec: Codec,
    fps_n: u32,
    fps_d: u32,
    interval: Duration,
    closed_reason: &'static str,
}

/// One running source: its capture, workers, and the stats the UI reads.
///
/// Neither worker's sender is kept here: the address string is all the UI needs.
pub struct SourceRuntime {
    address: String,
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
        let sender = Sender::open(&cfg.name, Mode::Hx, hx_quality(quality), hx_codec(codec))?;
        let address = sender.address();
        let sender = Arc::new(SharedSender::new(sender));
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
            let setup = SendSetup {
                name: cfg.name.clone(),
                quality: hx_quality(quality),
                codec: hx_codec(codec),
                fps_n,
                fps_d,
                interval,
                closed_reason,
            };
            move || send_loop(sender, sink, flags, stats, stop, setup)
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
            address,
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
            *self.stats.lock() = RowStats {
                state: SourceState::Offline(reason),
                audio_error: audio_error.clone(),
                ..RowStats::default()
            };
        }
        let mut row = self.stats.lock().clone();
        row.audio_error = audio_error;
        // An Offline row has nothing left to send: stop the workers so the
        // sender drops and the OMT source closes.
        if matches!(row.state, SourceState::Offline(_)) {
            self.stop_workers();
        }
        row
    }

    /// Source address a receiver connects to: `HOSTNAME (name)`.
    pub fn address(&self) -> String {
        self.address.clone()
    }

    /// Stop the workers without taking their handles.
    fn stop_workers(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(audio) = &self.audio {
            audio.request_stop();
        }
        if let Some(control) = self.capture.lock().take()
            && let Err(err) = control.stop()
        {
            tracing::warn!("stopping capture failed: {err}");
        }
    }

    /// Stop audio, the sender thread and the capture. Safe to call more than once.
    pub fn stop(&mut self) {
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take()
            && let Err(panic) = thread.join()
        {
            tracing::error!("sender thread panicked: {panic:?}");
            *self.stats.lock() = RowStats {
                state: SourceState::Offline("sender thread panicked".into()),
                ..RowStats::default()
            };
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

/// What the sender thread does after a failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recovery {
    /// The frame size changed: reopen the sender under the same name and retry once.
    Reopen,
    /// A failure that may be transient; keep sending.
    Retry,
    /// Too many failures in a row: the row goes Offline.
    Fatal,
}

/// Failure bookkeeping: one reopen per size change, Offline after three in a row.
#[derive(Default)]
struct SendFailures {
    /// Dimensions of the last frame the sender accepted.
    sent: Option<(u32, u32)>,
    consecutive: u32,
}

impl SendFailures {
    /// The sender took a frame of this size.
    fn accepted(&mut self, width: u32, height: u32) {
        self.sent = Some((width, height));
        self.consecutive = 0;
    }

    /// The sender refused a frame of this size.
    fn failed(&mut self, width: u32, height: u32) -> Recovery {
        if self.sent.is_some_and(|(w, h)| (w, h) != (width, height)) {
            // Recorded now so the same size never reopens twice.
            self.sent = Some((width, height));
            self.consecutive = 0;
            Recovery::Reopen
        } else {
            self.consecutive += 1;
            if self.consecutive >= MAX_SEND_FAILURES {
                Recovery::Fatal
            } else {
                Recovery::Retry
            }
        }
    }
}

/// Sends one frame per tick until stopped, refreshing [`RowStats`] every second.
///
/// `closed` means the source went away; the loop marks the row offline and
/// returns on its next tick, so a closed window never keeps sending its last frame.
///
/// A resolution change the encoder rejects reopens the sender once under the same
/// name; repeated send failures take the row Offline with the library's message.
fn send_loop(
    sender: Arc<SharedSender>,
    sink: Arc<LatestFrame>,
    flags: Arc<CaptureFlags>,
    stats: Arc<Mutex<RowStats>>,
    stop: Arc<AtomicBool>,
    setup: SendSetup,
) {
    let mut pump = FramePump::default();
    let mut prev = sender.current().stats();
    let mut sampled_at = Instant::now();
    let mut next = Instant::now();
    let mut failures = SendFailures::default();
    while !stop.load(Ordering::Relaxed) {
        if flags.closed.load(Ordering::Relaxed) {
            mark_offline(&stats, setup.closed_reason);
            return;
        }
        next += setup.interval;
        if let Some((frame, timestamp)) = pump.next(&sink, clock::now()) {
            let format = VideoFormat {
                width: frame.width,
                height: frame.height,
                fps_n: setup.fps_n,
                fps_d: setup.fps_d,
                alpha: false,
            };
            let stride = frame.width as usize * 4;
            match sender
                .current()
                .send_bgra(&format, &frame.bgra, stride, timestamp)
            {
                Ok(()) => failures.accepted(frame.width, frame.height),
                Err(err) => {
                    tracing::warn!("sending a frame failed: {err}");
                    match failures.failed(frame.width, frame.height) {
                        Recovery::Reopen => {
                            let reopened =
                                Sender::open(&setup.name, Mode::Hx, setup.quality, setup.codec);
                            match reopened {
                                Ok(reopened) => {
                                    sender.replace(reopened);
                                    match sender.current().send_bgra(
                                        &format,
                                        &frame.bgra,
                                        stride,
                                        timestamp,
                                    ) {
                                        Ok(()) => failures.accepted(frame.width, frame.height),
                                        Err(retry) => {
                                            tracing::warn!(
                                                "sending after reopening failed: {retry}"
                                            );
                                            if failures.failed(frame.width, frame.height)
                                                == Recovery::Fatal
                                            {
                                                mark_offline(&stats, &retry.to_string());
                                                return;
                                            }
                                        }
                                    }
                                }
                                Err(open_err) => {
                                    mark_offline(
                                        &stats,
                                        &RuntimeError::Library(open_err).to_string(),
                                    );
                                    return;
                                }
                            }
                        }
                        Recovery::Fatal => {
                            mark_offline(&stats, &err.to_string());
                            return;
                        }
                        Recovery::Retry => {}
                    }
                }
            }
        }
        let elapsed = sampled_at.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            let now = sender.current().stats();
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

/// Take the row Offline with `reason`, keeping any audio message.
fn mark_offline(stats: &Mutex<RowStats>, reason: &str) {
    let mut row = stats.lock();
    row.state = SourceState::Offline(reason.to_string());
    row.fps = 0.0;
    row.mbps = 0.0;
    row.connections = 0;
    row.encode_ms = 0.0;
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
fn state_labels_drop_the_offline_reason() {
    assert_eq!(SourceState::Ready.label(), "Ready");
    assert_eq!(SourceState::Live.label(), "Live");
    assert_eq!(
        SourceState::Offline("window closed".into()).label(),
        "Offline"
    );
    assert_eq!(
        SourceState::Offline("window closed".into()).to_string(),
        "Offline: window closed"
    );
}

#[test]
fn static_source_repeats_the_last_frame_without_copying_it() {
    let sink = LatestFrame::default();
    let mut pump = FramePump::default();
    assert!(pump.next(&sink, 100).is_none());

    let captured = Arc::new(FrameData {
        width: 2,
        height: 2,
        timestamp: 10,
        bgra: vec![1; 16],
    });
    sink.store_from_arc(Arc::clone(&captured));
    let (first, first_ts) = pump.next(&sink, 100).expect("fresh frame");
    assert_eq!(first_ts, 10);
    assert!(Arc::ptr_eq(&first, &captured));

    let (repeat, repeat_ts) = pump.next(&sink, 200).expect("must resend");
    assert_eq!(repeat_ts, 200);
    assert!(Arc::ptr_eq(&repeat, &first));
}

#[test]
fn send_failures_reopen_once_per_size_change_then_go_fatal() {
    let mut failures = SendFailures::default();
    // Nothing accepted yet: no size to compare, so it counts as a plain failure.
    assert_eq!(failures.failed(640, 480), Recovery::Retry);
    failures.accepted(640, 480);
    assert_eq!(failures.failed(640, 480), Recovery::Retry);
    assert_eq!(failures.failed(640, 480), Recovery::Retry);
    // A new size reopens once; the same size failing after that never reopens again.
    assert_eq!(failures.failed(800, 600), Recovery::Reopen);
    assert_eq!(failures.failed(800, 600), Recovery::Retry);
    assert_eq!(failures.failed(800, 600), Recovery::Retry);
    assert_eq!(failures.failed(800, 600), Recovery::Fatal);
    // One accepted frame clears the count.
    failures.accepted(800, 600);
    assert_eq!(failures.failed(800, 600), Recovery::Retry);
}

#[test]
fn offline_clears_counters_and_keeps_the_audio_message() {
    let stats = Mutex::new(RowStats {
        fps: 30.0,
        mbps: 4.0,
        connections: 2,
        encode_ms: 3.0,
        audio_error: Some("audio unavailable: no endpoint".into()),
        state: SourceState::Live,
    });
    mark_offline(&stats, "OMT-HX: encoder failed");
    let row = stats.lock().clone();
    assert_eq!(
        row.state,
        SourceState::Offline("OMT-HX: encoder failed".into())
    );
    assert_eq!(
        (row.fps, row.mbps, row.connections, row.encode_ms),
        (0.0, 0.0, 0, 0.0)
    );
    assert_eq!(
        row.audio_error.as_deref(),
        Some("audio unavailable: no endpoint")
    );
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
