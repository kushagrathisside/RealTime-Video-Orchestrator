#[cfg(test)]
mod scenarios {
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    use rvo_detector::detector::DetectorNode;
    use rvo_detector::{load::LoadDetector, DummyDetector};
    use rvo_events::{CompareOp, Condition, EventDefinition, EventType, SignalPredicate};
    use rvo_signals::store::SignalType;
    use rvo_testkit::{
        ChainedDetector, FailingDetector, MetricsSnapshot, PipelineBuilder, ScriptEntry,
        ScriptedDetector, SyntheticCamera, SyntheticPattern,
    };

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn scripted_detector(entries: Vec<ScriptEntry>) -> ScriptedDetector {
        ScriptedDetector::new("scripted", 500.0, entries)
    }

    fn event_def(condition: Condition, duration_ns: u64, cooldown_ns: u64) -> EventDefinition {
        EventDefinition {
            event_type: EventType::DummyEvent,
            condition,
            duration_ns,
            cooldown_ns,
        }
    }

    #[test]
    fn happy_path_event_fires_after_duration() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let before = MetricsSnapshot::capture();

        let detector = scripted_detector(vec![ScriptEntry {
            tick: 0,
            signal_type: SignalType::Dummy,
            value: 1,
            ttl_ns: 5_000_000_000,
        }]);

