from typing import List
from pydantic import BaseModel


class RvoEvent(BaseModel):
    event_type: str
    ts_ns: int
    confidence: float


class ClipWindow(BaseModel):
    start: int
    end: int


class RvoClipMetadata(BaseModel):
    event_type: str
    event_ts_ns: int
    clip_window_ns: ClipWindow
    frames_total: int
    frames_written: int
    frame_ts_ns: List[int]
    encode_ms: int
