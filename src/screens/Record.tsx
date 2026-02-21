import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./Record.css";

type RecordState = "idle" | "recording" | "stopping";

export default function RecordScreen() {
  const [state, setState] = useState<RecordState>("idle");
  const [recordingId, setRecordingId] = useState<string | null>(null);
  const [duration, setDuration] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [startTimer, setStartTimer] = useState<ReturnType<typeof setInterval> | null>(null);

  const handleStart = useCallback(async () => {
    setError(null);
    setState("recording");
    setDuration(0);

    try {
      const id = await invoke<string>("start_recording", { monitorIndex: 0 });
      setRecordingId(id);

      const timer = setInterval(() => {
        setDuration((d) => d + 1);
      }, 1000);
      setStartTimer(timer);
    } catch (err) {
      setState("idle");
      setError(String(err));
    }
  }, []);

  const handleStop = useCallback(async () => {
    if (!recordingId) return;
    setState("stopping");

    if (startTimer) {
      clearInterval(startTimer);
      setStartTimer(null);
    }

    try {
      await invoke("stop_recording", { recordingId });
      setState("idle");
      setRecordingId(null);
    } catch (err) {
      setState("idle");
      setError(String(err));
    }
  }, [recordingId, startTimer]);

  const formatDuration = (secs: number) => {
    const m = Math.floor(secs / 60).toString().padStart(2, "0");
    const s = (secs % 60).toString().padStart(2, "0");
    return `${m}:${s}`;
  };

  return (
    <div className="record-screen">
      <div className="record-header">
        <h1>Record</h1>
        <p className="record-subtitle">Capture your screen without cursor — metadata recorded separately</p>
      </div>

      <div className="record-card">
        <div className={`record-status ${state === "recording" ? "record-status--active" : ""}`}>
          <div className="record-indicator" />
          <span>
            {state === "idle" && "Ready to record"}
            {state === "recording" && `Recording ${formatDuration(duration)}`}
            {state === "stopping" && "Stopping..."}
          </span>
        </div>

        <div className="record-controls">
          {state === "idle" && (
            <button className="btn-primary record-btn" onClick={handleStart}>
              Start Recording
            </button>
          )}
          {state === "recording" && (
            <button className="btn-danger record-btn" onClick={handleStop}>
              Stop Recording
            </button>
          )}
          {state === "stopping" && (
            <button className="btn-ghost record-btn" disabled>
              Saving...
            </button>
          )}
        </div>

        {recordingId && (
          <div className="record-meta">
            <span className="record-meta-label">Recording ID</span>
            <code className="record-meta-value">{recordingId}</code>
          </div>
        )}

        {error && (
          <div className="record-error">
            <strong>Error:</strong> {error}
          </div>
        )}
      </div>

      <div className="record-info">
        <div className="record-info-item">
          <span className="info-icon">🎬</span>
          <div>
            <strong>Clean video</strong>
            <p>Screen captured without system cursor via WGC</p>
          </div>
        </div>
        <div className="record-info-item">
          <span className="info-icon">📊</span>
          <div>
            <strong>Telemetry</strong>
            <p>Mouse/keyboard events with UI context collected in parallel</p>
          </div>
        </div>
      </div>
    </div>
  );
}
