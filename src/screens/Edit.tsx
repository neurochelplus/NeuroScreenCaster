import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { dirname, isAbsolute, join } from "@tauri-apps/api/path";
import type { EventsFile } from "../types/events";
import type { NormalizedRect, PanKeyframe, Project, ZoomSegment } from "../types/project";
import "./Edit.css";

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

interface CursorSample {
  ts: number;
  x: number;
  y: number;
}

interface TimelineSegmentVisual {
  id: string;
  startPreviewMs: number;
  endPreviewMs: number;
  leftPx: number;
  widthPx: number;
  isAuto: boolean;
}

type SegmentDragMode = "move" | "start" | "end";

interface SegmentDragState {
  segmentId: string;
  mode: SegmentDragMode;
  pointerStartX: number;
  initialStartTs: number;
  initialEndTs: number;
}

const DEFAULT_RECT: NormalizedRect = { x: 0.2, y: 0.2, width: 0.6, height: 0.6 };
const FULL_RECT: NormalizedRect = { x: 0, y: 0, width: 1, height: 1 };
const MIN_RECT_SIZE = 0.05;
const MIN_SEGMENT_MS = 200;
const PLAYHEAD_SYNC_THRESHOLD_MS = 12;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function normalizeRect(rect: NormalizedRect): NormalizedRect {
  const width = clamp(rect.width, MIN_RECT_SIZE, 1);
  const height = clamp(rect.height, MIN_RECT_SIZE, 1);
  const x = clamp(rect.x, 0, 1 - width);
  const y = clamp(rect.y, 0, 1 - height);
  return { x, y, width, height };
}

function getSegmentBaseRect(segment: ZoomSegment): NormalizedRect {
  return normalizeRect(segment.initialRect ?? segment.targetRect ?? DEFAULT_RECT);
}

function getSortedPanTrajectory(segment: ZoomSegment): PanKeyframe[] {
  return [...(segment.panTrajectory ?? [])].sort((a, b) => a.ts - b.ts);
}

function panOffsetAtTime(trajectory: PanKeyframe[], ts: number): { offsetX: number; offsetY: number } {
  if (trajectory.length === 0 || ts <= trajectory[0].ts) {
    return { offsetX: 0, offsetY: 0 };
  }

  const last = trajectory[trajectory.length - 1];
  if (ts >= last.ts) {
    return { offsetX: last.offsetX, offsetY: last.offsetY };
  }

  for (let index = 0; index < trajectory.length - 1; index += 1) {
    const left = trajectory[index];
    const right = trajectory[index + 1];
    if (ts < left.ts || ts > right.ts) {
      continue;
    }

    const span = right.ts - left.ts;
    if (span <= 0) {
      return { offsetX: right.offsetX, offsetY: right.offsetY };
    }

    const t = (ts - left.ts) / span;
    return {
      offsetX: left.offsetX + (right.offsetX - left.offsetX) * t,
      offsetY: left.offsetY + (right.offsetY - left.offsetY) * t,
    };
  }

  return { offsetX: last.offsetX, offsetY: last.offsetY };
}

function getSegmentRectAtTimelineTs(segment: ZoomSegment, timelineTs: number): NormalizedRect {
  const base = getSegmentBaseRect(segment);
  const { offsetX, offsetY } = panOffsetAtTime(getSortedPanTrajectory(segment), timelineTs);

  return normalizeRect({
    x: base.x + offsetX,
    y: base.y + offsetY,
    width: base.width,
    height: base.height,
  });
}

function updateSegmentBaseRect(segment: ZoomSegment, rect: NormalizedRect): ZoomSegment {
  const { targetRect: _legacyTargetRect, ...rest } = segment;
  return {
    ...rest,
    initialRect: normalizeRect(rect),
    panTrajectory: [],
  };
}

function sortSegments(segments: ZoomSegment[]): ZoomSegment[] {
  return [...segments].sort((a, b) => a.startTs - b.startTs);
}

function formatMs(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const min = Math.floor(total / 60)
    .toString()
    .padStart(2, "0");
  const sec = (total % 60).toString().padStart(2, "0");
  return `${min}:${sec}`;
}

function formatDate(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) {
    return "unknown";
  }
  return new Date(ms).toLocaleString();
}

function mapTimeMs(valueMs: number, fromDurationMs: number, toDurationMs: number): number {
  if (!Number.isFinite(valueMs) || fromDurationMs <= 0 || toDurationMs <= 0) {
    return 0;
  }
  return clamp(Math.round((valueMs / fromDurationMs) * toDurationMs), 0, toDurationMs);
}

