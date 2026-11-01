//! Graphics Capture handler feeding a [`LatestFrame`] sink.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;

use crate::frame::{FrameData, LatestFrame, Pixels, packed_rows, pad_to_even};
use crate::gpu::GpuRing;

/// Failure while turning a captured frame into a [`FrameData`].
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Reading the frame's pixels failed.
    #[error("frame buffer: {0}")]
    Frame(#[from] windows_capture::frame::Error),
}

/// Shared between the capture handler and the runtime that owns it.
pub struct CaptureFlags {
    /// Where captured frames go.
    pub sink: Arc<LatestFrame>,
    /// Set when the source went away (window closed, display gone).
    pub closed: AtomicBool,
    /// Frames stay in video memory. Cleared, for good, when that path fails; frames are then read back.
    pub gpu: AtomicBool,
}

/// Puts each captured frame in the shared sink; the newest frame wins.
pub struct CaptureHandler {
    flags: Arc<CaptureFlags>,
    /// `None` when the capture device could not be shared with the sender thread.
    ring: Option<GpuRing>,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = Arc<CaptureFlags>;
    type Error = CaptureError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let ring = GpuRing::new(ctx.device, ctx.device_context)
            .inspect_err(|err| tracing::warn!("capture stays in memory: {err}"))
            .ok();
        Ok(Self {
            flags: ctx.flags,
            ring,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let width = frame.width();
        let height = frame.height();
        if self.flags.gpu.load(Ordering::Relaxed)
            && let Some(ring) = &mut self.ring
        {
            // A GPU-to-GPU copy: queued, so this returns at once and the frame goes back to the pool.
            match ring.copy(frame.as_raw_texture(), width, height) {
                Ok(texture) => {
                    self.flags.sink.store(FrameData {
                        width: width + width % 2,
                        height: height + height % 2,
                        pixels: Pixels::Gpu(texture),
                    });
                    return Ok(());
                }
                Err(err) => {
                    tracing::warn!(
                        "copying the frame in video memory failed, reading frames back: {err}"
                    );
                    self.flags.gpu.store(false, Ordering::Relaxed);
                }
            }
        }
        let mut buffer = frame.buffer()?;
        let row_pitch = buffer.row_pitch();
        let bgra = packed_rows(buffer.as_raw_buffer(), width, height, row_pitch);
        self.flags.sink.store(pad_to_even(width, height, bgra));
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.flags.closed.store(true, Ordering::Relaxed);
        Ok(())
    }
}
