import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { primaryMonitor } from "@tauri-apps/api/window";
import { WebviewWindow, getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  RECORDING_OVERLAY_ACTION_EVENT,
  RECORDING_OVERLAY_UPDATE_EVENT,
  RECORDING_OVERLAY_WINDOW_LABEL,
  type OverlayRecordingState,
  type RecordingOverlayActionPayload,
  type RecordingOverlayUpdatePayload,
} from "../recordingOverlay";
import "./Record.css";

type RecordState = OverlayRecordingState;
type AutoZoomTriggerMode = "single-click" | "multi-click-window" | "ctrl-click";
type RecordingQuality = "low" | "balanced" | "high";
type RecordingFps = 30 | 60;

interface StartRecordingOptions {
  autoZoomTriggerMode: AutoZoomTriggerMode;
  quality: RecordingQuality;
  targetFps: RecordingFps;
}

interface NativePreviewFrame {
  dataUrl: string;
  width: number;
  height: number;
  sequence: number;
}

interface RecordScreenProps {
  isActive: boolean;
}

function formatDuration(secs: number): string {
  const m = Math.floor(secs / 60)
    .toString()
    .padStart(2, "0");
  const s = (secs % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

async function waitForWebviewWindowCreated(webviewWindow: WebviewWindow): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    let settled = false;
    const finish = (cb: () => void) => {
      if (settled) {
        return;
      }
      settled = true;
      cb();
    };

    void webviewWindow.once("tauri://created", () => finish(resolve));
    void webviewWindow.once("tauri://error", (event) =>
      finish(() => reject(new Error(String(event.payload))))
    );

    // In some runs the window may already be created before listeners are attached.
    setTimeout(() => finish(resolve), 1200);
  });
}

