export const RECORDING_OVERLAY_WINDOW_LABEL = "recording-controls-overlay";
export const RECORDING_OVERLAY_UPDATE_EVENT = "recording-overlay:update";
export const RECORDING_OVERLAY_ACTION_EVENT = "recording-overlay:action";

export type OverlayRecordingState = "idle" | "recording" | "paused" | "stopping";

export interface RecordingOverlayUpdatePayload {
  recordingId: string | null;
  state: OverlayRecordingState;
  duration: number;
  hidden: boolean;
}

export type RecordingOverlayAction = "pause" | "resume" | "stop";

export interface RecordingOverlayActionPayload {
  action: RecordingOverlayAction;
}
