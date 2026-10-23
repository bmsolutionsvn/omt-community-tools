//! WASAPI loopback capture: system audio as planar f32 for one OMT source.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use parking_lot::Mutex;
use wasapi::{
    AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode,
    WaveFormat,
};

use crate::clock;
use crate::runtime::SharedSender;

/// 100 ns ticks per second, OMT's timestamp unit.
const TICKS_PER_SEC: i64 = 10_000_000;
/// Audio sent per packet: packets are coalesced up to this long.
const PACKET_MS: usize = 10;
/// Poll interval while WASAPI has no packet ready.
const POLL: Duration = Duration::from_millis(4);
/// Wait before reopening loopback after a capture error.
const REOPEN: Duration = Duration::from_millis(250);
/// How long `start` waits for the first loopback open.
const OPEN_TIMEOUT: Duration = Duration::from_secs(3);

/// Failure to start system audio.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// Loopback could not be opened: no endpoint, unsupported mix format, or WASAPI error.
    #[error("audio unavailable: {0}")]
    Unsupported(String),
}

/// A render endpoint whose loopback can be captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Endpoint id, stable across sessions.
    pub id: String,
    /// Friendly name shown in the picker.
    pub name: String,
    /// Whether this is the current default output device.
    pub is_default: bool,
}