function extractCursorSamples(eventsFile: EventsFile | null, smoothingFactor: number): CursorSample[] {
  if (!eventsFile || eventsFile.screenWidth <= 0 || eventsFile.screenHeight <= 0) {
    return [];
  }

  const samples: CursorSample[] = [];
  for (const event of eventsFile.events) {
    if (event.type === "move" || event.type === "click" || event.type === "mouseUp" || event.type === "scroll") {
      samples.push({
        ts: event.ts,
        x: clamp(event.x / eventsFile.screenWidth, 0, 1),
        y: clamp(event.y / eventsFile.screenHeight, 0, 1),
      });
    }
  }

  const sorted = samples.sort((a, b) => a.ts - b.ts);
  if (sorted.length <= 1) {
    return sorted;
  }

  // 0.0 = no smoothing, 1.0 = maximum smoothing.
  const factor = clamp(smoothingFactor, 0, 1);
  const alpha = 1 - factor * 0.9;
  let smoothedX = sorted[0].x;
  let smoothedY = sorted[0].y;

  const smoothed = [sorted[0]];
  for (let index = 1; index < sorted.length; index += 1) {
    const sample = sorted[index];
    smoothedX = smoothedX + alpha * (sample.x - smoothedX);
    smoothedY = smoothedY + alpha * (sample.y - smoothedY);
    smoothed.push({
      ts: sample.ts,
      x: smoothedX,
      y: smoothedY,
    });
  }

  return smoothed;
}

