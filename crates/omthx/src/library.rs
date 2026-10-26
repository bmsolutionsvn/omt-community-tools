//! Loading the library and the shared value types.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use crate::LIB_NAME;

/// Whether the source sends plain OMT or OMT-HX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plain OMT with VMX.
    Full,
    /// HEVC or H.264 inside OMT.
    Hx,
}

impl Mode {
    pub(crate) const fn as_c(self) -> c_int {
        match self {
            Self::Full => 0,
            Self::Hx => 1,
        }
    }

    pub(crate) const fn from_c(value: c_int) -> Self {
        if value == 1 { Self::Hx } else { Self::Full }
    }
}

/// HX encoding quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// Low.
    Low,
    /// Medium (the default).
    #[default]
    Medium,
    /// High.
    High,
}

impl Quality {
    pub(crate) const fn as_c(self) -> c_int {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
        }
    }
}

/// HX video codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Codec {
    /// H.265 / HEVC (the default).
    #[default]
    Hevc,
    /// H.264 / AVC.
    H264,
}

impl Codec {
    pub(crate) const fn as_c(self) -> c_int {
        match self {
            Self::Hevc => 0,
            Self::H264 => 1,
        }
    }

    pub(crate) const fn from_c(value: c_int) -> Self {
        if value == 1 { Self::H264 } else { Self::Hevc }
    }
}

/// Picture format of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    /// Display width in pixels.
    pub width: u32,
    /// Display height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub fps_n: u32,
    /// Frame rate denominator.
    pub fps_d: u32,
    /// Whether the source carries alpha.
    pub alpha: bool,
}

impl VideoFormat {
    pub(crate) const fn to_c(self) -> CVideoFormat {
        CVideoFormat {
            width: self.width as c_int,
            height: self.height as c_int,
            fps_n: self.fps_n as c_int,
            fps_d: self.fps_d as c_int,
            alpha: self.alpha as c_int,
        }
    }

    pub(crate) fn from_c(f: CVideoFormat) -> Self {
        Self {
            width: f.width.max(0) as u32,
            height: f.height.max(0) as u32,
            fps_n: f.fps_n.max(1) as u32,
            fps_d: f.fps_d.max(1) as u32,
            alpha: f.alpha != 0,
        }
    }
}

/// What a source is sending, read from one frame header without decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// Full or HX.
    pub mode: Mode,
    /// Codec of an HX source; meaningless for Full.
    pub codec: Codec,
    /// The picture at its full size.
    pub format: VideoFormat,
}

/// Why a call did not work.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// The HX library could not be found or loaded.
    #[error(
        "OMT HX needs omthx.dll (Windows) or libomthx.so (Linux) beside the program or on the library path"
    )]
    Missing,
    /// The library was found but a call reported an error.
    #[error("{0}")]
    Api(String),
}

/// C-side picture format; the layout of `omthx_c_video_format`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CVideoFormat {
    pub width: c_int,
    pub height: c_int,
    pub fps_n: c_int,
    pub fps_d: c_int,
    pub alpha: c_int,
}

pub(crate) const PIXEL_BGRA: c_int = 0;
pub(crate) const PIXEL_UYVY: c_int = 1;

/// The loaded function table. Lives for the process lifetime.
pub(crate) struct Api {
    _lib: libloading::Library,
    pub(crate) error: unsafe extern "C" fn() -> *const c_char,
    pub(crate) sender_open: unsafe extern "C" fn(*const c_char, c_int, c_int, c_int) -> *mut c_void,
    pub(crate) sender_close: unsafe extern "C" fn(*mut c_void),
    pub(crate) sender_video: unsafe extern "C" fn(
        *mut c_void,
        *const CVideoFormat,
        c_int,
        *const u8,
        c_int,
        c_int,
        i64,
    ) -> c_int,
    pub(crate) sender_audio:
        unsafe extern "C" fn(*mut c_void, *const f32, c_int, c_int, c_int, i64) -> c_int,
    pub(crate) sender_keyframe: unsafe extern "C" fn(*mut c_void),
    pub(crate) sender_address: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    pub(crate) sender_get_counters: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
    pub(crate) probe: unsafe extern "C" fn(
        *const c_char,
        c_int,
        *mut CVideoFormat,
        *mut c_int,
        *mut c_int,
    ) -> c_int,
    pub(crate) receiver_open: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    pub(crate) receiver_close: unsafe extern "C" fn(*mut c_void),
    pub(crate) receiver_video: unsafe extern "C" fn(
        *mut c_void,
        c_int,
        *mut CVideoFormat,
        *mut c_int,
        *mut *const u8,
        *mut i64,
    ) -> c_int,
    pub(crate) receiver_audio: unsafe extern "C" fn(
        *mut c_void,
        c_int,
        *mut *const f32,
        *mut c_int,
        *mut c_int,
        *mut c_int,
        *mut i64,
    ) -> c_int,
    pub(crate) receiver_keyframe: unsafe extern "C" fn(*mut c_void),
    pub(crate) receiver_mode: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) receiver_codec: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) receiver_decoder: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    pub(crate) receiver_get_counters: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
}

// The table only holds pointers into a library that lives as long as the process.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

