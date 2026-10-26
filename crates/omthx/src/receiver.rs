//! Receiving an OMT source in either mode.

use std::ffi::{CStr, CString, c_int, c_void};
use std::time::Duration;

use crate::library::{Api, Codec, Error, Mode, VideoFormat, api};

/// One decoded picture. BGRA in memory, valid until the next `recv_video` call.
pub struct Video<'a> {
    /// Display width in pixels.
    pub width: u32,
    /// Display height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub fps_n: u32,
    /// Frame rate denominator.
    pub fps_d: u32,
    /// Whether the colour is premultiplied by alpha.
    pub premultiplied: bool,
    /// OMT timestamp in 100 ns ticks.
    pub timestamp: i64,
    /// Tightly packed BGRA, `width * height * 4` bytes.
    pub bgra: &'a [u8],
}

/// One audio packet, planar f32, valid until the next `recv_audio` call.
pub struct Audio<'a> {
    /// Sample rate in Hz.
    pub sample_rate: i32,
    /// Channel count.
    pub channels: i32,
    /// Samples per channel.
    pub samples: i32,
    /// OMT timestamp of the first sample, 100 ns ticks.
    pub timestamp: i64,
    /// All samples of channel 0, then channel 1, and so on.
    pub planar: &'a [f32],
}

/// Counter snapshot from `omthx::ReceiverStats`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiverStats {
    /// Mode of the source.
    pub mode: c_int,
    /// Codec of the source (HX only).
    pub codec: c_int,
    /// Pictures received.
    pub frames_received: u64,
    /// Frames dropped while waiting for a keyframe.
    pub frames_discarded: u64,
    /// Wire bytes.
    pub bytes_received: u64,
    /// Decode and unpack time of the last frame, milliseconds (HX only).
    pub decode_ms: f64,
}

impl ReceiverStats {
    /// Source mode as an enum.
    pub fn mode(&self) -> Mode {
        Mode::from_c(self.mode)
    }

    /// Source codec as an enum.
    pub fn codec(&self) -> Codec {
        Codec::from_c(self.codec)
    }
}

/// A receiver for one source, in either mode. Video and audio may be read from two threads.
pub struct Receiver {
    api: &'static Api,
    ptr: *mut c_void,
    decoder: String,
}

unsafe impl Send for Receiver {}

impl Receiver {
    /// Connect to `address` (`HOST (name)` or `omt://host:port`).
    pub fn open(address: &str) -> Result<Self, Error> {
        let api = api().ok_or(Error::Missing)?;
        let address =
            CString::new(address).map_err(|_| Error::Api("address contains a NUL".into()))?;
        let ptr = unsafe { (api.receiver_open)(address.as_ptr()) };
        if ptr.is_null() {
            return Err(Error::Api(api.error_message()));
        }
        let receiver = Self {
            api,
            ptr,
            decoder: String::new(),
        };
        receiver.request_keyframe();
        Ok(receiver)
    }

    /// Wait up to `timeout` for a picture.
    pub fn recv_video(&mut self, timeout: Duration) -> Option<Video<'_>> {
        let mut format = crate::library::CVideoFormat::default();
        let mut premultiplied = 0;
        let mut bgra = std::ptr::null();
        let mut timestamp = 0i64;
        let ok = unsafe {
            (self.api.receiver_video)(
                self.ptr,
                timeout.as_millis() as c_int,
                &mut format,
                &mut premultiplied,
                &mut bgra,
                &mut timestamp,
            )
        };
        if ok != 1 || bgra.is_null() {
            return None;
        }
        let width = format.width.max(0) as u32;
        let height = format.height.max(0) as u32;
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        Some(Video {
            width,
            height,
            fps_n: format.fps_n.max(1) as u32,
            fps_d: format.fps_d.max(1) as u32,
            premultiplied: premultiplied != 0,
            timestamp,
            bgra: unsafe { std::slice::from_raw_parts(bgra, len) },
        })
    }

    /// Wait up to `timeout` for an audio packet.
    pub fn recv_audio(&mut self, timeout: Duration) -> Option<Audio<'_>> {
        let mut planar = std::ptr::null();
        let mut sample_rate = 0;
        let mut channels = 0;
        let mut samples = 0;
        let mut timestamp = 0i64;
        let ok = unsafe {
            (self.api.receiver_audio)(
                self.ptr,
                timeout.as_millis() as c_int,
                &mut planar,
                &mut sample_rate,
                &mut channels,
                &mut samples,
                &mut timestamp,
            )
        };
        if ok != 1 || planar.is_null() || channels <= 0 || samples <= 0 {
            return None;
        }
        let len = (channels as usize).checked_mul(samples as usize)?;
        Some(Audio {
            sample_rate,
            channels,
            samples,
            timestamp,
            planar: unsafe { std::slice::from_raw_parts(planar, len) },
        })
    }

    /// Ask an HX source for a keyframe (at most once per 500 ms).
    pub fn request_keyframe(&self) {
        unsafe { (self.api.receiver_keyframe)(self.ptr) };
    }

    /// What the source is sending.
    pub fn mode(&self) -> Mode {
        Mode::from_c(unsafe { (self.api.receiver_mode)(self.ptr) })
    }

    /// Codec of an HX source.
    pub fn codec(&self) -> Codec {
        Codec::from_c(unsafe { (self.api.receiver_codec)(self.ptr) })
    }

    /// Decoder description for a log line, for example `hevc (d3d11va)`.
    pub fn decoder(&mut self) -> &str {
        let ptr = unsafe { (self.api.receiver_decoder)(self.ptr) };
        if !ptr.is_null() {
            self.decoder = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
        }
        &self.decoder
    }

    /// Current counters.
    pub fn stats(&self) -> ReceiverStats {
        let mut out = ReceiverStats::default();
        let ok = unsafe {
            (self.api.receiver_get_counters)(
                self.ptr,
                &mut out as *mut ReceiverStats as *mut c_void,
            )
        };
        if ok == 1 {
            out
        } else {
            ReceiverStats::default()
        }
    }

    /// The source's picture format as a [`VideoFormat`].
    pub fn format(video: &Video<'_>) -> VideoFormat {
        VideoFormat {
            width: video.width,
            height: video.height,
            fps_n: video.fps_n,
            fps_d: video.fps_d,
            alpha: false,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.api.receiver_close)(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}
