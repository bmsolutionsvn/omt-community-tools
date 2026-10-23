//! Sending an OMT source in either mode.

use std::ffi::{CStr, CString, c_int, c_void};

use crate::library::{Api, Codec, Error, Mode, Quality, VideoFormat, api, PIXEL_BGRA, PIXEL_UYVY};

/// Counter snapshot from `omthx::SenderStats`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SenderStats {
    /// Frames handed to libomt.
    pub frames_sent: u64,
    /// Keyframes sent (HX only).
    pub keyframes: u64,
    /// Wire bytes, summed over all receivers.
    pub bytes_sent: u64,
    /// Frames a receiver was too slow to take.
    pub frames_dropped: u64,
    /// Pack and encode time of the last frame, milliseconds (HX only).
    pub encode_ms: f64,
    /// OMT connections; each receiver uses two.
    pub connections: i32,
}

/// One OMT source. Video and audio may be sent from two threads, one each.
pub struct Sender {
    api: &'static Api,
    ptr: *mut c_void,
}

// The C API documents sendVideo and sendAudio as callable from two threads.
unsafe impl Send for Sender {}
unsafe impl Sync for Sender {}

impl Sender {
    /// Create a source. In [`Mode::Full`] the codec is ignored.
    pub fn open(name: &str, mode: Mode, quality: Quality, codec: Codec) -> Result<Self, Error> {
        let api = api().ok_or(Error::Missing)?;
        let name = CString::new(name).map_err(|_| Error::Api("source name contains a NUL".into()))?;
        let ptr = unsafe {
            (api.sender_open)(
                name.as_ptr(),
                mode.as_c(),
                quality.as_c(),
                codec.as_c(),
            )
        };
        if ptr.is_null() {
            return Err(Error::Api(api.error_message()));
        }
        Ok(Self { api, ptr })
    }

    /// Send one tightly packed UYVY picture.
    pub fn send_uyvy(
        &self,
        format: &VideoFormat,
        uyvy: &[u8],
        stride: usize,
        timestamp: i64,
    ) -> Result<(), Error> {
        let c = format.to_c();
        let ok = unsafe {
            (self.api.sender_video)(
                self.ptr,
                &c,
                PIXEL_UYVY,
                uyvy.as_ptr(),
                stride as c_int,
                0,
                timestamp,
            )
        };
        if ok == 1 {
            Ok(())
        } else {
            Err(Error::Api(self.api.error_message()))
        }
    }

    /// Send one BGRA picture (top row first).
    pub fn send_bgra(
        &self,
        format: &VideoFormat,
        bgra: &[u8],
        stride: usize,
        timestamp: i64,
    ) -> Result<(), Error> {
        let c = format.to_c();
        let ok = unsafe {
            (self.api.sender_video)(
                self.ptr,
                &c,
                PIXEL_BGRA,
                bgra.as_ptr(),
                stride as c_int,
                0,
                timestamp,
            )
        };
        if ok == 1 {
            Ok(())
        } else {
            Err(Error::Api(self.api.error_message()))
        }
    }

    /// Send planar f32 audio: all samples of channel 0, then channel 1, and so on.
    pub fn send_audio_planar(
        &self,
        planar: &[f32],
        sample_rate: i32,
        channels: i32,
        timestamp: i64,
    ) -> Result<(), Error> {
        if planar.is_empty() || channels <= 0 {
            return Ok(());
        }
        let samples = planar.len() / channels.max(1) as usize;
        let ok = unsafe {
            (self.api.sender_audio)(
                self.ptr,
                planar.as_ptr(),
                sample_rate,
                channels,
                samples as c_int,
                timestamp,
            )
        };
        if ok == 1 {
            Ok(())
        } else {
            Err(Error::Api(self.api.error_message()))
        }
    }

    /// Make the next coded frame a keyframe (HX only, at most once per 500 ms).
    pub fn request_keyframe(&self) {
        unsafe { (self.api.sender_keyframe)(self.ptr) };
    }

    /// The source address a receiver connects to: `HOSTNAME (name)`.
    pub fn address(&self) -> String {
        let ptr = unsafe { (self.api.sender_address)(self.ptr) };
        if ptr.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }

    /// Current counters.
    pub fn stats(&self) -> SenderStats {
        let mut out = SenderStats::default();
        let ok = unsafe {
            (self.api.sender_get_counters)(self.ptr, &mut out as *mut SenderStats as *mut c_void)
        };
        if ok == 1 { out } else { SenderStats::default() }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.api.sender_close)(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::available;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn hx_loopback_probe_and_receive() {
        if !available() {
            return; // the library is optional; this test needs it
        }
        let width = 320u32;
        let height = 180u32;
        let format = VideoFormat {
            width,
            height,
            fps_n: 30,
            fps_d: 1,
            alpha: false,
        };
        let sender = Sender::open("OMTHX Rust Test", Mode::Hx, Quality::Medium, Codec::Hevc)
            .expect("open HX sender");
        let address = sender.address();
        assert!(!address.is_empty());

        let frame = vec![128u8; (width * 2 * height) as usize];
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sender = std::sync::Arc::new(sender);
        let writer = {
            let sender = std::sync::Arc::clone(&sender);
            let running = std::sync::Arc::clone(&running);
            thread::spawn(move || {
                let mut ts = 0i64;
                while running.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = sender.send_uyvy(&format, &frame, (width * 2) as usize, ts);
                    ts += 333_333;
                    thread::sleep(Duration::from_millis(10));
                }
            })
        };

        let probe = crate::probe(&address, Duration::from_millis(1500));
        assert!(probe.is_some(), "probe found no picture");
        assert_eq!(probe.unwrap().mode, Mode::Hx);

        let mut receiver = crate::Receiver::open(&address).expect("open receiver");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut video = None;
        while video.is_none() && std::time::Instant::now() < deadline {
            video = receiver.recv_video(Duration::from_millis(100));
        }
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = writer.join();
        let video = video.expect("no decoded frame");
        assert_eq!(video.width, width);
        assert_eq!(video.height, height);
        assert_eq!(video.bgra.len(), (width * height * 4) as usize);
        assert!(sender.stats().frames_sent > 0);
    }

    #[test]
    fn hx_loopback_bgra() {
        if !available() {
            return;
        }
        let (width, height) = (320u32, 180u32);
        let format = VideoFormat { width, height, fps_n: 30, fps_d: 1, alpha: false };
        let sender =
            Sender::open("OMTHX BGRA Test", Mode::Hx, Quality::Medium, Codec::Hevc).expect("open");
        let address = sender.address();
        let mut bgra = vec![0u8; (width * 4 * height) as usize];
        for px in bgra.chunks_exact_mut(4) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 200; // red in BGRA
            px[3] = 255;
        }
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let sender = std::sync::Arc::new(sender);
        let writer = {
            let sender = std::sync::Arc::clone(&sender);
            let running = std::sync::Arc::clone(&running);
            thread::spawn(move || {
                let mut ts = 0i64;
                while running.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = sender.send_bgra(&format, &bgra, (width * 4) as usize, ts);
                    ts += 333_333;
                    thread::sleep(Duration::from_millis(10));
                }
            })
        };
        let mut receiver = crate::Receiver::open(&address).expect("open receiver");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut video = None;
        while video.is_none() && std::time::Instant::now() < deadline {
            video = receiver.recv_video(Duration::from_millis(100));
        }
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = writer.join();
        let video = video.expect("no decoded frame");
        assert_eq!((video.width, video.height), (width, height));
        assert_eq!(video.bgra.len(), (width * height * 4) as usize);
        assert!(sender.stats().frames_sent > 0);
    }
}