/// List active render endpoints, the default one first.
pub fn devices() -> Result<Vec<AudioDevice>, AudioError> {
    let _com = Com::init()?;
    let enumerator = DeviceEnumerator::new().map_err(unsupported)?;
    let default = enumerator
        .get_default_device(&Direction::Render)
        .ok()
        .and_then(|device| device.get_id().ok());
    let collection = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(unsupported)?;
    let mut out = Vec::new();
    for device in &collection {
        let Ok(device) = device else { continue };
        let (Ok(id), Ok(name)) = (device.get_id(), device.get_friendlyname()) else {
            continue;
        };
        out.push(AudioDevice {
            is_default: default.as_deref() == Some(id.as_str()),
            id,
            name,
        });
    }
    out.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// Interleaved f32 samples → planar: all of channel 0, then channel 1, and so on.
pub fn to_planar_f32(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    let frames = interleaved.len() / channels;
    let mut out = Vec::with_capacity(frames * channels);
    for channel in 0..channels {
        out.extend((0..frames).map(|frame| interleaved[frame * channels + channel]));
    }
    out
}

/// Little-endian PCM16 → interleaved f32 in -1..=1.
pub fn pcm16_to_f32(bytes: &[u8]) -> Vec<f32> {
    pcm_to_f32(bytes, 16, 16)
}

/// Little-endian PCM in 16- or 32-bit containers → interleaved f32.
///
/// `valid_bits` handles padded formats (24-in-32): valid bits are the high bits
/// of the container, so the excess low bits are shifted out before scaling.
fn pcm_to_f32(bytes: &[u8], store_bits: u16, valid_bits: u16) -> Vec<f32> {
    let shift = u32::from(store_bits.saturating_sub(valid_bits));
    let scale = 1.0 / (1u64 << (valid_bits.max(1) - 1)) as f32;
    match store_bits {
        16 => bytes
            .chunks_exact(2)
            .map(|b| (i16::from_le_bytes([b[0], b[1]]) >> shift) as f32 * scale)
            .collect(),
        32 => bytes
            .chunks_exact(4)
            .map(|b| (i32::from_le_bytes([b[0], b[1], b[2], b[3]]) >> shift) as f32 * scale)
            .collect(),
        _ => Vec::new(),
    }
}

/// One source's system-audio capture thread.
pub struct AudioRuntime {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioRuntime {
    /// Start loopback capture on the render endpoint `device_id`.
    ///
    /// `None` and `Some("")` mean no endpoint is selected: audio does not start and
    /// [`AudioError::Unsupported`] lets the source keep sending video without it.
    pub fn start(device_id: Option<&str>, sender: Arc<SharedSender>) -> Result<Self, AudioError> {
        let device_id = selected_device(device_id)?.to_owned();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let thread = thread::spawn({
            let stop = Arc::clone(&stop);
            let error = Arc::clone(&error);
            move || capture_thread(device_id, sender, stop, error, ready_tx)
        });
        let mut runtime = Self {
            stop,
            error,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(runtime),
            Ok(Err(err)) => {
                if let Some(thread) = runtime.thread.take() {
                    let _ = thread.join();
                }
                Err(err)
            }
            Err(_) => {
                runtime.stop();
                Err(AudioError::Unsupported(
                    "loopback did not open in time".into(),
                ))
            }
        }
    }

    /// Why audio stopped, if it did; `None` while it is running.
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    /// Ask the capture thread to stop without waiting for it.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stop capturing and wait for the thread. Safe to call more than once.
    pub fn stop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AudioRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Keeps COM initialised on this thread for as long as it lives.
struct Com;

impl Com {
    fn init() -> Result<Self, AudioError> {
        let mta = wasapi::initialize_mta();
        if mta.is_ok() {
            return Ok(Self);
        }
        // The UI thread may already be an STA; WASAPI works there too.
        let sta = wasapi::initialize_sta();
        if sta.is_ok() {
            return Ok(Self);
        }
        Err(AudioError::Unsupported(format!(
            "COM init failed: MTA {mta:?}, STA {sta:?}"
        )))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

/// Converts the endpoint's mix format to interleaved f32.
#[derive(Clone, Copy)]
enum Convert {
    F32,
    F64,
    Pcm { store_bits: u16, valid_bits: u16 },
}

impl Convert {
    fn for_format(format: &WaveFormat) -> Result<Self, AudioError> {
        let store_bits = format.get_bitspersample();
        // 0 means "same as the container" in WAVEFORMATEXTENSIBLE.
        let valid_bits = match format.get_validbitspersample() {
            0 => store_bits,
            bits => bits.min(store_bits),
        };
        let sub = format.get_subformat();
        match (sub.as_ref().ok(), store_bits) {
            (Some(SampleType::Float), 32) => Ok(Self::F32),
            (Some(SampleType::Float), 64) => Ok(Self::F64),
            (Some(SampleType::Int), 16 | 32) => Ok(Self::Pcm {
                store_bits,
                valid_bits,
            }),
            _ => Err(AudioError::Unsupported(format!(
                "unsupported mix format: {store_bits}-bit {sub:?} at {} Hz",
                format.get_samplespersec()
            ))),
        }
    }

    fn convert(self, bytes: &[u8]) -> Vec<f32> {
        match self {
            Self::F32 => bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
            Self::F64 => bytes
                .chunks_exact(8)
                .map(|b| f64::from_le_bytes(b.try_into().expect("8 bytes")) as f32)
                .collect(),
            Self::Pcm {
                store_bits: 16,
                valid_bits: 16,
            } => pcm16_to_f32(bytes),
            Self::Pcm {
                store_bits,
                valid_bits,
            } => pcm_to_f32(bytes, store_bits, valid_bits),
        }
    }
}

/// An opened loopback stream, owned by the capture thread.
struct Opened {
    client: AudioClient,
    capture: AudioCaptureClient,
    rate: usize,
    channels: usize,
    block_align: usize,
    convert: Convert,
    /// Endpoint buffer length in 100 ns ticks, used when a packet has no QPC stamp.
    buffer_ticks: i64,
}

fn open_loopback(device_id: &str) -> Result<Opened, String> {
    let enumerator = DeviceEnumerator::new().map_err(text)?;
    let device = enumerator.get_device(device_id).map_err(text)?;
    let mut client = device.get_iaudioclient().map_err(text)?;
    let format = client.get_mixformat().map_err(text)?;
    let rate = format.get_samplespersec() as usize;
    let channels = format.get_nchannels() as usize;
    if rate == 0 || channels == 0 {
        return Err("mix format has no channels or sample rate".into());
    }
    let convert = Convert::for_format(&format).map_err(|err| err.to_string())?;
    let mode = StreamMode::PollingShared {
        autoconvert: false,
        buffer_duration_hns: 0,
    };
    client
        .initialize_client(&format, &Direction::Capture, &mode)
        .map_err(text)?;
    let capture = client.get_audiocaptureclient().map_err(text)?;
    let buffer_frames = client.get_buffer_size().unwrap_or(0) as i64;
    let block_align = format.get_blockalign() as usize;
    client.start_stream().map_err(text)?;
    Ok(Opened {
        client,
        capture,
        rate,
        channels,
        block_align,
        convert,
        buffer_ticks: buffer_frames * TICKS_PER_SEC / rate as i64,
    })
}

fn capture_thread(
    device_id: String,
    sender: Arc<SharedSender>,
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    ready: mpsc::Sender<Result<(), AudioError>>,
) {
    let _com = match Com::init() {
        Ok(com) => com,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    let mut first = Some(ready);
    loop {
        match open_loopback(&device_id) {
            Ok(opened) => {
                if let Some(ready) = first.take() {
                    let _ = ready.send(Ok(()));
                }
                *error.lock() = None;
                let result = capture_loop(&opened, &sender, &stop, &error);
                let _ = opened.client.stop_stream();
                match result {
                    Ok(()) => return,
                    Err(reason) => {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        *error.lock() = Some(AudioError::Unsupported(reason).to_string());
                    }
                }
            }
            Err(reason) => {
                let failure = AudioError::Unsupported(reason);
                if let Some(ready) = first.take() {
                    let _ = ready.send(Err(failure));
                    return;
                }
                *error.lock() = Some(failure.to_string());
            }
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(REOPEN);
    }
}

/// Read packets, coalesce them to ~10 ms, and send planar f32 until stopped.
fn capture_loop(
    opened: &Opened,
    sender: &SharedSender,
    stop: &AtomicBool,
    error: &Mutex<Option<String>>,
) -> Result<(), String> {
    let flush_frames = (opened.rate * PACKET_MS / 1000).max(1);
    let mut pending: Vec<f32> = Vec::new();
    let mut pending_ts = 0i64;
    while !stop.load(Ordering::Relaxed) {
        let mut read_any = false;
        loop {
            let frames = match opened.capture.get_next_packet_size() {
                Ok(Some(0)) | Ok(None) => break,
                Ok(Some(frames)) => frames as usize,
                Err(err) => return Err(err.to_string()),
            };
            let mut buffer = vec![0u8; frames * opened.block_align];
            let (read, info) = opened
                .capture
                .read_from_device(&mut buffer)
                .map_err(|err| err.to_string())?;
            if read == 0 {
                break;
            }
            let bytes = read as usize * opened.block_align;
            let ts = if info.timestamp == 0 || info.flags.timestamp_error {
                clock::now() - opened.buffer_ticks
            } else {
                // WASAPI already reports this in 100 ns units on the QPC basis.
                clock::from_qpc(info.timestamp as i64)
            };
            if pending.is_empty() {
                pending_ts = ts;
            }
            pending.extend(packet_samples(
                &buffer[..bytes],
                read as usize,
                opened.channels,
                opened.convert,
                info.flags.silent,
            ));
            read_any = true;
        }
        if pending.len() / opened.channels >= flush_frames {
            let planar = to_planar_f32(&pending, opened.channels);
            // Read per packet so a reopened sender is picked up.
            if let Err(err) = sender.current().send_audio_planar(
                &planar,
                opened.rate as i32,
                opened.channels as i32,
                pending_ts,
            ) {
                tracing::warn!("sending audio failed: {err}");
                *error.lock() = Some(format!("audio send failed: {err}"));
            }
            pending.clear();
        }
        if !read_any {
            thread::sleep(POLL);
        }
    }
    Ok(())
}

/// Samples for one packet: zeros when WASAPI marks it silent, its contents otherwise.
fn packet_samples(
    buffer: &[u8],
    frames: usize,
    channels: usize,
    convert: Convert,
    silent: bool,
) -> Vec<f32> {
    if silent {
        vec![0.0; frames * channels]
    } else {
        convert.convert(buffer)
    }
}

/// The endpoint to capture; an empty or missing id means no audio.
fn selected_device(device_id: Option<&str>) -> Result<&str, AudioError> {
    device_id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AudioError::Unsupported("no audio device selected".into()))
}

fn text(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn unsupported(err: impl std::fmt::Display) -> AudioError {
    AudioError::Unsupported(err.to_string())
}

#[test]
fn deinterleaves_stereo() {
    assert_eq!(
        to_planar_f32(&[1.0, 2.0, 3.0, 4.0], 2),
        vec![1.0, 3.0, 2.0, 4.0]
    );
}

#[test]
fn converts_pcm16() {
    let pcm = [0x00u8, 0x00, 0xFF, 0x7F, 0x00, 0x80];
    let out = pcm16_to_f32(&pcm);
    assert!((out[0] - 0.0).abs() < 1e-6);
    assert!((out[1] - 1.0).abs() < 1e-3);
    assert!((out[2] + 1.0).abs() < 1e-3);
}

#[test]
fn converts_pcm32() {
    let pcm = [0x00u8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80];
    let out = pcm_to_f32(&pcm, 32, 32);
    assert_eq!(out.len(), 2);
    assert!((out[0] - 0.0).abs() < 1e-6);
    assert!((out[1] + 1.0).abs() < 1e-3);
}

#[test]
fn converts_24_in_32_through_the_format() {
    let format = WaveFormat::new(32, 24, &SampleType::Int, 48_000, 2, None);
    let convert = Convert::for_format(&format).expect("PCM 24-in-32");
    let half = 1i32 << 30; // 24 valid bits, left aligned: 0.5 after >> 8 and scaling.
    let bytes: Vec<u8> = [half, -half]
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    let out = convert.convert(&bytes);
    assert_eq!(out.len(), 2);
    assert!((out[0] - 0.5).abs() < 1e-6);
    assert!((out[1] + 0.5).abs() < 1e-6);
}

#[test]
fn zero_valid_bits_mean_the_container_width() {
    // WAVEFORMATEXTENSIBLE uses 0 for "same as the container"; clamping it to 1
    // would shift every sample out and leave silence.
    let format = WaveFormat::new(32, 0, &SampleType::Int, 48_000, 2, None);
    let convert = Convert::for_format(&format).expect("PCM 32");
    let half = 1i32 << 30;
    let bytes: Vec<u8> = [half, -half]
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    let out = convert.convert(&bytes);
    assert_eq!(out.len(), 2);
    assert!((out[0] - 0.5).abs() < 1e-6);
    assert!((out[1] + 0.5).abs() < 1e-6);
}

#[test]
fn silent_packets_become_zeros() {
    let silence = packet_samples(&[0xFF; 8], 2, 2, Convert::F32, true);
    assert_eq!(silence, vec![0.0; 4]);
    let one = packet_samples(&[0, 0, 0, 0, 0, 0, 128, 63], 2, 1, Convert::F32, false);
    assert_eq!(one, vec![0.0, 1.0]);
}

#[test]
fn empty_or_missing_device_is_no_audio() {
    assert!(matches!(
        selected_device(None),
        Err(AudioError::Unsupported(_))
    ));
    assert!(matches!(
        selected_device(Some("")),
        Err(AudioError::Unsupported(_))
    ));
    assert_eq!(selected_device(Some("endpoint")).unwrap(), "endpoint");
}

#[test]
fn request_stop_flips_the_flag() {
    let stop = Arc::new(AtomicBool::new(false));
    let mut runtime = AudioRuntime {
        stop: Arc::clone(&stop),
        error: Arc::new(Mutex::new(None)),
        thread: Some(thread::spawn(|| {})),
    };
    runtime.request_stop();
    assert!(stop.load(Ordering::Relaxed));
    runtime.stop();
}

/// Opens the default render endpoint's loopback.
///
/// `wasapi` 0.26 needs real hardware, so this stays opt-in:
/// `cargo test -p omt-desktop-capture audio -- --ignored`.
#[test]
#[ignore = "needs a real render endpoint"]
fn default_loopback_opens() {
    let _com = Com::init().expect("COM");
    let devices = devices().expect("devices");
    let default = devices
        .iter()
        .find(|device| device.is_default)
        .expect("default device");
    let opened = open_loopback(&default.id).expect("loopback opens");
    assert!(opened.rate > 0 && opened.channels > 0);
    opened.client.stop_stream().expect("stop");
}
