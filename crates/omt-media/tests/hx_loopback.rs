//! End-to-end check of the HX send and receive paths through `omt-media`.
//!
//! Runs only when the optional OMT-HX library is present (`OMTHX_LIBRARY`, beside the test binary,
//! or on the loader path). The sender and receiver live in one process, which libomt's probe cannot
//! see, so the test names the mode instead of probing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use omt_media::{
    BufferSettings, ConnectOptions, HxSendConfig, PlayoutMode, Quality, ReceiveWorker, SendSession,
    SendSessionConfig, VideoDecodePath,
};

#[test]
fn hx_send_is_received_by_the_worker() {
    if !omthx::available() {
        return;
    }
    let (width, height) = (320i32, 180i32);
    let name = format!("OMT HX loopback {}", std::process::id());
    let frame = Arc::new(vec![128u8; (width * 2 * height) as usize]);
    let provider = {
        let frame = Arc::clone(&frame);
        Arc::new(move |_| frame.as_ref().clone())
    };
    let config = SendSessionConfig {
        name,
        width,
        height,
        fps_n: 30,
        fps_d: 1,
        hx: Some(HxSendConfig::default()),
        ..Default::default()
    };
    let mut send = SendSession::start(config, provider).expect("start HX sender");
    let address = send.address().expect("HX sender has an address");

    let worker = ReceiveWorker::spawn();
    worker.set_buffer(BufferSettings {
        mode: PlayoutMode::Immediate,
        ..Default::default()
    });
    worker.connect_with(ConnectOptions {
        url: format!("omt://{address}"),
        address: Some(address),
        addresses: Vec::new(),
        mode: None,
        quality: Quality::Default,
        preview: false,
        video_decode: VideoDecodePath::Cpu,
    });

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut frame = None;
    while Instant::now() < deadline {
        frame = worker.latest().wait_take(Duration::from_millis(100));
        if frame.is_some() {
            break;
        }
    }
    send.stop();
    worker.disconnect();

    let frame = frame.expect("no HX frame reached the receive worker");
    assert_eq!(frame.width, width as u32);
    assert_eq!(frame.height, height as u32);
    assert_eq!(frame.bgra.len(), (width * height * 4) as usize);
}

/// Manual check against a running HX source: set `OMTHX_TEST_SOURCE` to `HOST (Name)` and run
/// `cargo test -p omt-media --test hx_loopback external_source -- --ignored`.
#[test]
#[ignore = "needs a running HX source in OMTHX_TEST_SOURCE"]
fn external_hx_source_is_received() {
    let address = std::env::var("OMTHX_TEST_SOURCE").expect("OMTHX_TEST_SOURCE");
    assert!(
        omthx::probe(&address, Duration::from_millis(2000))
            .is_some_and(|p| p.mode == omthx::Mode::Hx),
        "no HX picture from {address}"
    );
    let worker = ReceiveWorker::spawn();
    worker.set_buffer(BufferSettings {
        mode: PlayoutMode::Immediate,
        ..Default::default()
    });
    worker.connect_with(ConnectOptions {
        url: format!("omt://{address}"),
        address: Some(address),
        addresses: Vec::new(),
        mode: None,
        quality: Quality::Default,
        preview: false,
        video_decode: VideoDecodePath::Cpu,
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut frame = None;
    while Instant::now() < deadline && frame.is_none() {
        frame = worker.latest().wait_take(Duration::from_millis(100));
    }
    worker.disconnect();
    let frame = frame.expect("no HX frame reached the receive worker");
    assert!(!frame.bgra.is_empty());
    assert_eq!(frame.bgra.len(), (frame.width * frame.height * 4) as usize);
}
