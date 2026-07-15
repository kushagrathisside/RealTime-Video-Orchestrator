export interface RvoEvent {
    event_type: string;
    ts_ns: number;
    confidence: number;
}


export interface RvoClipMetadata {
    event_type: string;
    event_ts_ns: number;

    clip_window_ns: {
        start: number;
        end: number;
    };

    frames_total: number;
    frames_written: number;
    frame_ts_ns: number[];
    encode_ms: number;
}