        let mut pipeline = PipelineBuilder::new()
            .detector(detector)
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                50_000_000,
                5_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(120));

        let after = MetricsSnapshot::capture();
        let delta = after.delta_since(&before);

        pipeline.event_capture.assert_count(1);
        let event = pipeline.event_capture.events()[0].clone();
        assert!((event.confidence - 1.0).abs() <= 0.05);
        assert_eq!(delta.events_emitted, 1);
    }

    #[test]
    fn duration_not_met_emits_no_event() {
        let _guard = TEST_MUTEX.lock().unwrap();

        let detector = scripted_detector(vec![ScriptEntry {
            tick: 0,
            signal_type: SignalType::Dummy,
            value: 1,
            ttl_ns: 200_000_000,
        }]);

        let mut pipeline = PipelineBuilder::new()
            .detector(detector)
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                500_000_000,
                5_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(150));
        pipeline.event_capture.assert_empty();
    }

    #[test]
    fn signal_break_resets_state_machine() {
        let _guard = TEST_MUTEX.lock().unwrap();

        let detector = scripted_detector(vec![
            ScriptEntry {
                tick: 0,
                signal_type: SignalType::Dummy,
                value: 1,
                ttl_ns: 100_000_000,
            },
            ScriptEntry {
                tick: 10,
                signal_type: SignalType::Dummy,
                value: 0,
                ttl_ns: 100_000_000,
            },
            ScriptEntry {
                tick: 20,
                signal_type: SignalType::Dummy,
                value: 1,
                ttl_ns: 10_000_000_000,
            },
        ]);

        let mut pipeline = PipelineBuilder::new()
            .detector(detector)
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                200_000_000,
                2_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(500));
        let count = pipeline.event_capture.count();
        assert!(count <= 1, "expected at most one event, got {}", count);
        if count == 1 {
            assert!(pipeline.event_capture.events()[0].ts_ns > 200_000_000);
        }
    }

    #[test]
    fn cooldown_is_enforced() {
        let _guard = TEST_MUTEX.lock().unwrap();

        let detector = scripted_detector(vec![ScriptEntry {
            tick: 0,
            signal_type: SignalType::Dummy,
            value: 1,
            ttl_ns: 10_000_000_000,
        }]);

        let mut pipeline = PipelineBuilder::new()
            .detector(detector)
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                0,
                300_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(50));
        pipeline.event_capture.assert_count(1);
        pipeline.event_capture.clear();

        pipeline.run_for(Duration::from_millis(100));
        pipeline.event_capture.assert_empty();

        pipeline.run_for(Duration::from_millis(320));
        pipeline.event_capture.assert_count(1);
    }

    #[test]
    fn multi_event_definitions_fire_independently() {
        let _guard = TEST_MUTEX.lock().unwrap();

        let detector_a = ScriptedDetector::new(
            "dummy",
            500.0,
            vec![ScriptEntry {
                tick: 0,
                signal_type: SignalType::Dummy,
                value: 1,
                ttl_ns: 10_000_000_000,
            }],
        );
        let detector_b = ScriptedDetector::new(
            "motion",
            500.0,
            vec![ScriptEntry {
                tick: 0,
                signal_type: SignalType::MotionLevel,
                value: 100,
                ttl_ns: 10_000_000_000,
            }],
        );

        let mut pipeline = PipelineBuilder::new()
            .detector(detector_a)
            .detector(detector_b)
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                0,
                10_000_000_000,
            ))
            .event(event_def(
                Condition::single_gte(SignalType::MotionLevel, 50),
                0,
                10_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(50));
        pipeline.event_capture.assert_count(2);
        assert!(pipeline
            .event_capture
            .events()
            .iter()
            .all(|event| event.event_type == EventType::DummyEvent));
    }

    #[test]
    fn all_condition_requires_both_signals() {
        let _guard = TEST_MUTEX.lock().unwrap();

        let all_condition = Condition::All(vec![
            SignalPredicate {
                signal_type: SignalType::Dummy,
                op: CompareOp::Gte,
                value: 1,
            },
            SignalPredicate {
                signal_type: SignalType::FacePresent,
                op: CompareOp::Eq,
                value: 1,
            },
        ]);

        let mut pipeline_missing = PipelineBuilder::new()
            .detector(scripted_detector(vec![ScriptEntry {
                tick: 0,
                signal_type: SignalType::Dummy,
                value: 1,
                ttl_ns: 10_000_000_000,
            }]))
            .event(event_def(all_condition.clone(), 0, 10_000_000_000))
            .build();

        pipeline_missing.run_for(Duration::from_millis(100));
        pipeline_missing.event_capture.assert_empty();

        let mut pipeline_present = PipelineBuilder::new()
            .detector(ScriptedDetector::new(
                "dummy",
                500.0,
                vec![ScriptEntry {
                    tick: 0,
                    signal_type: SignalType::Dummy,
                    value: 1,
                    ttl_ns: 10_000_000_000,
                }],
            ))
            .detector(ScriptedDetector::new(
                "face",
                500.0,
                vec![ScriptEntry {
                    tick: 0,
                    signal_type: SignalType::FacePresent,
                    value: 1,
                    ttl_ns: 10_000_000_000,
                }],
            ))
            .event(event_def(all_condition, 0, 10_000_000_000))
            .build();

        pipeline_present.run_for(Duration::from_millis(100));
        pipeline_present.event_capture.assert_count(1);
    }

    #[test]
    fn any_condition_fires_on_first_match() {
        let _guard = TEST_MUTEX.lock().unwrap();

        let detector = scripted_detector(vec![ScriptEntry {
            tick: 0,
            signal_type: SignalType::MotionLevel,
            value: 200,
            ttl_ns: 10_000_000_000,
        }]);

        let mut pipeline = PipelineBuilder::new()
            .detector(detector)
            .event(event_def(
                Condition::Any(vec![
                    SignalPredicate {
                        signal_type: SignalType::Dummy,
                        op: CompareOp::Gte,
                        value: 1,
                    },
                    SignalPredicate {
                        signal_type: SignalType::MotionLevel,
                        op: CompareOp::Gte,
                        value: 50,
                    },
                ]),
                0,
                10_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(50));
        pipeline.event_capture.assert_count(1);
    }

    #[test]
    fn dependency_gating_allows_chained_detector() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let before = MetricsSnapshot::capture();

        let mut pipeline = PipelineBuilder::new()
            .detector(ScriptedDetector::new(
                "dummy",
                500.0,
                vec![ScriptEntry {
                    tick: 0,
                    signal_type: SignalType::Dummy,
                    value: 1,
                    ttl_ns: 2_000_000_000,
                }],
            ))
            .detector(ChainedDetector::new(
                "face-from-dummy",
                500.0,
                SignalType::Dummy,
                SignalType::FacePresent,
                1,
                2_000_000_000,
            ))
            .event(event_def(
                Condition::single_gte(SignalType::FacePresent, 1),
                0,
                10_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(200));

        let after = MetricsSnapshot::capture();
        let delta = after.delta_since(&before);
        pipeline.event_capture.assert_count(1);
        assert!(delta.detector_execs >= 2);
    }

    #[test]
    fn load_shedding_produces_skips() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let before = MetricsSnapshot::capture();

        let mut pipeline = PipelineBuilder::new()
            .detector(scripted_detector(vec![ScriptEntry {
                tick: 0,
                signal_type: SignalType::Dummy,
                value: 1,
                ttl_ns: 10_000_000_000,
            }]))
            .detectors(vec![
                Box::new(LoadDetector::new(200_000_000)) as Box<dyn DetectorNode>
            ])
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                0,
                10_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(900));

        let after = MetricsSnapshot::capture();
        let delta = after.delta_since(&before);

        assert!(delta.detector_skips > 0);
        assert!(delta.events_emitted >= 1);
    }

    #[test]
    fn failed_health_disables_detector() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let before = MetricsSnapshot::capture();

        let mut pipeline = PipelineBuilder::new()
            .detectors(vec![
                Box::new(FailingDetector::new(Box::new(DummyDetector), 2)) as Box<dyn DetectorNode>,
            ])
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                0,
                1_000_000_000,
            ))
            .build();

        pipeline.run_for(Duration::from_millis(500));

        let after = MetricsSnapshot::capture();
        let delta = after.delta_since(&before);

        assert_eq!(delta.detector_failures, 1);
        assert_eq!(delta.detector_execs, 3);
    }

    #[test]
    fn frame_drops_under_camera_pressure() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let before = MetricsSnapshot::capture();

        let mut pipeline = PipelineBuilder::new().frame_channel_capacity(5).build();

        SyntheticCamera::new(
            64,
            48,
            300.0,
            SyntheticPattern::SolidColor {
                r: 10,
                g: 20,
                b: 30,
            },
        )
        .start(pipeline.frame_tx.clone());

        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(200) {
            pipeline.scheduler.tick();
            thread::sleep(Duration::from_millis(30));
        }

        let after = MetricsSnapshot::capture();
        let delta = after.delta_since(&before);

        assert!(
            delta.frame_drops > 0,
            "expected frame drops, got {:?}",
            delta
        );
        assert!(delta.scheduler_ticks > 0);
    }

    #[test]
    fn post_roll_frames_are_captured() {
        let _guard = TEST_MUTEX.lock().unwrap();

        let mut pipeline = PipelineBuilder::new()
            .detector(scripted_detector(vec![ScriptEntry {
                tick: 0,
                signal_type: SignalType::Dummy,
                value: 1,
                ttl_ns: 10_000_000_000,
            }]))
            .event(event_def(
                Condition::single_gte(SignalType::Dummy, 1),
                100_000_000,
                10_000_000_000,
            ))
            .clip_window(Duration::from_millis(200), Duration::from_millis(200))
            .build();

        SyntheticCamera::new(
            64,
            48,
            30.0,
            SyntheticPattern::Alternating {
                color_a: (0, 0, 0),
                color_b: (255, 255, 255),
                period_frames: 15,
            },
        )
        .start(pipeline.frame_tx.clone());

        pipeline.run_for(Duration::from_millis(180));
        pipeline.event_capture.assert_count(1);

        thread::sleep(Duration::from_millis(300));

        let mut jobs = Vec::new();
        while let Ok((job, frames)) = pipeline.clip_rx.try_recv() {
            jobs.push((job, frames));
        }

        assert!(!jobs.is_empty(), "expected at least one clip job");
        let (job, frames) = &jobs[0];
        assert!(job.start_ts < job.end_ts);
        assert!(!frames.is_empty(), "expected captured frames in clip job");
    }
}
