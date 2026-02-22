import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Project } from "../types/project";
import "./Export.css";

interface ProjectListItem {
  id: string;
  name: string;
  createdAt: number;
  durationMs: number;
  videoWidth: number;
  videoHeight: number;
  projectPath: string;
  folderPath: string;
  modifiedTimeMs: number;
}

interface ExportStatus {
  isRunning: boolean;
  progress: number;
  message: string;
  outputPath: string | null;
  error: string | null;
  startedAtMs: number | null;
  finishedAtMs: number | null;
}

const CODEC_OPTIONS = ["h264", "h265", "vp9"] as const;

const DEFAULT_STATUS: ExportStatus = {
  isRunning: false,
  progress: 0,
  message: "Idle",
  outputPath: null,
  error: null,
  startedAtMs: null,
  finishedAtMs: null,
};

function formatDate(ms: number | null): string {
  if (!ms || !Number.isFinite(ms)) {
    return "n/a";
  }
  return new Date(ms).toLocaleString();
}

function formatMs(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const min = Math.floor(total / 60)
    .toString()
    .padStart(2, "0");
  const sec = (total % 60).toString().padStart(2, "0");
  return `${min}:${sec}`;
}

export default function ExportScreen() {
  const [projects, setProjects] = useState<ProjectListItem[]>([]);
  const [selectedProjectPath, setSelectedProjectPath] = useState<string>("");
  const [selectedProjectName, setSelectedProjectName] = useState<string>("");
  const [width, setWidth] = useState(1920);
  const [height, setHeight] = useState(1080);
  const [fps, setFps] = useState(30);
  const [codec, setCodec] = useState<(typeof CODEC_OPTIONS)[number]>("h264");
  const [status, setStatus] = useState<ExportStatus>(DEFAULT_STATUS);
  const [isRefreshingProjects, setIsRefreshingProjects] = useState(false);
  const [isLoadingProject, setIsLoadingProject] = useState(false);
  const [isStartingExport, setIsStartingExport] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);

  const progressPercent = useMemo(
    () => Math.round(Math.min(Math.max(status.progress, 0), 1) * 100),
    [status.progress]
  );

  const refreshProjects = async (autoSelect = false) => {
    setIsRefreshingProjects(true);
    setError(null);
    try {
      const listed = await invoke<ProjectListItem[]>("list_projects");
      setProjects(listed);

      if (listed.length === 0) {
        setSelectedProjectPath("");
        setSelectedProjectName("");
        return;
      }

      if (autoSelect && !selectedProjectPath) {
        const latest = listed[0];
        setSelectedProjectPath(latest.projectPath);
        setSelectedProjectName(latest.name);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsRefreshingProjects(false);
    }
  };

  const loadProjectSettings = async (projectPath: string) => {
    if (!projectPath) {
      return;
    }
    setIsLoadingProject(true);
    setError(null);
    try {
      const loaded = await invoke<Project>("get_project", { projectPath });
      setSelectedProjectName(loaded.name);
      setWidth(loaded.settings.export.width);
      setHeight(loaded.settings.export.height);
      setFps(loaded.settings.export.fps);
      const nextCodec = loaded.settings.export.codec.toLowerCase();
      setCodec(
        CODEC_OPTIONS.includes(nextCodec as (typeof CODEC_OPTIONS)[number])
          ? (nextCodec as (typeof CODEC_OPTIONS)[number])
          : "h264"
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setIsLoadingProject(false);
    }
  };

  const fetchStatus = async () => {
    try {
      const nextStatus = await invoke<ExportStatus>("get_export_status");
      setStatus(nextStatus);
    } catch (err) {
      setError(String(err));
    }
  };

  useEffect(() => {
    void refreshProjects(true);
    void fetchStatus();
  }, []);

  useEffect(() => {
    if (!selectedProjectPath) {
      return;
    }
    void loadProjectSettings(selectedProjectPath);
  }, [selectedProjectPath]);

  useEffect(() => {
    let isCancelled = false;

    const pull = async () => {
      try {
        const next = await invoke<ExportStatus>("get_export_status");
        if (!isCancelled) {
          setStatus(next);
        }
      } catch (err) {
        if (!isCancelled) {
          setError(String(err));
        }
      }
    };

    const intervalMs = status.isRunning ? 350 : 1400;
    const timer = setInterval(() => void pull(), intervalMs);
    void pull();

    return () => {
      isCancelled = true;
      clearInterval(timer);
    };
  }, [status.isRunning]);

  const handleStartExport = async () => {
    if (!selectedProjectPath) {
      setError("Select project for export.");
      return;
    }

    setError(null);
    setInfo(null);
    setIsStartingExport(true);
    try {
      await invoke("start_export", {
        projectPath: selectedProjectPath,
        width,
        height,
        fps,
        codec,
      });
      setInfo("Export started.");
      await fetchStatus();
    } catch (err) {
      setError(String(err));
    } finally {
      setIsStartingExport(false);
    }
  };

  const handleResetStatus = async () => {
    setError(null);
    try {
      await invoke("reset_export_status");
      await fetchStatus();
    } catch (err) {
      setError(String(err));
    }
  };

  return (
    <div className="export-screen">
      <header className="export-header">
        <div className="export-header-copy">
          <h1>Export</h1>
          <p className="export-subtitle">Render the edited timeline to a final video file.</p>
        </div>
        <div className={`export-pill ${status.isRunning ? "export-pill--active" : ""}`}>
          {status.isRunning ? "Rendering" : "Ready"}
        </div>
      </header>

      <div className="export-layout">
        <div className="export-column">
          <section className="export-card">
            <div className="export-card-head">
              <h2>Project</h2>
              <button
                className="btn-ghost"
                onClick={() => void refreshProjects(false)}
                disabled={isRefreshingProjects}
              >
                {isRefreshingProjects ? "Refreshing..." : "Refresh"}
              </button>
            </div>

            <label className="export-field">
              <span>Selected Recording</span>
              <select
                value={selectedProjectPath}
                onChange={(event) => setSelectedProjectPath(event.target.value)}
                disabled={isLoadingProject || projects.length === 0}
              >
                {projects.length === 0 ? (
                  <option value="">No projects</option>
                ) : (
                  projects.map((item) => (
                    <option key={item.projectPath} value={item.projectPath}>
                      {item.name} | {formatMs(item.durationMs)} | {item.videoWidth}x{item.videoHeight}
                    </option>
                  ))
                )}
              </select>
            </label>
          </section>

          <section className="export-card">
            <h2>Output Settings</h2>
            <div className="export-grid">
              <label className="export-field">
                <span>Width</span>
                <input
                  type="number"
                  min={320}
                  max={7680}
                  value={width}
                  onChange={(event) => setWidth(Math.max(320, Number(event.target.value) || 320))}
                />
              </label>
              <label className="export-field">
                <span>Height</span>
                <input
                  type="number"
                  min={240}
                  max={4320}
                  value={height}
                  onChange={(event) => setHeight(Math.max(240, Number(event.target.value) || 240))}
                />
              </label>
              <label className="export-field">
                <span>FPS</span>
                <input
                  type="number"
                  min={10}
                  max={120}
                  value={fps}
                  onChange={(event) => setFps(Math.max(10, Number(event.target.value) || 10))}
                />
              </label>
              <label className="export-field">
                <span>Codec</span>
                <select
                  value={codec}
                  onChange={(event) => setCodec(event.target.value as (typeof CODEC_OPTIONS)[number])}
                >
                  {CODEC_OPTIONS.map((item) => (
                    <option key={item} value={item}>
                      {item}
                    </option>
                  ))}
                </select>
              </label>
            </div>

            <div className="export-actions">
              <button
                className="btn-primary"
                onClick={() => void handleStartExport()}
                disabled={!selectedProjectPath || isStartingExport || status.isRunning}
              >
                {status.isRunning ? "Exporting..." : isStartingExport ? "Starting..." : "Start Export"}
              </button>
              <button className="btn-ghost" onClick={() => void handleResetStatus()} disabled={status.isRunning}>
                Reset Status
              </button>
            </div>
          </section>
        </div>

        <aside className="export-card export-card--status">
          <h2>Status</h2>
          <div className="export-status-row">
            <span className="export-status-label">{status.message || "Idle"}</span>
            <span className="export-status-value">{progressPercent}%</span>
          </div>
          <div className="export-progress">
            <div className="export-progress-fill" style={{ width: `${progressPercent}%` }} />
          </div>
          <div className="export-meta">
            <span>Project: {selectedProjectName || "n/a"}</span>
            <span>Started: {formatDate(status.startedAtMs)}</span>
            <span>Finished: {formatDate(status.finishedAtMs)}</span>
            <span>Output: {status.outputPath ?? "n/a"}</span>
          </div>
        </aside>
      </div>

      <div className="export-banners">
        {status.error && <div className="export-banner export-banner--error">{status.error}</div>}
        {error && <div className="export-banner export-banner--error">{error}</div>}
        {info && <div className="export-banner export-banner--info">{info}</div>}
      </div>
    </div>
  );
}
