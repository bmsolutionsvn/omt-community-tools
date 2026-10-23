//! Frames from the capture API.

use std::sync::Arc;

use parking_lot::Mutex;

/// One BGRA frame, unpadded, timestamped in 100 ns units.
#[derive(Clone)]
pub struct FrameData {
    /// Frame width in pixels, always even after `pad_to_even`.
    pub width: u32,
    /// Frame height in pixels, always even after `pad_to_even`.
    pub height: u32,
    /// Capture timestamp in 100 ns units.
    pub timestamp: i64,
    /// `width * height * 4` bytes, BGRA order.
    pub bgra: Vec<u8>,
}

/// Return `frame` unchanged when even, otherwise copy it into an even-sized
/// black-padded frame: OMT-HX rejects odd dimensions.
#[must_use]
pub fn pad_to_even(frame: FrameData) -> FrameData {
    if frame.width.is_multiple_of(2) && frame.height.is_multiple_of(2) {
        return frame;
    }
    let width = frame.width + frame.width % 2;
    let height = frame.height + frame.height % 2;
    let row = frame.width as usize * 4;
    let padded_row = width as usize * 4;
    let mut bgra = vec![0u8; padded_row * height as usize];
    for y in 0..frame.height as usize {
        bgra[y * padded_row..y * padded_row + row]
            .copy_from_slice(&frame.bgra[y * row..y * row + row]);
    }
    FrameData {
        width,
        height,
        timestamp: frame.timestamp,
        bgra,
    }
}

/// Copy `width * 4` bytes of each row, dropping DXGI row padding.
///
/// `windows_capture::frame::FrameBuffer::as_nopadding_buffer` is unusable here:
/// it resizes its scratch vec only when `capacity < frame_size` and then slices
/// `[0..frame_size]`, so a larger padded frame (above 4K) can panic.
pub(crate) fn packed_rows(raw: &[u8], width: u32, height: u32, row_pitch: u32) -> Vec<u8> {
    let row = width as usize * 4;
    let pitch = row_pitch as usize;
    let mut bgra = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let start = y * pitch;
        bgra.extend_from_slice(&raw[start..start + row]);
    }
    bgra
}

/// Holds only the newest frame: the sender thread drains it, the capture
/// thread overwrites it, and a slow sender never queues stale frames.
#[derive(Default)]
pub struct LatestFrame {
    slot: Mutex<Option<Arc<FrameData>>>,
}

impl LatestFrame {
    /// Replace the pending frame.
    pub fn store(&self, frame: FrameData) {
        self.store_from_arc(Arc::new(frame));
    }

    /// Replace the pending frame with one already shared.
    pub fn store_from_arc(&self, frame: Arc<FrameData>) {
        *self.slot.lock() = Some(frame);
    }

    /// Take the pending frame, if the capture thread stored a new one.
    pub fn take_latest(&self) -> Option<Arc<FrameData>> {
        self.slot.lock().take()
    }
}

#[test]
fn copies_rows_dropping_pitch_padding() {
    // 3-wide rows are 12 bytes; give them 16-byte pitch and 4 garbage bytes.
    let mut raw = Vec::new();
    for y in 0..3u8 {
        for x in 0..3u8 {
            raw.extend_from_slice(&[x, y, 7, 255]);
        }
        raw.extend_from_slice(&[0xAA; 4]);
    }
    let out = packed_rows(&raw, 3, 3, 16);
    assert_eq!(out.len(), 3 * 3 * 4);
    assert_eq!(&out[0..4], &[0, 0, 7, 255]);
    assert_eq!(&out[12..16], &[0, 1, 7, 255]);
    assert!(!out.contains(&0xAA));
}

#[test]
fn copies_even_frame_unchanged() {
    let raw: Vec<u8> = (0..16).collect();
    assert_eq!(packed_rows(&raw, 2, 2, 8), raw);
}

#[test]
fn pads_odd_frames_with_black() {
    let mut bgra = Vec::new();
    for y in 0..3u8 {
        for x in 0..3u8 {
            bgra.extend_from_slice(&[x, y, 7, 255]);
        }
    }
    let out = pad_to_even(FrameData {
        width: 3,
        height: 3,
        timestamp: 0,
        bgra,
    });
    assert_eq!((out.width, out.height), (4, 4));
    assert_eq!(out.bgra.len(), 4 * 4 * 4);
    assert_eq!(&out.bgra[0..4], &[0, 0, 7, 255]);
    assert_eq!(&out.bgra[12..16], &[0, 0, 0, 0]);
    assert_eq!(&out.bgra[16..20], &[0, 1, 7, 255]);
    assert!(out.bgra[48..].iter().all(|b| *b == 0));
}

#[test]
fn latest_frame_replaces_older() {
    let sink = LatestFrame::default();
    let first = Arc::new(FrameData {
        width: 2,
        height: 2,
        timestamp: 1,
        bgra: vec![1; 16],
    });
    let second = Arc::new(FrameData {
        width: 2,
        height: 2,
        timestamp: 2,
        bgra: vec![2; 16],
    });
    sink.store_from_arc(first);
    sink.store_from_arc(second);
    assert_eq!(sink.take_latest().unwrap().timestamp, 2);
    assert!(sink.take_latest().is_none());
}