export default function RecordScreen({ isActive }: RecordScreenProps) {
  const [state, setState] = useState<RecordState>("idle");
  const [recordingId, setRecordingId] = useState<string | null>(null);
  const [duration, setDuration] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [isPreviewLoading, setIsPreviewLoading] = useState(false);
  const [previewImageSrc, setPreviewImageSrc] = useState<string | null>(null);
  const [isCtrlPressed, setIsCtrlPressed] = useState(false);
  const [autoZoomTriggerMode, setAutoZoomTriggerMode] = useState<AutoZoomTriggerMode>("single-click");
  const [recordingQuality, setRecordingQuality] = useState<RecordingQuality>("high");
  const [recordingFps, setRecordingFps] = useState<RecordingFps>(60);

  const tickerRef = useRef<number | null>(null);
  const elapsedBeforePauseMsRef = useRef(0);
  const resumedAtMsRef = useRef<number | null>(null);
  const stateRef = useRef<RecordState>("idle");
  const previewPollRef = useRef<number | null>(null);
  const previewRequestInFlightRef = useRef(false);
  const previewSequenceRef = useRef(0);
  const isPreviewLoadingRef = useRef(false);
  const ctrlPollRef = useRef<number | null>(null);
  const ctrlRequestInFlightRef = useRef(false);
  const overlayWindowRef = useRef<WebviewWindow | null>(null);
  const overlayHiddenRef = useRef<boolean | null>(null);

  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  const stopTicker = useCallback(() => {
    if (tickerRef.current !== null) {
      cancelAnimationFrame(tickerRef.current);
      tickerRef.current = null;
    }
  }, []);

  const updateDurationFromClock = useCallback((now: number) => {
    let elapsedMs = elapsedBeforePauseMsRef.current;
    if (stateRef.current === "recording" && resumedAtMsRef.current !== null) {
      elapsedMs += now - resumedAtMsRef.current;
    }
    const elapsedSec = Math.floor(elapsedMs / 1000);
    setDuration((current) => (current === elapsedSec ? current : elapsedSec));
  }, []);

  const startTicker = useCallback(() => {
    stopTicker();
    const tick = () => {
      updateDurationFromClock(performance.now());
      tickerRef.current = requestAnimationFrame(tick);
    };
    tickerRef.current = requestAnimationFrame(tick);
  }, [stopTicker, updateDurationFromClock]);

  const closeOverlayWindow = useCallback(async () => {
    try {
      const overlayWindow =
        overlayWindowRef.current ??
        (await WebviewWindow.getByLabel(RECORDING_OVERLAY_WINDOW_LABEL));
      if (overlayWindow) {
        await overlayWindow.close();
      }
    } catch {
      // Window may already be closed.
    } finally {
      overlayWindowRef.current = null;
      overlayHiddenRef.current = null;
    }
  }, []);

  const ensureOverlayWindow = useCallback(async (): Promise<WebviewWindow> => {
    if (overlayWindowRef.current) {
      return overlayWindowRef.current;
    }

    const existing = await WebviewWindow.getByLabel(RECORDING_OVERLAY_WINDOW_LABEL);
    if (existing) {
      overlayWindowRef.current = existing;
      return existing;
    }

    const monitor = await primaryMonitor();
    const overlayWidth = 320;
    const overlayHeight = 60;
    const bottomGap = 24;
    const scaleFactor = monitor?.scaleFactor ?? 1;

    const monitorLogicalWidth = monitor
      ? monitor.size.width / scaleFactor
      : window.innerWidth;
    const monitorLogicalHeight = monitor
      ? monitor.size.height / scaleFactor
      : window.innerHeight;
    const monitorLogicalX = monitor ? monitor.position.x / scaleFactor : 0;
    const monitorLogicalY = monitor ? monitor.position.y / scaleFactor : 0;

    const x = Math.round(
      monitorLogicalX + (monitorLogicalWidth - overlayWidth) / 2
    );
    const y = Math.round(
      monitorLogicalY + monitorLogicalHeight - overlayHeight - bottomGap
    );

    const overlayWindow = new WebviewWindow(RECORDING_OVERLAY_WINDOW_LABEL, {
      title: "Recording Controls",
      width: overlayWidth,
      height: overlayHeight,
      x,
      y,
      decorations: false,
      resizable: false,
      alwaysOnTop: true,
      skipTaskbar: true,
      transparent: true,
      focus: false,
      shadow: false,
      contentProtected: true,
    });

    await waitForWebviewWindowCreated(overlayWindow);
    overlayWindowRef.current = overlayWindow;
    return overlayWindow;
  }, []);

  const emitOverlayUpdate = useCallback(
    async (
      nextState: RecordState,
      nextDuration: number,
      nextRecordingId: string | null,
      hidden: boolean
    ) => {
      if (nextState === "idle") {
        await closeOverlayWindow();
        return;
      }

      if (hidden) {
        if (overlayHiddenRef.current !== true) {
          await closeOverlayWindow();
          overlayHiddenRef.current = true;
        }
        return;
      }

      const overlayWindow = await ensureOverlayWindow();
      try {
        await overlayWindow.setIgnoreCursorEvents(false);
      } catch {
        // Ignore unsupported click-through toggle.
      }
      overlayHiddenRef.current = false;

      const payload: RecordingOverlayUpdatePayload = {
        recordingId: nextRecordingId,
        state: nextState,
        duration: nextDuration,
        hidden,
      };
      await getCurrentWebviewWindow().emitTo(
        overlayWindow.label,
        RECORDING_OVERLAY_UPDATE_EVENT,
        payload
      );
    },
    [closeOverlayWindow, ensureOverlayWindow]
  );

  const stopCtrlPolling = useCallback(() => {
    if (ctrlPollRef.current !== null) {
      window.clearInterval(ctrlPollRef.current);
      ctrlPollRef.current = null;
    }
    ctrlRequestInFlightRef.current = false;
  }, []);

  const pullCtrlState = useCallback(async () => {
    if (ctrlRequestInFlightRef.current) {
      return;
    }
    ctrlRequestInFlightRef.current = true;
    try {
      const pressed = await invoke<boolean>("is_ctrl_pressed");
      setIsCtrlPressed((current) => (current === pressed ? current : pressed));
    } catch {
      // Ctrl hide is best-effort.
    } finally {
      ctrlRequestInFlightRef.current = false;
    }
  }, []);

  const stopPreviewPolling = useCallback(() => {
    if (previewPollRef.current !== null) {
      clearInterval(previewPollRef.current);
      previewPollRef.current = null;
    }
  }, []);

  const fetchPreviewFrame = useCallback(async () => {
    if (previewRequestInFlightRef.current) {
      return;
    }
    previewRequestInFlightRef.current = true;

    try {
      const frame = await invoke<NativePreviewFrame | null>("get_native_preview_frame");
      if (frame && frame.sequence !== previewSequenceRef.current) {
        previewSequenceRef.current = frame.sequence;
        setPreviewImageSrc(frame.dataUrl);
        setPreviewError(null);
      }
    } catch (err) {
      setPreviewError(String(err));
    }
    previewRequestInFlightRef.current = false;
  }, []);

  const stopPreview = useCallback(async () => {
    stopPreviewPolling();
    previewRequestInFlightRef.current = false;
    previewSequenceRef.current = 0;
    isPreviewLoadingRef.current = false;
    setIsPreviewLoading(false);
    setPreviewImageSrc(null);
    try {
      await invoke("stop_native_preview");
    } catch {
      // Preview is best-effort; ignore stop errors.
    }
  }, [stopPreviewPolling]);

  const startPreview = useCallback(async () => {
    if (stateRef.current !== "idle" || isPreviewLoadingRef.current) {
      return;
    }

    isPreviewLoadingRef.current = true;
    setIsPreviewLoading(true);
    setPreviewError(null);
    try {
      await invoke("start_native_preview", { monitorIndex: 0 });
      await fetchPreviewFrame();
      stopPreviewPolling();
      previewPollRef.current = window.setInterval(() => {
        void fetchPreviewFrame();
      }, 1000 / 12);
    } catch (err) {
      setPreviewError(String(err));
      await stopPreview();
    } finally {
      isPreviewLoadingRef.current = false;
      setIsPreviewLoading(false);
    }
  }, [fetchPreviewFrame, stopPreview, stopPreviewPolling]);

  useEffect(() => {
    if (!isActive || state !== "idle") {
      void stopPreview();
      return;
    }

    const timer = window.setTimeout(() => {
      void startPreview();
    }, 140);

    return () => {
      window.clearTimeout(timer);
    };
  }, [isActive, startPreview, state, stopPreview]);

  useEffect(() => {
    if (state === "idle") {
      stopCtrlPolling();
      setIsCtrlPressed(false);
      return;
    }

    void pullCtrlState();
    stopCtrlPolling();
    ctrlPollRef.current = window.setInterval(() => {
      void pullCtrlState();
    }, 80);

    return () => {
      stopCtrlPolling();
    };
  }, [pullCtrlState, state, stopCtrlPolling]);

  useEffect(() => {
    void emitOverlayUpdate(state, duration, recordingId, isCtrlPressed).catch(() => {
      // Overlay is optional; ignore delivery errors.
    });
  }, [duration, emitOverlayUpdate, isCtrlPressed, recordingId, state]);

  useEffect(() => {
    return () => {
      stopTicker();
      stopCtrlPolling();
      void stopPreview();
      void closeOverlayWindow();
    };
  }, [closeOverlayWindow, stopCtrlPolling, stopPreview, stopTicker]);

  const finalizeElapsedBeforePause = useCallback(() => {
    if (resumedAtMsRef.current !== null) {
      const now = performance.now();
      elapsedBeforePauseMsRef.current += now - resumedAtMsRef.current;
      resumedAtMsRef.current = null;
    }
    updateDurationFromClock(performance.now());
  }, [updateDurationFromClock]);

  const handleStart = useCallback(async () => {
    setError(null);
    finalizeElapsedBeforePause();
    setDuration(0);
    elapsedBeforePauseMsRef.current = 0;
    resumedAtMsRef.current = null;

    try {
      await stopPreview();
      const options: StartRecordingOptions = {
        autoZoomTriggerMode,
        quality: recordingQuality,
        targetFps: recordingFps,
      };
      const id = await invoke<string>("start_recording", { monitorIndex: 0, options });
      setRecordingId(id);
      resumedAtMsRef.current = performance.now();
      setState("recording");
      startTicker();
      try {
        await getCurrentWebviewWindow().minimize();
      } catch {
        // Recording should continue even if window minimize is unavailable.
      }
    } catch (err) {
      setState("idle");
      setError(String(err));
    }
  }, [
    autoZoomTriggerMode,
    finalizeElapsedBeforePause,
    recordingFps,
    recordingQuality,
    stopPreview,
    startTicker,
  ]);

  const handlePause = useCallback(async () => {
    if (!recordingId || state !== "recording") {
      return;
    }
    setError(null);

    try {
      await invoke("pause_recording", { recordingId });
      finalizeElapsedBeforePause();
      setState("paused");
    } catch (err) {
      setError(String(err));
    }
  }, [finalizeElapsedBeforePause, recordingId, state]);

  const handleResume = useCallback(async () => {
    if (!recordingId || state !== "paused") {
      return;
    }
    setError(null);

    try {
      await invoke("resume_recording", { recordingId });
      resumedAtMsRef.current = performance.now();
      setState("recording");
    } catch (err) {
      setError(String(err));
    }
  }, [recordingId, state]);

  const handleStop = useCallback(async () => {
    if (!recordingId || state === "stopping") {
      return;
    }
    setState("stopping");
    finalizeElapsedBeforePause();
    stopTicker();

    try {
      await invoke("stop_recording", { recordingId });
      setRecordingId(null);
      setState("idle");
      elapsedBeforePauseMsRef.current = 0;
      resumedAtMsRef.current = null;
      setDuration(0);
    } catch (err) {
      setState("idle");
      setError(String(err));
    }
  }, [finalizeElapsedBeforePause, recordingId, state, stopTicker]);

  useEffect(() => {
    const appWindow = getCurrentWebviewWindow();
    const unlistenPromise = appWindow.listen<RecordingOverlayActionPayload>(
      RECORDING_OVERLAY_ACTION_EVENT,
      (event) => {
        if (event.payload.action === "pause") {
          void handlePause();
          return;
        }
        if (event.payload.action === "resume") {
          void handleResume();
          return;
        }
        if (event.payload.action === "stop") {
          void handleStop();
        }
      }
    );

    return () => {
      void unlistenPromise.then((unlisten) => {
        unlisten();
      });
    };
  }, [handlePause, handleResume, handleStop]);

  const isIdle = state === "idle";
  const statusText =
    state === "idle"
      ? "Ready to record"
      : state === "recording"
      ? `Recording ${formatDuration(duration)}`
      : state === "paused"
      ? `Paused ${formatDuration(duration)}`
      : "Saving...";

  return (
    <div className="record-screen">
      <div className="record-workspace">
        <aside className="record-settings">
          <header className="record-settings-header">
            <h2>Capture Setup</h2>
            <p>Configure trigger, quality, and frame rate before starting.</p>
          </header>

          <section className="record-settings-group">
            <label className="record-field">
              <span className="record-field-label">Auto Zoom Trigger</span>
              <select
                value={autoZoomTriggerMode}
                onChange={(event) => setAutoZoomTriggerMode(event.target.value as AutoZoomTriggerMode)}
                disabled={!isIdle}
              >
                <option value="single-click">1 click</option>
                <option value="multi-click-window">2 clicks in 3 seconds</option>
                <option value="ctrl-click">Ctrl + click</option>
              </select>
            </label>

            <label className="record-field">
              <span className="record-field-label">Recording Quality</span>
              <select
                value={recordingQuality}
                onChange={(event) => setRecordingQuality(event.target.value as RecordingQuality)}
                disabled={!isIdle}
              >
                <option value="low">Low</option>
                <option value="balanced">Balanced</option>
                <option value="high">High</option>
              </select>
            </label>
          </section>

          <section className="record-settings-group">
            <div className="record-field record-field--fps">
              <span className="record-field-label">Capture FPS</span>
              <div className="record-fps-options">
                <button
                  type="button"
                  className={`record-fps-btn ${recordingFps === 30 ? "record-fps-btn--active" : ""}`}
                  data-active={recordingFps === 30}
                  onClick={() => setRecordingFps(30)}
                  disabled={!isIdle}
                >
                  30 FPS
                </button>
                <button
                  type="button"
                  className={`record-fps-btn ${recordingFps === 60 ? "record-fps-btn--active" : ""}`}
                  data-active={recordingFps === 60}
                  onClick={() => setRecordingFps(60)}
                  disabled={!isIdle}
                >
                  60 FPS
                </button>
              </div>
              <small className="record-fps-current">Selected: {recordingFps} FPS</small>
            </div>
          </section>

          <div className="record-settings-footnote">
            <span className="record-chip">Default trigger: 1 click</span>
            <span className="record-chip">Hold Ctrl to hide overlay</span>
          </div>
        </aside>

        <section className="record-stage">
          <header className="record-stage-header">
            <div className="record-stage-title-wrap">
              <h1>Screen Capture</h1>
              <p>Live monitor preview with minimal-latency recording controls.</p>
            </div>
            <div className={`record-status ${state === "recording" ? "record-status--active" : ""}`}>
              <div className="record-indicator" />
              <span>{statusText}</span>
            </div>
          </header>

          <div className="record-stage-toolbar">
            <div className="record-stage-controls">
              {state === "idle" && (
                <button className="btn-action record-btn" onClick={handleStart}>
                  Start Recording
                </button>
              )}
              {state === "recording" && (
                <>
                  <button className="btn-ghost record-btn" onClick={handlePause}>
                    Pause
                  </button>
                  <button className="btn-danger record-btn" onClick={handleStop}>
                    Stop
                  </button>
                </>
              )}
              {state === "paused" && (
                <>
                  <button className="btn-primary record-btn" onClick={handleResume}>
                    Resume
                  </button>
                  <button className="btn-danger record-btn" onClick={handleStop}>
                    Stop
                  </button>
                </>
              )}
              {state === "stopping" && (
                <button className="btn-ghost record-btn" disabled>
                  Saving...
                </button>
              )}
            </div>

            <div className="record-stage-meta">
              <span className="record-meta-label">Session</span>
              <span className="record-meta-value mono">
                {recordingId ? recordingId.slice(0, 8).toUpperCase() : "NOT STARTED"}
              </span>
            </div>
          </div>

          <div className="record-preview-shell">
            {previewImageSrc ? (
              <img src={previewImageSrc} className="record-preview-video" alt="Screen preview" />
            ) : (
              <div className="record-preview-placeholder">
                <strong>Screen Preview</strong>
                <p>
                  {previewError
                    ? previewError
                    : isPreviewLoading
                    ? "Connecting to screen preview..."
                    : "Preview is disabled."}
                </p>
                <button className="btn-ghost" onClick={() => void startPreview()} disabled={isPreviewLoading || !isIdle}>
                  Enable Preview
                </button>
              </div>
            )}
          </div>

          <footer className="record-stage-footer">
            <span className="record-footer-item">
              {isCtrlPressed
                ? "Overlay controls hidden while Ctrl is pressed."
                : "Hold Ctrl to temporarily hide overlay controls."}
            </span>
            <span className="record-footer-item mono">Elapsed {formatDuration(duration)}</span>
          </footer>

          {error && (
            <div className="record-error">
              <strong>Error:</strong> {error}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
