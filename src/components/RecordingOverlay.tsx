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
};

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

  if (overlayState.state === "idle" || overlayState.hidden) {
    return null;
  }

  return (
    <div className="recording-overlay-root">
      <div className="recording-overlay-panel">
        <span className="recording-overlay-time">
          {formatDuration(overlayState.duration)}
        </span>

        {overlayState.state === "recording" && (
          <button className="btn-ghost" onClick={handlePause}>
            Pause
          </button>
        )}

        {overlayState.state === "paused" && (
          <button className="btn-primary" onClick={handleResume}>
            Resume
          </button>
        )}

        <button
          className="btn-danger"
          onClick={handleStop}
          disabled={overlayState.state === "stopping"}
        >
          {overlayState.state === "stopping" ? "Saving..." : "Stop"}
        </button>
      </div>
    </div>
  );
}
