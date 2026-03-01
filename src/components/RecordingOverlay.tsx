import { useCallback, useEffect, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  RECORDING_OVERLAY_ACTION_EVENT,
  RECORDING_OVERLAY_UPDATE_EVENT,
  type RecordingOverlayAction,
  type RecordingOverlayActionPayload,
  type RecordingOverlayUpdatePayload,
} from "../recordingOverlay";
import "./RecordingOverlay.css";

function formatDuration(secs: number): string {
  const m = Math.floor(secs / 60)
    .toString()
    .padStart(2, "0");
  const s = (secs % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

const INITIAL_OVERLAY_STATE: RecordingOverlayUpdatePayload = {
  recordingId: null,
  state: "idle",
  duration: 0,
  hidden: false,
  showCursor: true,
};

function PauseIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <rect x="4.6" y="4.2" width="3.9" height="11.6" rx="1.1" fill="currentColor" />
      <rect x="11.5" y="4.2" width="3.9" height="11.6" rx="1.1" fill="currentColor" />
    </svg>
  );
}

function ResumeIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path d="M6.2 4.7 15 10l-8.8 5.3V4.7Z" fill="currentColor" />
    </svg>
  );
}

function StopIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <rect x="5" y="5" width="10" height="10" rx="2.2" fill="currentColor" />
    </svg>
  );
}

function CursorOnIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path
        d="M10 4.4c-3.7 0-6.4 2.5-7.8 5.6 1.4 3.1 4.1 5.6 7.8 5.6s6.4-2.5 7.8-5.6c-1.4-3.1-4.1-5.6-7.8-5.6Zm0 8.4a2.8 2.8 0 1 1 0-5.6 2.8 2.8 0 0 1 0 5.6Z"
        fill="currentColor"
      />
    </svg>
  );
}

function CursorOffIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path
        d="M3.7 3.1a.8.8 0 1 0-1.1 1.1l2 2C3.5 7.2 2.6 8.5 2 10c1.4 3.1 4.1 5.6 7.8 5.6 1.7 0 3.1-.5 4.3-1.3l2.2 2.2a.8.8 0 1 0 1.1-1.1l-13.7-13.3Zm6.1 11c-2.8 0-4.9-1.8-6.2-4.1.5-1 1.1-1.9 1.9-2.6l1.6 1.5a2.8 2.8 0 0 0 3.8 3.8l1.6 1.5c-.8.4-1.7.6-2.7.6Zm7.9-4.1a9.8 9.8 0 0 1-2.5 3.2l-1.1-1.1c.8-.6 1.4-1.3 1.9-2.1-1.2-2.3-3.4-4.1-6.2-4.1-.7 0-1.4.1-2 .3L6.6 5.1c1-.5 2-.7 3.2-.7 3.7 0 6.4 2.5 7.8 5.6Z"
        fill="currentColor"
      />
    </svg>
  );
}

export default function RecordingOverlay() {
  const [overlayState, setOverlayState] = useState<RecordingOverlayUpdatePayload>(
    INITIAL_OVERLAY_STATE
  );

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.overflow = "hidden";
    const root = document.getElementById("root");
    if (root) {
      root.style.background = "transparent";
    }

    const appWindow = getCurrentWebviewWindow();
    let unlisten: (() => void) | null = null;

    void appWindow
      .listen<RecordingOverlayUpdatePayload>(
        RECORDING_OVERLAY_UPDATE_EVENT,
        (event) => {
          setOverlayState(event.payload);
        }
      )
      .then((fn) => {
        unlisten = fn;
      });

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  const emitAction = useCallback(async (action: RecordingOverlayAction) => {
    const payload: RecordingOverlayActionPayload = { action };
    await getCurrentWebviewWindow().emitTo("main", RECORDING_OVERLAY_ACTION_EVENT, payload);
  }, []);

  const handlePause = useCallback(async () => {
    if (overlayState.state !== "recording") {
      return;
    }

    try {
      await emitAction("pause");
      setOverlayState((current) => ({ ...current, state: "paused" }));
    } catch {
      // Main window will keep authoritative state.
    }
  }, [emitAction, overlayState.state]);

  const handleResume = useCallback(async () => {
    if (overlayState.state !== "paused") {
      return;
    }

    try {
      await emitAction("resume");
      setOverlayState((current) => ({ ...current, state: "recording" }));
    } catch {
      // Main window will keep authoritative state.
    }
  }, [emitAction, overlayState.state]);

  const handleStop = useCallback(async () => {
    if (overlayState.state === "stopping") {
      return;
    }

    try {
      setOverlayState((current) => ({ ...current, state: "stopping" }));
      await emitAction("stop");
    } catch {
      // Main window will keep authoritative state.
    }
  }, [emitAction, overlayState.state]);

  const handleToggleCursor = useCallback(async () => {
    if (overlayState.state === "stopping") {
      return;
    }
    const nextShowCursor = !overlayState.showCursor;
    try {
      await getCurrentWebviewWindow().emitTo("main", RECORDING_OVERLAY_ACTION_EVENT, {
        action: "set-cursor-visible",
        showCursor: nextShowCursor,
      } satisfies RecordingOverlayActionPayload);
      setOverlayState((current) => ({ ...current, showCursor: nextShowCursor }));
    } catch {
      // Main window will keep authoritative state.
    }
  }, [overlayState.showCursor, overlayState.state]);

  if (overlayState.state === "idle" || overlayState.hidden) {
    return null;
  }

  return (
    <div className="recording-overlay-root">
      <div className="recording-overlay-panel">
        <span className="recording-overlay-time">
          {formatDuration(overlayState.duration)}
        </span>

        <button
          className={`recording-overlay-icon-btn ${
            overlayState.showCursor
              ? "recording-overlay-icon-btn--cursor-on"
              : "recording-overlay-icon-btn--cursor-off"
          }`}
          onClick={handleToggleCursor}
          aria-label={overlayState.showCursor ? "Hide cursor" : "Show cursor"}
          title={overlayState.showCursor ? "Hide cursor + disable auto zoom" : "Show cursor"}
          disabled={overlayState.state === "stopping"}
        >
          {overlayState.showCursor ? <CursorOnIcon /> : <CursorOffIcon />}
        </button>

        {overlayState.state === "recording" && (
          <button
            className="recording-overlay-icon-btn recording-overlay-icon-btn--ghost"
            onClick={handlePause}
            aria-label="Pause recording"
            title="Pause"
          >
            <PauseIcon />
          </button>
        )}

        {overlayState.state === "paused" && (
          <button
            className="recording-overlay-icon-btn recording-overlay-icon-btn--primary"
            onClick={handleResume}
            aria-label="Resume recording"
            title="Resume"
          >
            <ResumeIcon />
          </button>
        )}

        <button
          className="recording-overlay-icon-btn recording-overlay-icon-btn--danger"
          onClick={handleStop}
          disabled={overlayState.state === "stopping"}
          aria-label={overlayState.state === "stopping" ? "Stopping recording" : "Stop recording"}
          title={overlayState.state === "stopping" ? "Stopping..." : "Stop"}
        >
          <StopIcon />
        </button>
      </div>
    </div>
  );
}
