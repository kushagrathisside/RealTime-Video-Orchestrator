//! End-to-end integration test for the gRPC remote detector.
//!
//! Stands up an in-process tonic server that returns a canned `PersonDetected`
//! signal, wires a [`RemoteGrpcDetector`] pointed at it through the testkit
//! [`PipelineBuilder`], feeds real frames, and asserts an event fires. This is
//! the CI guarantee for the remote path — deterministic, no Python, no models.

use std::thread;
use std::time::{Duration, Instant};

use opencv::core::{Mat, Scalar, CV_8UC3};

use rvo_buffer::Frame;
use rvo_events::{Condition, EventDefinition, EventType};
use rvo_remote::pb::detector_server::{Detector, DetectorServer};
use rvo_remote::pb::{DetectRequest, DetectResponse, SignalOut};
use rvo_remote::RemoteGrpcDetector;
use rvo_signals::store::SignalType;
use rvo_testkit::{MetricsSnapshot, PipelineBuilder};

const ADDR: &str = "127.0.0.1:50337";

#[derive(Default)]
struct MockModel;

#[tonic::async_trait]
impl Detector for MockModel {
    async fn detect(
        &self,
        _request: tonic::Request<DetectRequest>,
    ) -> Result<tonic::Response<DetectResponse>, tonic::Status> {
        Ok(tonic::Response::new(DetectResponse {
            signals: vec![SignalOut {
                signal_type: "PersonDetected".to_string(),
                value: 1,
                ttl_ns: 1_000_000_000,
            }],
        }))
    }
}

fn start_mock_server() {
    thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("server runtime");
        rt.block_on(async {
            let addr = ADDR.parse().unwrap();
            tonic::transport::Server::builder()
                .add_service(DetectorServer::new(MockModel))
                .serve(addr)
                .await
                .expect("mock server");
        });
    });
}

fn solid_frame(id: u64) -> Frame {
    // A real (non-empty) Mat so the worker's JPEG encode succeeds.
    let image =
        Mat::new_rows_cols_with_default(48, 64, CV_8UC3, Scalar::all(255.0)).expect("alloc mat");
    Frame {
        ts: Instant::now(),
        id,
        image,
    }
}

#[test]
fn remote_detector_drives_event_end_to_end() {
    start_mock_server();
    // Give the server a moment to bind before the worker dials.
    thread::sleep(Duration::from_millis(300));

    let before = MetricsSnapshot::capture();

    let detector = RemoteGrpcDetector::new(
        "test-remote",
        format!("http://{ADDR}"),
        SignalType::PersonDetected,
        50.0,          // max_fps -> worker polls every ~20ms
        200,           // timeout_ms
        1_000_000_000, // ttl_ns (1s)
    );

    let mut pipeline = PipelineBuilder::new()
        .detector(detector)
        .event(EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::single_gte(SignalType::PersonDetected, 1),
            duration_ns: 50_000_000, // 50ms sustained
            cooldown_ns: 1_000_000_000,
        })
        .build();

    // Seed frames; execute() republishes the newest frame each tick, so the
    // worker keeps getting one to send.
    for id in 0..5 {
        pipeline.inject_frame(solid_frame(id));
    }

    pipeline.run_for(Duration::from_millis(800));

    let delta = MetricsSnapshot::capture().delta_since(&before);

    assert!(
        pipeline.event_capture.count() >= 1,
        "expected at least one event from the remote detector"
    );
    assert!(
        delta.detector_execs > 0,
        "expected the remote detector to have executed"
    );
}
