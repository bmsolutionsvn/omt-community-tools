//! Graphics Capture handler feeding a [`LatestFrame`] sink.

use std::sync::Arc;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;

use crate::frame::{FrameData, LatestFrame, packed_rows, pad_to_even};

/// Failure while turning a captured frame into a [`FrameData`].
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Reading the frame's pixels failed.
    #[error("frame buffer: {0}")]
    Frame(#[from] windows_capture::frame::Error),
}

/// Copies each captured frame into the shared sink; the newest frame wins.
pub struct CaptureHandler {
    sink: Arc<LatestFrame>,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = Arc<LatestFrame>;
    type Error = CaptureError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { sink: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let timestamp = frame.timestamp().Duration;
        let mut buffer = frame.buffer()?;
        let width = buffer.width();
        let height = buffer.height();
        let row_pitch = buffer.row_pitch();
        let bgra = packed_rows(buffer.as_raw_buffer(), width, height, row_pitch);
        self.sink.store(pad_to_even(FrameData {
            width,
            height,
            timestamp,
            bgra,
        }));
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
