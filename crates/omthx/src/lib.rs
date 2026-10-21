//! Runtime binding to the OMT-HX C API.
//!
//! The library is loaded on first use, so the tools build and run without it: Full OMT needs nothing
//! from this crate. A copy beside the executable, `OMTHX_LIBRARY`, or the normal loader path is used.

#![deny(missing_docs)]

mod library;
mod receiver;
mod sender;

pub use library::{Codec, Error, Mode, Probe, Quality, VideoFormat, available, probe};
pub use receiver::{Audio, Receiver, ReceiverStats, Video};
pub use sender::{Sender, SenderStats};

/// File name of the shared library on this platform.
pub const LIB_NAME: &str = if cfg!(windows) { "omthx.dll" } else { "libomthx.so" };