impl Api {
    pub(crate) fn error_message(&self) -> String {
        let ptr = unsafe { (self.error)() };
        if ptr.is_null() {
            return "unknown OMT HX error".into();
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

static API: OnceLock<Option<Api>> = OnceLock::new();

/// The function table, or `None` when the library is not installed.
pub(crate) fn api() -> Option<&'static Api> {
    API.get_or_init(|| unsafe { load().ok() }).as_ref()
}

/// True when the HX library is present and can be used.
pub fn available() -> bool {
    api().is_some()
}

/// Read what `address` sends, without decoding. `None` on timeout or when the library is absent.
pub fn probe(address: &str, timeout: Duration) -> Option<Probe> {
    let api = api()?;
    let address = CString::new(address).ok()?;
    let mut format = CVideoFormat::default();
    let mut mode = 0;
    let mut codec = 0;
    let ok = unsafe {
        (api.probe)(
            address.as_ptr(),
            timeout.as_millis() as c_int,
            &mut format,
            &mut mode,
            &mut codec,
        )
    };
    (ok == 1).then(|| Probe {
        mode: Mode::from_c(mode),
        codec: Codec::from_c(codec),
        format: VideoFormat::from_c(format),
    })
}

/// Paths tried in order: `OMTHX_LIBRARY`, then beside the executable, then the loader's own search.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("OMTHX_LIBRARY").filter(|p| !p.is_empty()) {
        out.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push(dir.join(LIB_NAME));
    }
    out.push(PathBuf::from(LIB_NAME));
    out
}

unsafe fn open(path: &PathBuf) -> Result<libloading::Library, libloading::Error> {
    #[cfg(windows)]
    {
        // Its FFmpeg siblings sit beside it; without these flags the loader searches the app dir only.
        const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
        const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
        unsafe {
            libloading::os::windows::Library::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .map(libloading::Library::from)
    }
    #[cfg(not(windows))]
    {
        unsafe { libloading::Library::new(path) }
    }
}

unsafe fn load() -> Result<Api, Error> {
    let mut last = None;
    for path in candidates() {
        if path.is_absolute() && !path.is_file() {
            continue;
        }
        match unsafe { open(&path) } {
            Ok(lib) => return unsafe { Api::from_library(lib) },
            Err(e) => last = Some(e),
        }
    }
    let _ = last;
    Err(Error::Missing)
}

impl Api {
    unsafe fn from_library(lib: libloading::Library) -> Result<Self, Error> {
        unsafe fn sym<T: Copy>(lib: &libloading::Library, name: &[u8]) -> Result<T, Error> {
            unsafe { lib.get::<T>(name) }
                .map(|s| *s)
                .map_err(|e| Error::Api(e.to_string()))
        }
        Ok(Self {
            error: unsafe { sym(&lib, b"omthx_c_error\0")? },
            sender_open: unsafe { sym(&lib, b"omthx_c_sender_open\0")? },
            sender_close: unsafe { sym(&lib, b"omthx_c_sender_close\0")? },
            sender_video: unsafe { sym(&lib, b"omthx_c_sender_video\0")? },
            sender_audio: unsafe { sym(&lib, b"omthx_c_sender_audio\0")? },
            sender_keyframe: unsafe { sym(&lib, b"omthx_c_sender_keyframe\0")? },
            sender_address: unsafe { sym(&lib, b"omthx_c_sender_address\0")? },
            sender_get_counters: unsafe { sym(&lib, b"omthx_c_sender_get_counters\0")? },
            probe: unsafe { sym(&lib, b"omthx_c_probe\0")? },
            receiver_open: unsafe { sym(&lib, b"omthx_c_receiver_open\0")? },
            receiver_close: unsafe { sym(&lib, b"omthx_c_receiver_close\0")? },
            receiver_video: unsafe { sym(&lib, b"omthx_c_receiver_video\0")? },
            receiver_audio: unsafe { sym(&lib, b"omthx_c_receiver_audio\0")? },
            receiver_keyframe: unsafe { sym(&lib, b"omthx_c_receiver_keyframe\0")? },
            receiver_mode: unsafe { sym(&lib, b"omthx_c_receiver_mode\0")? },
            receiver_codec: unsafe { sym(&lib, b"omthx_c_receiver_codec\0")? },
            receiver_decoder: unsafe { sym(&lib, b"omthx_c_receiver_decoder\0")? },
            receiver_get_counters: unsafe { sym(&lib, b"omthx_c_receiver_get_counters\0")? },
            _lib: lib,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_round_trips() {
        assert_eq!(Mode::from_c(Mode::Hx.as_c()), Mode::Hx);
        assert_eq!(Mode::from_c(Mode::Full.as_c()), Mode::Full);
    }

    #[test]
    fn codec_round_trips() {
        assert_eq!(Codec::from_c(Codec::H264.as_c()), Codec::H264);
        assert_eq!(Codec::from_c(Codec::Hevc.as_c()), Codec::Hevc);
    }

    #[test]
    fn missing_library_is_reported_not_panicked() {
        assert!(unsafe { open(&PathBuf::from("no-such-omthx.dll")) }.is_err());
    }
}
