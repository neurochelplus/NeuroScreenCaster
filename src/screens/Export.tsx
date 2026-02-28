import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { join } from "@tauri-apps/api/path";
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

function sanitizeFileName(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) {
    return "";
  }
  return trimmed.replace(/[<>:"/\\|?*\x00-\x1f]/g, "_");
}

function ensureMp4Extension(fileName: string): string {
  if (fileName.toLowerCase().endsWith(".mp4")) {
    return fileName;
  }
  return `${fileName}.mp4`;
}

export default function ExportScreen() {
  const [projects, setProjects] = useState<ProjectListItem[]>([]);
  const [selectedProjectPath, setSelectedProjectPath] = useState<string>("");
  const [selectedProjectName, setSelectedProjectName] = useState<string>("");
  const [width, setWidth] = useState(1920);
  const [height, setHeight] = useState(1080);
  const [fps, setFps] = useState(30);
  const [codec, setCodec] = useState<(typeof CODEC_OPTIONS)[number]>("h264");
  const [outputDirectory, setOutputDirectory] = useState("");
  const [outputFileName, setOutputFileName] = useState("");
  const [status, setStatus] = useState<ExportStatus>(DEFAULT_STATUS);
  const [isRefreshingProjects, setIsRefreshingProjects] = useState(false);
  const [isLoadingProject, setIsLoadingProject] = useState(false);
  const [isStartingExport, setIsStartingExport] = useState(false);
  const [isCancellingExport, setIsCancellingExport] = useState(false);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);

  const progressPercentPrecise = useMemo(
    () => Math.min(Math.max(status.progress, 0), 1) * 100,
    [status.progress]
  );
  const elapsedMs = useMemo(() => {
    if (!status.startedAtMs) {
      return null;
    }
    return Math.max(0, nowMs - status.startedAtMs);
  }, [nowMs, status.startedAtMs]);
  const etaMs = useMemo(() => {
    if (!status.isRunning || !elapsedMs) {
      return null;
    }
    const progress = Math.min(Math.max(status.progress, 0), 1);
    if (progress < 0.01 || progress >= 1) {
      return null;
    }
    return Math.round((elapsedMs * (1 - progress)) / progress);
  }, [elapsedMs, status.isRunning, status.progress]);

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
        setOutputDirectory(latest.folderPath);
        setOutputFileName(ensureMp4Extension(sanitizeFileName(latest.name) || "export"));
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
      setOutputFileName((current) => {
        const sanitized = sanitizeFileName(loaded.name) || sanitizeFileName(selectedProjectName) || "export";
        const normalized = ensureMp4Extension(sanitized);
        if (!current.trim()) {
          return normalized;
        }
        return current;
      });
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

  useEffect(() => {
    if (!status.isRunning) {
      return;
    }
    const timer = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [status.isRunning]);

  const handleStartExport = async () => {
    if (!selectedProjectPath) {
      setError("Select project for export.");
      return;
    }
    const sanitizedName = sanitizeFileName(outputFileName);
    if (!outputDirectory.trim()) {
      setError("Select output folder.");
      return;
    }
    if (!sanitizedName) {
      setError("Enter export file name.");
      return;
    }

    setError(null);
    setInfo(null);
    setIsStartingExport(true);
    try {
      const fullOutputPath = await join(outputDirectory, ensureMp4Extension(sanitizedName));
      await invoke("start_export", {
        projectPath: selectedProjectPath,
        width,
        height,
        fps,
        codec,
        outputPath: fullOutputPath,
      });
      setInfo("Export started.");
      await fetchStatus();
    } catch (err) {
      setError(String(err));
    } finally {
      setIsStartingExport(false);
    }
  };

  const handleProjectSelection = (projectPath: string) => {
    setSelectedProjectPath(projectPath);
    const found = projects.find((item) => item.projectPath === projectPath);
    if (found && !outputDirectory.trim()) {
      setOutputDirectory(found.folderPath);
    }
  };

  const handlePickOutputDirectory = async () => {
    setError(null);
    try {
      const selected = await invoke<string | null>("pick_export_folder", {
        initialDir: outputDirectory || null,
      });
      if (selected) {
        setOutputDirectory(selected);
      }
    } catch (err) {
      setError(String(err));
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

  const handleCancelExport = async () => {
    setError(null);
    setInfo(null);
    setIsCancellingExport(true);
    try {
      await invoke("cancel_export");
      setInfo("Cancel requested.");
      await fetchStatus();
    } catch (err) {
      setError(String(err));
    } finally {
      setIsCancellingExport(false);
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
                onChange={(event) => handleProjectSelection(event.target.value)}
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

            <div className="export-output-section">
              <label className="export-field">
                <span>Output Folder</span>
                <div className="export-folder-row">
                  <input
                    type="text"
                    value={outputDirectory}
                    onChange={(event) => setOutputDirectory(event.target.value)}
                    placeholder="D:\\Videos\\Exports"
                  />
                  <button className="btn-ghost" type="button" onClick={() => void handlePickOutputDirectory()}>
                    Browse
                  </button>
                </div>
              </label>

              <label className="export-field">
                <span>File Name</span>
                <input
                  type="text"
                  value={outputFileName}
                  onChange={(event) => setOutputFileName(event.target.value)}
                  placeholder="my-recording.mp4"
                />
              </label>
            </div>

            <div className="export-actions">
              <button
                className="btn-primary"
                onClick={() => void handleStartExport()}
                disabled={
                  !selectedProjectPath || isStartingExport || isCancellingExport || status.isRunning
                }
              >
                {status.isRunning ? "Exporting..." : isStartingExport ? "Starting..." : "Start Export"}
              </button>
              <button
                className="btn-ghost"
                onClick={() => void handleCancelExport()}
                disabled={!status.isRunning || isCancellingExport}
              >
                {isCancellingExport ? "Cancelling..." : "Cancel Export"}
              </button>
              <button
                className="btn-ghost"
                onClick={() => void handleResetStatus()}
                disabled={status.isRunning || isCancellingExport}
              >
                Reset Status
              </button>
            </div>
          </section>
        </div>

        <aside className="export-card export-card--status">
          <h2>Status</h2>
          <div className="export-status-row">
            <span className="export-status-label">{status.message || "Idle"}</span>
            <span className="export-status-value">{progressPercentPrecise.toFixed(1)}%</span>
          </div>
          <div className="export-progress">
            <div
              className="export-progress-fill"
              style={{
                width: `${Math.max(
                  progressPercentPrecise,
                  status.isRunning && progressPercentPrecise > 0 ? 0.3 : 0
                )}%`,
              }}
            />
          </div>
          <div className="export-meta">
            <span>Project: {selectedProjectName || "n/a"}</span>
            <span>Started: {formatDate(status.startedAtMs)}</span>
            <span>Finished: {formatDate(status.finishedAtMs)}</span>
            <span>Elapsed: {elapsedMs == null ? "n/a" : formatMs(elapsedMs)}</span>
            <span>ETA: {etaMs == null ? "n/a" : formatMs(etaMs)}</span>
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