function interpolateCursor(samples: CursorSample[], ts: number): { x: number; y: number } {
  if (samples.length === 0) {
    return { x: 0.5, y: 0.5 };
  }
  if (ts <= samples[0].ts) {
    return { x: samples[0].x, y: samples[0].y };
  }
  const last = samples[samples.length - 1];
  if (ts >= last.ts) {
    return { x: last.x, y: last.y };
  }

  let low = 0;
  let high = samples.length - 1;
  while (low <= high) {
    const mid = Math.floor((low + high) / 2);
    if (samples[mid].ts === ts) {
      return { x: samples[mid].x, y: samples[mid].y };
    }
    if (samples[mid].ts < ts) {
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }

  const next = samples[low];
  const prev = samples[Math.max(0, low - 1)];
  const span = next.ts - prev.ts;
  if (span <= 0) {
    return { x: prev.x, y: prev.y };
  }

  const t = (ts - prev.ts) / span;
  return {
    x: prev.x + (next.x - prev.x) * t,
    y: prev.y + (next.y - prev.y) * t,
  };
}

function getZoomStrength(rect: NormalizedRect): number {
  return 1 / Math.max(rect.width, rect.height);
}

function buildRectFromCenterZoom(
  centerX: number,
  centerY: number,
  zoomStrength: number,
  aspectRatio: number
): NormalizedRect {
  const safeAspect = Number.isFinite(aspectRatio) && aspectRatio > 0 ? aspectRatio : 16 / 9;
  const safeZoom = clamp(zoomStrength, 1, 6);

  let width = clamp(1 / safeZoom, MIN_RECT_SIZE, 1);
  let height = width / safeAspect;

  if (height > 1) {
    height = 1;
    width = height * safeAspect;
  }
  if (height < MIN_RECT_SIZE) {
    height = MIN_RECT_SIZE;
    width = height * safeAspect;
  }
  if (width > 1) {
    width = 1;
    height = width / safeAspect;
  }
  if (width < MIN_RECT_SIZE) {
    width = MIN_RECT_SIZE;
    height = width / safeAspect;
  }

  return normalizeRect({
    x: centerX - width / 2,
    y: centerY - height / 2,
    width,
    height,
  });
}

function chooseMarkerStepMs(pxPerMs: number): number {
  const targetSpacingPx = 90;
  const approxStepMs = targetSpacingPx / Math.max(pxPerMs, 0.0001);
  const options = [250, 500, 1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000];
  for (const option of options) {
    if (option >= approxStepMs) {
      return option;
    }
  }
  return 60_000;
}

export default function EditScreen() {
  const [projects, setProjects] = useState<ProjectListItem[]>([]);
  const [project, setProject] = useState<Project | null>(null);
  const [eventsFile, setEventsFile] = useState<EventsFile | null>(null);
  const [loadedProjectPath, setLoadedProjectPath] = useState<string | null>(null);
  const [videoSrc, setVideoSrc] = useState<string | null>(null);
  const [videoDurationMs, setVideoDurationMs] = useState<number | null>(null);
  const [previewStageSize, setPreviewStageSize] = useState({ width: 0, height: 0 });
  const [selectedSegmentId, setSelectedSegmentId] = useState<string | null>(null);
  const [playheadMs, setPlayheadMs] = useState(0);
  const [timelineZoom, setTimelineZoom] = useState(1);
  const [timelineViewportWidthPx, setTimelineViewportWidthPx] = useState(0);
  const [isRefreshingProjects, setIsRefreshingProjects] = useState(false);
  const [isLoadingProject, setIsLoadingProject] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isVideoPlaying, setIsVideoPlaying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [videoError, setVideoError] = useState<string | null>(null);
  const [eventsError, setEventsError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);

  const videoRef = useRef<HTMLVideoElement | null>(null);
  const previewStageRef = useRef<HTMLDivElement | null>(null);
  const timelineViewportRef = useRef<HTMLDivElement | null>(null);
  const rafRef = useRef<number | null>(null);
  const dragStateRef = useRef<SegmentDragState | null>(null);

  const timelineDurationMs = project?.durationMs ?? 0;
  const previewDurationMs = useMemo(() => {
    if (!Number.isFinite(videoDurationMs) || !videoDurationMs || videoDurationMs <= 0) {
      return timelineDurationMs;
    }
    return Math.round(videoDurationMs);
  }, [timelineDurationMs, videoDurationMs]);

  const playheadTimelineMs = useMemo(
    () => mapTimeMs(playheadMs, previewDurationMs, timelineDurationMs),
    [playheadMs, previewDurationMs, timelineDurationMs]
  );

  const previewAspectRatio = useMemo(() => {
    if (!project || project.videoHeight <= 0) {
      return 16 / 9;
    }
    return project.videoWidth / project.videoHeight;
  }, [project?.videoWidth, project?.videoHeight]);

  const previewFrameSize = useMemo(() => {
    const containerWidth = previewStageSize.width;
    const containerHeight = previewStageSize.height;
    if (containerWidth <= 0 || containerHeight <= 0) {
      return { width: 1, height: 1 };
    }

    let width = containerWidth;
    let height = width / previewAspectRatio;

    if (height > containerHeight) {
      height = containerHeight;
      width = height * previewAspectRatio;
    }

    return {
      width: Math.max(1, Math.floor(width)),
      height: Math.max(1, Math.floor(height)),
    };
  }, [previewStageSize.width, previewStageSize.height, previewAspectRatio]);

  const hasPreviewFrame = previewFrameSize.width > 1 && previewFrameSize.height > 1;

  const timelineContentWidthPx = useMemo(() => {
    if (previewDurationMs <= 0) {
      return Math.max(900, timelineViewportWidthPx || 900);
    }
    const scaled = Math.round((previewDurationMs / 1000) * 180 * timelineZoom);
    return Math.max(timelineViewportWidthPx || 0, scaled, 900);
  }, [previewDurationMs, timelineViewportWidthPx, timelineZoom]);

  const pxPerPreviewMs = timelineContentWidthPx / Math.max(previewDurationMs, 1);
  const playheadLeftPx = clamp(playheadMs * pxPerPreviewMs, 0, timelineContentWidthPx);

  const timelineSegments = useMemo(
    () => sortSegments(project?.timeline.zoomSegments ?? []),
    [project?.timeline.zoomSegments]
  );

  const selectedSegment = useMemo(() => {
    if (!project || !selectedSegmentId) {
      return null;
    }
    return project.timeline.zoomSegments.find((segment) => segment.id === selectedSegmentId) ?? null;
  }, [project, selectedSegmentId]);

  const activeSegment = useMemo(() => {
    if (!project) {
      return null;
    }
    return (
      project.timeline.zoomSegments.find(
        (segment) => playheadTimelineMs >= segment.startTs && playheadTimelineMs <= segment.endTs
      ) ?? null
    );
  }, [project, playheadTimelineMs]);

  const selectedSegmentCenter = useMemo(() => {
    if (!selectedSegment) {
      return { x: 0.5, y: 0.5 };
    }
    const rect = getSegmentBaseRect(selectedSegment);
    return {
      x: rect.x + rect.width / 2,
      y: rect.y + rect.height / 2,
    };
  }, [selectedSegment]);

  const selectedSegmentZoom = useMemo(
    () => (selectedSegment ? getZoomStrength(getSegmentBaseRect(selectedSegment)) : 1),
    [selectedSegment]
  );

  const selectedSegmentAspect = useMemo(() => {
    if (selectedSegment) {
      const rect = getSegmentBaseRect(selectedSegment);
      return rect.width / Math.max(rect.height, MIN_RECT_SIZE);
    }
    if (project) {
      return project.videoWidth / Math.max(project.videoHeight, 1);
    }
    return 16 / 9;
  }, [selectedSegment, project]);

  const previewRect = activeSegment
    ? getSegmentRectAtTimelineTs(activeSegment, playheadTimelineMs)
    : FULL_RECT;
  const previewScale = 1 / Math.max(previewRect.width, previewRect.height);
  const centerX = previewRect.x + previewRect.width / 2;
  const centerY = previewRect.y + previewRect.height / 2;
  const previewTranslateX = (0.5 - centerX * previewScale) * 100;
  const previewTranslateY = (0.5 - centerY * previewScale) * 100;

  const cursorSamples = useMemo(
    () => extractCursorSamples(eventsFile, project?.settings.cursor.smoothingFactor ?? 0.8),
    [eventsFile, project?.settings.cursor.smoothingFactor]
  );
  const cursorPosition = useMemo(
    () => interpolateCursor(cursorSamples, playheadTimelineMs),
    [cursorSamples, playheadTimelineMs]
  );

  const segmentVisuals = useMemo<TimelineSegmentVisual[]>(() => {
    if (!project || previewDurationMs <= 0 || timelineDurationMs <= 0) {
      return [];
    }

    return timelineSegments.map((segment) => {
      const startPreviewMs = mapTimeMs(segment.startTs, timelineDurationMs, previewDurationMs);
      const endPreviewMs = mapTimeMs(segment.endTs, timelineDurationMs, previewDurationMs);
      const leftPx = clamp(startPreviewMs * pxPerPreviewMs, 0, timelineContentWidthPx);
      const widthPx = Math.max((endPreviewMs - startPreviewMs) * pxPerPreviewMs, 20);

      return {
        id: segment.id,
        startPreviewMs,
        endPreviewMs,
        leftPx,
        widthPx,
        isAuto: segment.isAuto,
      };
    });
  }, [project, previewDurationMs, timelineDurationMs, timelineSegments, pxPerPreviewMs, timelineContentWidthPx]);

  const markerStepMs = useMemo(() => chooseMarkerStepMs(pxPerPreviewMs), [pxPerPreviewMs]);
  const timelineMarkers = useMemo(() => {
    if (previewDurationMs <= 0 || markerStepMs <= 0) {
      return [];
    }

    const markers: Array<{ ms: number; leftPx: number }> = [];
    for (let ms = 0; ms <= previewDurationMs; ms += markerStepMs) {
      markers.push({
        ms,
        leftPx: clamp(ms * pxPerPreviewMs, 0, timelineContentWidthPx),
      });
    }
    if (markers[markers.length - 1]?.ms !== previewDurationMs) {
      markers.push({
        ms: previewDurationMs,
        leftPx: timelineContentWidthPx,
      });
    }
    return markers;
  }, [previewDurationMs, markerStepMs, pxPerPreviewMs, timelineContentWidthPx]);

  const updateProject = (updater: (current: Project) => Project) => {
    setProject((current) => (current ? updater(current) : current));
  };

  const updateSegment = (segmentId: string, updater: (segment: ZoomSegment) => ZoomSegment) => {
    updateProject((current) => ({
      ...current,
      timeline: {
        ...current.timeline,
        zoomSegments: sortSegments(
          current.timeline.zoomSegments.map((segment) =>
            segment.id === segmentId ? updater(segment) : segment
          )
        ),
      },
    }));
  };

  const loadProjectByPath = async (projectPath: string, showLoadedInfo = true) => {
    setError(null);
    setVideoError(null);
    setEventsError(null);
    if (showLoadedInfo) {
      setInfo(null);
    }
    setIsLoadingProject(true);

    try {
      const loaded = await invoke<Project>("get_project", { projectPath });
      let loadedEvents: EventsFile | null = null;

      try {
        loadedEvents = await invoke<EventsFile>("get_events", { projectPath });
      } catch (eventsErr) {
        setEventsError(`Failed to load events: ${String(eventsErr)}`);
      }

      const sorted = sortSegments(loaded.timeline.zoomSegments);
      setProject({
        ...loaded,
        timeline: {
          ...loaded.timeline,
          zoomSegments: sorted,
        },
      });
      setEventsFile(loadedEvents);
      setSelectedSegmentId(sorted[0]?.id ?? null);
      setPlayheadMs(0);
      setTimelineZoom(1);
      setVideoDurationMs(null);
      setIsVideoPlaying(false);
      setLoadedProjectPath(projectPath);
      if (showLoadedInfo) {
        setInfo(`Loaded project: ${loaded.name}`);
      }
    } catch (err) {
      setError(String(err));
      setProject(null);
      setEventsFile(null);
      setSelectedSegmentId(null);
    } finally {
      setIsLoadingProject(false);
    }
  };

  const refreshProjects = async (autoLoadLatest: boolean) => {
    setError(null);
    setIsRefreshingProjects(true);
    try {
      const listed = await invoke<ProjectListItem[]>("list_projects");
      setProjects(listed);

      if (listed.length === 0) {
        if (autoLoadLatest) {
          setProject(null);
          setEventsFile(null);
          setLoadedProjectPath(null);
          setSelectedSegmentId(null);
          setVideoDurationMs(null);
          setInfo("No projects found. Create a recording first.");
        }
        return;
      }

      if (autoLoadLatest) {
        await loadProjectByPath(listed[0].projectPath, false);
        setInfo(`Loaded latest project: ${listed[0].name}`);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsRefreshingProjects(false);
    }
  };

  useEffect(() => {
    void refreshProjects(true);
  }, []);

  useEffect(() => {
    let isCancelled = false;

    const resolveVideoSrc = async () => {
      if (!project || !loadedProjectPath || !project.videoPath.trim()) {
        setVideoSrc(null);
        setVideoDurationMs(null);
        setVideoError(null);
        return;
      }

      try {
        const sourcePath = project.videoPath.trim();
        const absoluteSourcePath = (await isAbsolute(sourcePath))
          ? sourcePath
          : await join(await dirname(loadedProjectPath), sourcePath);

        if (!isCancelled) {
          setVideoSrc(convertFileSrc(absoluteSourcePath));
          setVideoDurationMs(null);
          setVideoError(null);
        }
      } catch (err) {
        if (!isCancelled) {
          setVideoSrc(null);
          setVideoDurationMs(null);
          setVideoError(`Failed to resolve video file path: ${String(err)}`);
        }
      }
    };

    void resolveVideoSrc();

    return () => {
      isCancelled = true;
    };
  }, [project?.videoPath, loadedProjectPath]);

  useEffect(() => {
    if (!videoRef.current || previewDurationMs <= 0) {
      return;
    }

    const video = videoRef.current;
    const targetTimeSec = clamp(playheadMs, 0, previewDurationMs) / 1000;
    if (Math.abs(video.currentTime - targetTimeSec) > 0.05) {
      video.currentTime = targetTimeSec;
    }
  }, [playheadMs, previewDurationMs]);

  useEffect(() => {
    if (previewDurationMs <= 0) {
      return;
    }
    setPlayheadMs((current) => clamp(current, 0, previewDurationMs));
  }, [previewDurationMs]);

  useEffect(() => {
    if (!isVideoPlaying || previewDurationMs <= 0) {
      return;
    }

    const updateFromVideo = () => {
      const video = videoRef.current;
      if (!video || video.paused || video.ended) {
        setIsVideoPlaying(false);
        return;
      }

      const nextMs = clamp(Math.round(video.currentTime * 1000), 0, previewDurationMs);
      setPlayheadMs((current) =>
        Math.abs(current - nextMs) >= PLAYHEAD_SYNC_THRESHOLD_MS ? nextMs : current
      );

      rafRef.current = requestAnimationFrame(updateFromVideo);
    };

    rafRef.current = requestAnimationFrame(updateFromVideo);

    return () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, [isVideoPlaying, previewDurationMs]);

  useEffect(() => {
    const viewport = timelineViewportRef.current;
    if (!viewport) {
      return;
    }

    const updateWidth = () => setTimelineViewportWidthPx(viewport.clientWidth);
    updateWidth();
    const observer = new ResizeObserver(updateWidth);
    observer.observe(viewport);

    return () => observer.disconnect();
  }, [project]);

  useEffect(() => {
    const stage = previewStageRef.current;
    if (!stage) {
      return;
    }

    const updateSize = () => {
      setPreviewStageSize({
        width: stage.clientWidth,
        height: stage.clientHeight,
      });
    };

    updateSize();
    const observer = new ResizeObserver(updateSize);
    observer.observe(stage);

    return () => observer.disconnect();
  }, [project]);

  useEffect(() => {
    const onPointerMove = (event: PointerEvent) => {
      const drag = dragStateRef.current;
      if (!drag || timelineDurationMs <= 0 || previewDurationMs <= 0) {
        return;
      }

      const deltaPx = event.clientX - drag.pointerStartX;
      const deltaPreviewMs = deltaPx / Math.max(pxPerPreviewMs, 0.0001);
      const deltaTimelineMs = Math.round((deltaPreviewMs * timelineDurationMs) / previewDurationMs);

      setProject((current) => {
        if (!current) {
          return current;
        }

        const nextSegments = current.timeline.zoomSegments.map((segment) => {
          if (segment.id !== drag.segmentId) {
            return segment;
          }

          if (drag.mode === "move") {
            const length = drag.initialEndTs - drag.initialStartTs;
            const startTs = clamp(drag.initialStartTs + deltaTimelineMs, 0, Math.max(0, timelineDurationMs - length));
            return {
              ...segment,
              startTs,
              endTs: startTs + length,
              isAuto: false,
            };
          }

          if (drag.mode === "start") {
            return {
              ...segment,
              startTs: clamp(
                drag.initialStartTs + deltaTimelineMs,
                0,
                Math.max(0, drag.initialEndTs - MIN_SEGMENT_MS)
              ),
              isAuto: false,
            };
          }

          return {
            ...segment,
            endTs: clamp(
              drag.initialEndTs + deltaTimelineMs,
              drag.initialStartTs + MIN_SEGMENT_MS,
              timelineDurationMs
            ),
            isAuto: false,
          };
        });

        return {
          ...current,
          timeline: {
            ...current.timeline,
            zoomSegments: sortSegments(nextSegments),
          },
        };
      });
    };

    const onPointerUp = () => {
      dragStateRef.current = null;
    };

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
    };
  }, [previewDurationMs, timelineDurationMs, pxPerPreviewMs]);

  const handleSaveProject = async () => {
    if (!project) {
      return;
    }
    setError(null);
    setInfo(null);
    setIsSaving(true);
    try {
      const savedPath = await invoke<string>("save_project", {
        project,
        projectPath: loadedProjectPath,
      });
      setLoadedProjectPath(savedPath);
      await refreshProjects(false);
      setInfo(`Project saved: ${savedPath}`);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsSaving(false);
    }
  };

  const handleAddSegment = () => {
    if (!project) {
      return;
    }

    const startTs = clamp(playheadTimelineMs, 0, Math.max(0, timelineDurationMs - MIN_SEGMENT_MS));
    const endTs = clamp(startTs + 1600, startTs + MIN_SEGMENT_MS, timelineDurationMs);
    const nextId = `manual-${Date.now()}`;
    const rect = activeSegment
      ? getSegmentRectAtTimelineTs(activeSegment, playheadTimelineMs)
      : DEFAULT_RECT;

    const newSegment: ZoomSegment = {
      id: nextId,
      startTs,
      endTs,
      initialRect: normalizeRect(rect),
      panTrajectory: [],
      easing: "ease-in-out",
      isAuto: false,
    };

    updateProject((current) => ({
      ...current,
      timeline: {
        ...current.timeline,
        zoomSegments: sortSegments([...current.timeline.zoomSegments, newSegment]),
      },
    }));
    setSelectedSegmentId(nextId);
  };

  const handleDeleteSelectedSegment = () => {
    if (!project || !selectedSegment) {
      return;
    }

    const nextSegments = project.timeline.zoomSegments.filter((segment) => segment.id !== selectedSegment.id);
    updateProject((current) => ({
      ...current,
      timeline: {
        ...current.timeline,
        zoomSegments: nextSegments,
      },
    }));
    setSelectedSegmentId(nextSegments[0]?.id ?? null);
  };

  const applySelectedSegmentRect = (centerX: number, centerY: number, zoomStrength: number) => {
    if (!selectedSegment) {
      return;
    }

    updateSegment(selectedSegment.id, (segment) => ({
      ...updateSegmentBaseRect(
        segment,
        buildRectFromCenterZoom(centerX, centerY, zoomStrength, selectedSegmentAspect)
      ),
      isAuto: false,
    }));
  };

  const seekToPreviewMs = (nextMs: number) => {
    const clampedMs = clamp(nextMs, 0, previewDurationMs);
    setPlayheadMs(clampedMs);
    if (videoRef.current) {
      videoRef.current.currentTime = clampedMs / 1000;
    }
  };

  const seekBy = (deltaMs: number) => {
    seekToPreviewMs(playheadMs + deltaMs);
  };

  const togglePlayback = async () => {
    const video = videoRef.current;
    if (!video) {
      return;
    }

    if (video.paused || video.ended) {
      try {
        await video.play();
        setIsVideoPlaying(true);
      } catch (err) {
        setVideoError(`Failed to play video: ${String(err)}`);
      }
      return;
    }

    video.pause();
    setIsVideoPlaying(false);
  };

  const startDragSegment = (
    event: React.PointerEvent<HTMLDivElement>,
    segment: ZoomSegment,
    mode: SegmentDragMode
  ) => {
    event.preventDefault();
    event.stopPropagation();
    setSelectedSegmentId(segment.id);
    dragStateRef.current = {
      segmentId: segment.id,
      mode,
      pointerStartX: event.clientX,
      initialStartTs: segment.startTs,
      initialEndTs: segment.endTs,
    };
  };

  const onTimelinePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (previewDurationMs <= 0) {
      return;
    }

    const target = event.target as HTMLElement;
    if (target.closest(".timeline-segment-block")) {
      return;
    }

    const rect = event.currentTarget.getBoundingClientRect();
    const localX = clamp(event.clientX - rect.left, 0, rect.width);
    const nextMs = Math.round((localX / Math.max(rect.width, 1)) * previewDurationMs);
    seekToPreviewMs(nextMs);
  };

  return (
    <div className="edit-shell">
      <section className="editor-toolbar">
        <div className="project-picker">
          <label htmlFor="project-select">Project</label>
          <select
            id="project-select"
            value={loadedProjectPath ?? ""}
            onChange={(event) => void loadProjectByPath(event.target.value)}
            disabled={isLoadingProject || projects.length === 0}
          >
            {projects.length === 0 ? (
              <option value="">No projects</option>
            ) : (
              projects.map((item) => (
                <option key={item.projectPath} value={item.projectPath}>
                  {item.name} | {formatDate(item.createdAt)} | {formatMs(item.durationMs)}
                </option>
              ))
            )}
          </select>
        </div>

        <div className="toolbar-actions">
          <button className="btn-ghost" onClick={() => void refreshProjects(false)} disabled={isRefreshingProjects}>
            {isRefreshingProjects ? "Refreshing..." : "Refresh"}
          </button>
          <button className="btn-primary" onClick={handleSaveProject} disabled={!project || isSaving}>
            {isSaving ? "Saving..." : "Save"}
          </button>
        </div>
      </section>

      {project && (
        <div className="project-meta">
          <span>{project.name}</span>
          <span>ID: {project.id}</span>
          <span>Timeline: {formatMs(timelineDurationMs)}</span>
          <span>Video: {formatMs(previewDurationMs)}</span>
          <span>
            Resolution: {project.videoWidth}x{project.videoHeight}
          </span>
        </div>
      )}

      {error && <div className="edit-banner edit-banner--error">{error}</div>}
      {videoError && <div className="edit-banner edit-banner--error">{videoError}</div>}
      {eventsError && <div className="edit-banner edit-banner--error">{eventsError}</div>}
      {info && <div className="edit-banner edit-banner--info">{info}</div>}

      {!project && (
        <section className="editor-empty">
          <h2>Project is not loaded</h2>
          <p>Create recording and choose it from dropdown above.</p>
        </section>
      )}

      {project && (
        <>
          <section className="editor-main">
            <aside className="editor-sidebar">
              <div className="sidebar-header">
                <h2>Selected Zoom</h2>
                <button className="btn-ghost" onClick={handleDeleteSelectedSegment} disabled={!selectedSegment}>
                  Delete
                </button>
              </div>

              {!selectedSegment ? (
                <p className="sidebar-placeholder">Select a zoom segment on timeline.</p>
              ) : (
                <div className="sidebar-controls">
                  <div className="segment-badge">
                    <span>{selectedSegment.id}</span>
                    <span>{selectedSegment.isAuto ? "auto" : "manual"}</span>
                  </div>

                  <label>
                    <span>Zoom Strength</span>
                    <input
                      type="range"
                      min={1}
                      max={6}
                      step={0.01}
                      value={selectedSegmentZoom}
                      onChange={(event) =>
                        applySelectedSegmentRect(
                          selectedSegmentCenter.x,
                          selectedSegmentCenter.y,
                          Number(event.target.value)
                        )
                      }
                    />
                  </label>

                  <label>
                    <span>Position X</span>
                    <input
                      type="range"
                      min={0}
                      max={1}
                      step={0.001}
                      value={selectedSegmentCenter.x}
                      onChange={(event) =>
                        applySelectedSegmentRect(
                          Number(event.target.value),
                          selectedSegmentCenter.y,
                          selectedSegmentZoom
                        )
                      }
                    />
                  </label>

                  <label>
                    <span>Position Y</span>
                    <input
                      type="range"
                      min={0}
                      max={1}
                      step={0.001}
                      value={selectedSegmentCenter.y}
                      onChange={(event) =>
                        applySelectedSegmentRect(
                          selectedSegmentCenter.x,
                          Number(event.target.value),
                          selectedSegmentZoom
                        )
                      }
                    />
                  </label>
                </div>
              )}

              <div className="sidebar-controls">
                <label>
                  <span>Cursor Size</span>
                  <input
                    type="range"
                    min={0.4}
                    max={4}
                    step={0.01}
                    value={project.settings.cursor.size}
                    onChange={(event) =>
                      updateProject((current) => ({
                        ...current,
                        settings: {
                          ...current.settings,
                          cursor: {
                            ...current.settings.cursor,
                            size: Number(event.target.value),
                          },
                        },
                      }))
                    }
                  />
                </label>
                <label>
                  <span>Cursor Smoothing</span>
                  <input
                    type="range"
                    min={0}
                    max={1}
                    step={0.01}
                    value={project.settings.cursor.smoothingFactor}
                    onChange={(event) =>
                      updateProject((current) => ({
                        ...current,
                        settings: {
                          ...current.settings,
                          cursor: {
                            ...current.settings.cursor,
                            smoothingFactor: Number(event.target.value),
                          },
                        },
                      }))
                    }
                  />
                </label>
              </div>
            </aside>

            <div className="editor-preview-column">
              <div className="preview-stage-viewport" ref={previewStageRef}>
                <div
                  className="preview-stage"
                  style={
                    hasPreviewFrame
                      ? {
                          width: `${previewFrameSize.width}px`,
                          height: `${previewFrameSize.height}px`,
                        }
                      : undefined
                  }
                >
                  <div
                    className="preview-canvas"
                    style={{
                      transform: `translate(${previewTranslateX}%, ${previewTranslateY}%) scale(${previewScale})`,
                    }}
                  >
                    {videoSrc ? (
                      <video
                        ref={videoRef}
                        className="preview-video"
                        src={videoSrc}
                        preload="metadata"
                        onPlay={() => setIsVideoPlaying(true)}
                        onPause={() => setIsVideoPlaying(false)}
                        onEnded={() => setIsVideoPlaying(false)}
                        onLoadedMetadata={(event) => {
                          const durationSec = event.currentTarget.duration;
                          if (Number.isFinite(durationSec) && durationSec > 0) {
                            setVideoDurationMs(Math.round(durationSec * 1000));
                          }
                        }}
                        onDurationChange={(event) => {
                          const durationSec = event.currentTarget.duration;
                          if (Number.isFinite(durationSec) && durationSec > 0) {
                            setVideoDurationMs(Math.round(durationSec * 1000));
                          }
                        }}
                        onTimeUpdate={(event) => {
                          const nextMs = clamp(
                            Math.round(event.currentTarget.currentTime * 1000),
                            0,
                            previewDurationMs
                          );
                          setPlayheadMs((current) =>
                            Math.abs(current - nextMs) >= PLAYHEAD_SYNC_THRESHOLD_MS
                              ? nextMs
                              : current
                          );
                        }}
                        onSeeking={(event) => {
                          const nextMs = clamp(
                            Math.round(event.currentTarget.currentTime * 1000),
                            0,
                            previewDurationMs
                          );
                          setPlayheadMs(nextMs);
                        }}
                        onError={() =>
                          setVideoError("Failed to load project video. Check file availability and asset scope.")
                        }
                      />
                    ) : (
                      <div className="preview-video-placeholder">Video source is unavailable for this project.</div>
                    )}

                    <div className="preview-overlay-grid" />
                    <div
                      className="preview-cursor"
                      style={{
                        left: `${cursorPosition.x * 100}%`,
                        top: `${cursorPosition.y * 100}%`,
                        width: `${project.settings.cursor.size * 16}px`,
                        height: `${project.settings.cursor.size * 16}px`,
                        background: project.settings.cursor.color,
                      }}
                    />
                  </div>
                </div>
              </div>

              <div className="preview-controls">
                <button className="btn-ghost" onClick={() => seekBy(-5000)}>
                  -5s
                </button>
                <button className="btn-primary" onClick={() => void togglePlayback()}>
                  {isVideoPlaying ? "Pause" : "Play"}
                </button>
                <button className="btn-ghost" onClick={() => seekBy(5000)}>
                  +5s
                </button>
                <span className="preview-time">
                  {formatMs(playheadMs)} / {formatMs(previewDurationMs)}
                </span>
              </div>
            </div>
          </section>

          <section className="timeline-shell">
            <div className="timeline-toolbar">
              <div className="timeline-toolbar-group">
                <button className="btn-primary" onClick={handleAddSegment}>
                  Add Zoom
                </button>
              </div>
              <div className="timeline-toolbar-group timeline-toolbar-group--grow">
                <span>Timeline Zoom</span>
                <input
                  type="range"
                  min={1}
                  max={6}
                  step={0.1}
                  value={timelineZoom}
                  onChange={(event) => setTimelineZoom(Number(event.target.value))}
                />
              </div>
            </div>

            <div className="timeline-viewport" ref={timelineViewportRef}>
              <div className="timeline-content" style={{ width: `${timelineContentWidthPx}px` }}>
                <div className="timeline-ruler">
                  {timelineMarkers.map((marker) => (
                    <div
                      key={marker.ms}
                      className="timeline-marker"
                      style={{ left: `${marker.leftPx}px` }}
                    >
                      <span>{formatMs(marker.ms)}</span>
                    </div>
                  ))}
                </div>

                <div className="timeline-rows" onPointerDown={onTimelinePointerDown}>
                  <div className="timeline-row">
                    <div className="timeline-row-label">Video</div>
                    <div className="timeline-row-lane">
                      <div className="timeline-video-track" />
                    </div>
                  </div>

                  <div className="timeline-row">
                    <div className="timeline-row-label">Zoom</div>
                    <div className="timeline-row-lane">
                      {segmentVisuals.map((visual) => {
                        const segment = timelineSegments.find((item) => item.id === visual.id);
                        if (!segment) {
                          return null;
                        }
                        const isSelected = selectedSegmentId === visual.id;
                        const zoom = getZoomStrength(getSegmentBaseRect(segment));

                        return (
                          <div
                            key={visual.id}
                            className={`timeline-segment-block ${
                              isSelected ? "timeline-segment-block--selected" : ""
                            }`}
                            style={{
                              left: `${visual.leftPx}px`,
                              width: `${visual.widthPx}px`,
                            }}
                            onPointerDown={(event) => startDragSegment(event, segment, "move")}
                            onClick={(event) => {
                              event.stopPropagation();
                              setSelectedSegmentId(visual.id);
                            }}
                          >
                            <div
                              className="timeline-segment-handle timeline-segment-handle--start"
                              onPointerDown={(event) => startDragSegment(event, segment, "start")}
                            />
                            <span>{visual.isAuto ? "A" : "M"} Zoom {zoom.toFixed(1)}x</span>
                            <div
                              className="timeline-segment-handle timeline-segment-handle--end"
                              onPointerDown={(event) => startDragSegment(event, segment, "end")}
                            />
                          </div>
                        );
                      })}
                    </div>
                  </div>

                  <div className="timeline-playhead" style={{ left: `${playheadLeftPx}px` }} />
                </div>
              </div>
            </div>
          </section>
        </>
      )}
    </div>
  );
}
