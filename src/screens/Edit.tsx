import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { dirname, isAbsolute, join } from "@tauri-apps/api/path";
import type { EventsFile } from "../types/events";
import type {
  CameraSpring,
  NormalizedRect,
  PanKeyframe,
  Project,
  TargetPoint,
  ZoomMode,
  ZoomSegment,
  ZoomTrigger,
} from "../types/project";
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

interface RawTimelineSegmentVisual {
  id: string;
  startPreviewMs: number;
  endPreviewMs: number;
  leftPx: number;
  naturalWidthPx: number;
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

interface RuntimeSegment {
  id: string;
  startTs: number;
  endTs: number;
  isAuto: boolean;
  mode: ZoomMode;
  trigger: ZoomTrigger;
  baseRect: NormalizedRect;
  targetPoints: TargetPoint[];
  spring: CameraSpring;
}

interface SpringCameraSample {
  ts: number;
  rect: NormalizedRect;
}

const DEFAULT_RECT: NormalizedRect = { x: 0.2, y: 0.2, width: 0.6, height: 0.6 };
const FULL_RECT: NormalizedRect = { x: 0, y: 0, width: 1, height: 1 };
const DEFAULT_SPRING: CameraSpring = { mass: 1, stiffness: 170, damping: 26 };
const DEFAULT_SEGMENT_MODE: ZoomMode = "fixed";
const DEFAULT_SEGMENT_TRIGGER: ZoomTrigger = "manual";
const MIN_RECT_SIZE = 0.05;
const MIN_SEGMENT_MS = 200;
const PLAYHEAD_STATE_SYNC_INTERVAL_MS = 120;
const PREVIEW_SPRING_FPS = 60;
const FOLLOW_SAMPLE_STEP_MS = 75;
const FOLLOW_DEAD_ZONE_RATIO = 0.2;
const FOLLOW_HARD_EDGE_RATIO = 0.35;
const FOLLOW_MAX_SPEED_PX_PER_S = 800;
const EFFECTIVE_ZOOM_EPSILON = 0.001;
const CURSOR_SIZE_TO_FRAME_RATIO = 0.03;
const CLICK_PULSE_MIN_SCALE = 0.82;
const CLICK_PULSE_TOTAL_MS = 150;
const CLICK_PULSE_DOWN_MS = 65;
const CURSOR_TIMING_OFFSET_MS = 45;
const VECTOR_CURSOR_WIDTH = 72;
const VECTOR_CURSOR_HEIGHT = 110;
const TIMELINE_MIN_ZOOM_PERCENT = 0;
const TIMELINE_MAX_ZOOM_PERCENT = 100;
const TIMELINE_DEFAULT_ZOOM_PERCENT = 50;
const TIMELINE_MAX_VISIBLE_WINDOW_MS = 10_000;
const TIMELINE_LABEL_WIDTH_PX = 72;
const TIMELINE_LANE_RIGHT_MARGIN_PX = 8;
const TIMELINE_MIN_SEGMENT_WIDTH_PX = 2;
const TIMELINE_MIN_VISIBLE_SEGMENT_WIDTH_PX = 1;
const TIMELINE_VISUAL_ZOOM_EPSILON = 0.0002;
const TIMELINE_VISUAL_MOTION_EPSILON = 0.00005;
const MIN_SEGMENT_GAP_MS = 200;
const TIMELINE_VISUAL_RETURN_TAIL_MS = 200;
const VECTOR_CURSOR_DATA_URI = `data:image/svg+xml;utf8,${encodeURIComponent(
  "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 72 110'><path d='M0 0 L0 90 L22 70 L35 110 L50 102 L38 63 L72 63 Z' fill='#000000' stroke='#ffffff' stroke-width='6' stroke-linejoin='round'/></svg>"
)}`;

function SeekBackIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path d="M6 4.5v11" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      <path d="m14.5 5.2-6.2 4.8 6.2 4.8V5.2Z" fill="currentColor" />
    </svg>
  );
}

function SeekForwardIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path d="M14 4.5v11" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      <path d="m5.5 5.2 6.2 4.8-6.2 4.8V5.2Z" fill="currentColor" />
    </svg>
  );
}

function PlayIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path d="m6.4 4.8 8.5 5.2-8.5 5.2V4.8Z" fill="currentColor" />
    </svg>
  );
}

function PauseIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <rect x="5.2" y="4.7" width="3.7" height="10.6" rx="1.2" fill="currentColor" />
      <rect x="11.1" y="4.7" width="3.7" height="10.6" rx="1.2" fill="currentColor" />
    </svg>
  );
}

function VolumeIcon({ muted }: { muted: boolean }) {
  return (
    <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <path
        d="M3.4 8.1h2.3l3.2-2.8v9.4l-3.2-2.8H3.4V8.1Z"
        fill="currentColor"
      />
      {muted ? (
        <path d="m12.2 8.3 4 4m0-4-4 4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      ) : (
        <>
          <path d="M12.4 8.2c1.1 1.1 1.1 2.5 0 3.6" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <path d="M14.3 6.4c2.1 2.1 2.1 5.2 0 7.3" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
        </>
      )}
    </svg>
  );
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function isExpectedPlaybackAbort(err: unknown): boolean {
  if (err instanceof DOMException && err.name === "AbortError") {
    return true;
  }
  const text = String(err);
  return (
    text.includes("AbortError") ||
    text.includes("interrupted by a call to pause()") ||
    text.includes("The play() request was interrupted")
  );
}

function normalizeRect(rect: NormalizedRect): NormalizedRect {
  const width = clamp(rect.width, MIN_RECT_SIZE, 1);
  const height = clamp(rect.height, MIN_RECT_SIZE, 1);
  const x = clamp(rect.x, 0, 1 - width);
  const y = clamp(rect.y, 0, 1 - height);
  return { x, y, width, height };
}

function normalizeSegmentMode(mode: ZoomMode | undefined): ZoomMode {
  return mode === "follow-cursor" ? "follow-cursor" : DEFAULT_SEGMENT_MODE;
}

function normalizeSegmentTrigger(trigger: ZoomTrigger | undefined, isAuto: boolean): ZoomTrigger {
  if (trigger === "auto-click" || trigger === "auto-scroll" || trigger === "manual") {
    return trigger;
  }
  return isAuto ? "auto-click" : DEFAULT_SEGMENT_TRIGGER;
}

function normalizeZoomSegment(segment: ZoomSegment): ZoomSegment {
  const normalizedMode = normalizeSegmentMode(segment.mode);
  const mode: ZoomMode = segment.isAuto ? "follow-cursor" : normalizedMode;
  return {
    ...segment,
    mode,
    trigger: normalizeSegmentTrigger(segment.trigger, segment.isAuto),
  };
}

function getSegmentBaseRect(segment: ZoomSegment): NormalizedRect {
  return normalizeRect(segment.initialRect ?? segment.targetRect ?? DEFAULT_RECT);
}

function normalizeSpring(spring: CameraSpring | undefined): CameraSpring {
  if (!spring) {
    return DEFAULT_SPRING;
  }
  return {
    mass: clamp(Number.isFinite(spring.mass) ? spring.mass : DEFAULT_SPRING.mass, 0.001, 50),
    stiffness: clamp(
      Number.isFinite(spring.stiffness) ? spring.stiffness : DEFAULT_SPRING.stiffness,
      0.001,
      5_000
    ),
    damping: clamp(
      Number.isFinite(spring.damping) ? spring.damping : DEFAULT_SPRING.damping,
      0,
      500
    ),
  };
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

function getLegacyPanRectAtTimelineTs(segment: ZoomSegment, timelineTs: number): NormalizedRect {
  const base = getSegmentBaseRect(segment);
  const { offsetX, offsetY } = panOffsetAtTime(getSortedPanTrajectory(segment), timelineTs);

  return normalizeRect({
    x: base.x + offsetX,
    y: base.y + offsetY,
    width: base.width,
    height: base.height,
  });
}

function getSegmentTargetPoints(segment: ZoomSegment): TargetPoint[] {
  const explicitPoints = (segment.targetPoints ?? [])
    .map((point) => ({
      ts: clamp(point.ts, segment.startTs, segment.endTs),
      rect: normalizeRect(point.rect),
    }))
    .sort((a, b) => a.ts - b.ts);

  if (explicitPoints.length > 0) {
    const points: TargetPoint[] = [];
    if (explicitPoints[0].ts > segment.startTs) {
      points.push({ ts: segment.startTs, rect: explicitPoints[0].rect });
    }
    points.push(...explicitPoints);
    const last = points[points.length - 1];
    if (last.ts < segment.endTs) {
      points.push({ ts: segment.endTs, rect: last.rect });
    }
    return points;
  }

  const legacyPan = getSortedPanTrajectory(segment);
  if (legacyPan.length === 0) {
    const baseRect = getSegmentBaseRect(segment);
    return [
      { ts: segment.startTs, rect: baseRect },
      { ts: segment.endTs, rect: baseRect },
    ];
  }

  const points: TargetPoint[] = [];
  const startRect = getLegacyPanRectAtTimelineTs(segment, segment.startTs);
  points.push({ ts: segment.startTs, rect: startRect });
  for (const keyframe of legacyPan) {
    if (keyframe.ts < segment.startTs || keyframe.ts > segment.endTs) {
      continue;
    }
    points.push({
      ts: keyframe.ts,
      rect: getLegacyPanRectAtTimelineTs(segment, keyframe.ts),
    });
  }
  const endRect = getLegacyPanRectAtTimelineTs(segment, segment.endTs);
  points.push({ ts: segment.endTs, rect: endRect });
  points.sort((a, b) => a.ts - b.ts);
  return points;
}

function getTargetRectAtTs(segment: RuntimeSegment, timelineTs: number): NormalizedRect {
  if (segment.targetPoints.length === 0) {
    return segment.baseRect;
  }
  if (timelineTs <= segment.targetPoints[0].ts) {
    return segment.targetPoints[0].rect;
  }
  const last = segment.targetPoints[segment.targetPoints.length - 1];
  if (timelineTs >= last.ts) {
    return last.rect;
  }
  for (let index = segment.targetPoints.length - 1; index >= 0; index -= 1) {
    const point = segment.targetPoints[index];
    if (timelineTs >= point.ts) {
      return point.rect;
    }
  }
  return segment.targetPoints[0].rect;
}

function toRuntimeSegments(
  segments: ZoomSegment[],
  cursorSamples: CursorSample[],
  sourceWidth: number,
  sourceHeight: number
): RuntimeSegment[] {
  return [...segments]
    .sort((a, b) => a.startTs - b.startTs)
    .map((rawSegment) => {
      const segment = normalizeZoomSegment(rawSegment);
      const baseRect = getSegmentBaseRect(segment);
      const targetPoints =
        segment.mode === "follow-cursor"
          ? buildFollowCursorTargetPoints(segment, baseRect, cursorSamples, sourceWidth, sourceHeight)
          : getSegmentTargetPoints(segment);
      return {
        id: segment.id,
        startTs: segment.startTs,
        endTs: segment.endTs,
        isAuto: segment.isAuto,
        mode: normalizeSegmentMode(segment.mode),
        trigger: normalizeSegmentTrigger(segment.trigger, segment.isAuto),
        baseRect,
        targetPoints,
        spring: normalizeSpring(segment.spring),
      };
    });
}

function resolveRuntimeSegment(segments: RuntimeSegment[], timelineTs: number): RuntimeSegment | null {
  for (let index = 0; index < segments.length; index += 1) {
    const segment = segments[index];
    if (timelineTs >= segment.startTs && timelineTs < segment.endTs) {
      return segment;
    }
  }
  return null;
}

function springStep(
  current: number,
  velocity: number,
  target: number,
  spring: CameraSpring,
  dtSeconds: number
): { value: number; velocity: number } {
  const safeDt = clamp(dtSeconds, 0.0001, 0.1);
  const accel =
    ((target - current) * spring.stiffness - spring.damping * velocity) / spring.mass;
  const nextVelocity = velocity + accel * safeDt;
  return {
    value: current + nextVelocity * safeDt,
    velocity: nextVelocity,
  };
}

function buildSpringCameraTrack(
  runtimeSegments: RuntimeSegment[],
  durationMs: number,
  fps = PREVIEW_SPRING_FPS
): SpringCameraSample[] {
  if (durationMs <= 0) {
    return [{ ts: 0, rect: FULL_RECT }];
  }

  const stepMs = 1000 / Math.max(1, fps);
  const samples: SpringCameraSample[] = [{ ts: 0, rect: FULL_RECT }];
  let rect = { ...FULL_RECT };
  let vx = 0;
  let vy = 0;
  let vw = 0;
  let vh = 0;
  let previousTs = 0;
  let frame = 1;

  while (previousTs < durationMs) {
    const ts = Math.min(Math.round(frame * stepMs), durationMs);
    frame += 1;
    if (ts <= previousTs) {
      continue;
    }
    // Integrate using target sampled at the start of the interval [previousTs, ts].
    // This keeps segment boundaries aligned with timeline bars.
    const activeSegment = resolveRuntimeSegment(runtimeSegments, previousTs);
    const targetRect = activeSegment ? getTargetRectAtTs(activeSegment, previousTs) : FULL_RECT;
    const spring = activeSegment?.spring ?? DEFAULT_SPRING;
    const dtSeconds = (ts - previousTs) / 1000;

    const stepX = springStep(rect.x, vx, targetRect.x, spring, dtSeconds);
    rect.x = stepX.value;
    vx = stepX.velocity;

    const stepY = springStep(rect.y, vy, targetRect.y, spring, dtSeconds);
    rect.y = stepY.value;
    vy = stepY.velocity;

    const stepW = springStep(rect.width, vw, targetRect.width, spring, dtSeconds);
    rect.width = stepW.value;
    vw = stepW.velocity;

    const stepH = springStep(rect.height, vh, targetRect.height, spring, dtSeconds);
    rect.height = stepH.value;
    vh = stepH.velocity;

    rect = normalizeRect(rect);
    samples.push({ ts, rect });
    previousTs = ts;
  }

  return samples;
}

function sampleCameraTrack(track: SpringCameraSample[], ts: number): NormalizedRect {
  if (track.length === 0) {
    return FULL_RECT;
  }
  if (ts <= track[0].ts) {
    return track[0].rect;
  }
  const last = track[track.length - 1];
  if (ts >= last.ts) {
    return last.rect;
  }

  let low = 0;
  let high = track.length - 1;
  while (low <= high) {
    const mid = Math.floor((low + high) / 2);
    if (track[mid].ts === ts) {
      return track[mid].rect;
    }
    if (track[mid].ts < ts) {
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }

  const next = track[low];
  const prev = track[Math.max(0, low - 1)];
  const span = Math.max(1, next.ts - prev.ts);
  const t = (ts - prev.ts) / span;
  return normalizeRect({
    x: prev.rect.x + (next.rect.x - prev.rect.x) * t,
    y: prev.rect.y + (next.rect.y - prev.rect.y) * t,
    width: prev.rect.width + (next.rect.width - prev.rect.width) * t,
    height: prev.rect.height + (next.rect.height - prev.rect.height) * t,
  });
}

function findTrackIndexAtOrAfter(track: SpringCameraSample[], ts: number): number {
  if (track.length === 0) {
    return -1;
  }
  if (ts <= track[0].ts) {
    return 0;
  }
  let low = 0;
  let high = track.length - 1;
  while (low <= high) {
    const mid = Math.floor((low + high) / 2);
    if (track[mid].ts < ts) {
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }
  return Math.min(low, track.length - 1);
}

function findTrackIndexAtOrBefore(track: SpringCameraSample[], ts: number): number {
  if (track.length === 0) {
    return -1;
  }
  if (ts >= track[track.length - 1].ts) {
    return track.length - 1;
  }
  let low = 0;
  let high = track.length - 1;
  while (low <= high) {
    const mid = Math.floor((low + high) / 2);
    if (track[mid].ts <= ts) {
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }
  return Math.max(0, high);
}

function resolveSegmentVisualBounds(
  segment: RuntimeSegment,
  track: SpringCameraSample[]
): { startTs: number; endTs: number } {
  if (track.length === 0) {
    return { startTs: segment.startTs, endTs: segment.endTs };
  }

  const rangeStart = findTrackIndexAtOrAfter(track, segment.startTs);
  const rangeEnd = findTrackIndexAtOrBefore(track, segment.endTs);
  if (rangeStart < 0 || rangeEnd < rangeStart) {
    return { startTs: segment.startTs, endTs: segment.endTs };
  }

  const motionDelta = (left: NormalizedRect, right: NormalizedRect) =>
    Math.max(
      Math.abs(left.x - right.x),
      Math.abs(left.y - right.y),
      Math.abs(left.width - right.width),
      Math.abs(left.height - right.height)
    );
  const isVisuallyActiveAt = (index: number) => {
    const sample = track[index];
    if (getZoomStrength(sample.rect) > 1 + TIMELINE_VISUAL_ZOOM_EPSILON) {
      return true;
    }
    if (index > 0) {
      return motionDelta(sample.rect, track[index - 1].rect) > TIMELINE_VISUAL_MOTION_EPSILON;
    }
    return false;
  };

  let peakIndex = rangeStart;
  let peakZoom = 1;
  for (let index = rangeStart; index <= rangeEnd; index += 1) {
    const zoom = getZoomStrength(track[index].rect);
    if (zoom > peakZoom) {
      peakZoom = zoom;
      peakIndex = index;
    }
  }

  if (peakZoom <= 1 + TIMELINE_VISUAL_ZOOM_EPSILON) {
    const firstMoving = (() => {
      for (let index = rangeStart; index <= rangeEnd; index += 1) {
        if (isVisuallyActiveAt(index)) {
          return index;
        }
      }
      return -1;
    })();
    if (firstMoving >= 0) {
      peakIndex = firstMoving;
    } else {
      return { startTs: segment.startTs, endTs: segment.endTs };
    }
  }

  let visualStartIndex = peakIndex;
  while (visualStartIndex > 0 && isVisuallyActiveAt(visualStartIndex - 1)) {
    visualStartIndex -= 1;
  }

  let visualEndIndex = peakIndex;
  while (visualEndIndex + 1 < track.length && isVisuallyActiveAt(visualEndIndex + 1)) {
    visualEndIndex += 1;
  }

  let startTs = track[visualStartIndex].ts;
  const endTs = track[visualEndIndex].ts;
  // Keep manual bars responsive: if a segment starts before noticeable zoom delta,
  // anchor the visual start to the segment boundary instead of lagging behind.
  if (segment.startTs < startTs) {
    startTs = segment.startTs;
  }

  if (endTs <= startTs) {
    return { startTs: segment.startTs, endTs: segment.endTs };
  }

  return {
    startTs,
    endTs,
  };
}

function updateSegmentBaseRect(segment: ZoomSegment, rect: NormalizedRect): ZoomSegment {
  const { targetRect: _legacyTargetRect, ...rest } = segment;
  return {
    ...rest,
    initialRect: normalizeRect(rect),
    spring: normalizeSpring(segment.spring),
    targetPoints: [],
    panTrajectory: [],
  };
}

function trimAutoNoopSegment(segment: ZoomSegment): ZoomSegment | null {
  if (!segment.isAuto) {
    return segment;
  }
  const points = getSegmentTargetPoints(segment);
  if (points.length === 0) {
    return segment;
  }

  const firstEffectiveIndex = points.findIndex(
    (point) => getZoomStrength(point.rect) > 1 + EFFECTIVE_ZOOM_EPSILON
  );
  if (firstEffectiveIndex < 0) {
    return null;
  }

  const effectiveStartTs = clamp(points[firstEffectiveIndex].ts, segment.startTs, segment.endTs);
  if (effectiveStartTs >= segment.endTs) {
    return null;
  }

  const trimmedPoints: TargetPoint[] = points
    .filter((point) => point.ts >= effectiveStartTs)
    .map((point) => ({
      ts: clamp(point.ts, effectiveStartTs, segment.endTs),
      rect: normalizeRect(point.rect),
    }));
  if (trimmedPoints.length === 0) {
    return null;
  }

  if (trimmedPoints[0].ts > effectiveStartTs) {
    trimmedPoints.unshift({ ts: effectiveStartTs, rect: trimmedPoints[0].rect });
  }
  const lastPoint = trimmedPoints[trimmedPoints.length - 1];
  if (lastPoint.ts < segment.endTs) {
    trimmedPoints.push({ ts: segment.endTs, rect: lastPoint.rect });
  }

  return {
    ...segment,
    startTs: effectiveStartTs,
    initialRect: trimmedPoints[0].rect,
    targetPoints: trimmedPoints,
  };
}

function sortSegments(segments: ZoomSegment[]): ZoomSegment[] {
  return [...segments]
    .map(normalizeZoomSegment)
    .map(trimAutoNoopSegment)
    .filter((segment): segment is ZoomSegment => segment !== null)
    .sort((a, b) => a.startTs - b.startTs);
}

function getSegmentNeighborBounds(
  segments: ZoomSegment[],
  segmentId: string,
  timelineDurationMs: number
): { prevEndTs: number; nextStartTs: number } {
  const sorted = [...segments].sort((a, b) => a.startTs - b.startTs);
  const index = sorted.findIndex((segment) => segment.id === segmentId);
  if (index < 0) {
    return {
      prevEndTs: 0,
      nextStartTs: Math.max(0, timelineDurationMs),
    };
  }

  return {
    prevEndTs: index > 0 ? sorted[index - 1].endTs : 0,
    nextStartTs:
      index + 1 < sorted.length ? sorted[index + 1].startTs : Math.max(0, timelineDurationMs),
  };
}

function findAvailableGapForSegment(
  segments: ZoomSegment[],
  timelineDurationMs: number,
  preferredStartTs: number
): { startTs: number; endTs: number } | null {
  const safeDuration = Math.max(0, timelineDurationMs);
  if (safeDuration < MIN_SEGMENT_MS) {
    return null;
  }

  const sorted = [...segments].sort((a, b) => a.startTs - b.startTs);
  const gaps: Array<{ start: number; end: number }> = [];
  let cursor = 0;
  for (const segment of sorted) {
    const segStart = clamp(segment.startTs, 0, safeDuration);
    const segEnd = clamp(segment.endTs, 0, safeDuration);
    if (segStart > cursor) {
      const gapStart = cursor <= 0 ? 0 : cursor + MIN_SEGMENT_GAP_MS;
      const gapEnd = segStart - MIN_SEGMENT_GAP_MS;
      if (gapEnd - gapStart >= MIN_SEGMENT_MS) {
        gaps.push({ start: gapStart, end: gapEnd });
      }
    }
    cursor = Math.max(cursor, segEnd);
  }
  if (cursor < safeDuration) {
    const gapStart = cursor <= 0 ? 0 : cursor + MIN_SEGMENT_GAP_MS;
    if (safeDuration - gapStart >= MIN_SEGMENT_MS) {
      gaps.push({ start: gapStart, end: safeDuration });
    }
  }

  const preferred = clamp(preferredStartTs, 0, safeDuration - MIN_SEGMENT_MS);
  const preferredGap =
    gaps.find((gap) => gap.end - gap.start >= MIN_SEGMENT_MS && preferred < gap.end) ?? null;
  if (!preferredGap) {
    return null;
  }

  const gapStart = preferredGap.start;
  const gapEnd = preferredGap.end;
  let startTs = clamp(preferred, gapStart, Math.max(gapStart, gapEnd - MIN_SEGMENT_MS));
  let endTs = Math.min(startTs + 1600, gapEnd);
  if (endTs - startTs < MIN_SEGMENT_MS) {
    startTs = Math.max(gapStart, gapEnd - MIN_SEGMENT_MS);
    endTs = gapEnd;
  }

  return { startTs, endTs };
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

function extractClickTimestamps(eventsFile: EventsFile | null): number[] {
  if (!eventsFile) {
    return [];
  }
  const clicks: number[] = [];
  for (const event of eventsFile.events) {
    if (event.type === "click") {
      clicks.push(event.ts);
    }
  }
  clicks.sort((a, b) => a - b);
  return clicks;
}

function sampleClickPulseScale(clickTimestamps: number[], ts: number): number {
  if (clickTimestamps.length === 0) {
    return 1;
  }

  let low = 0;
  let high = clickTimestamps.length - 1;
  let nearestIndex = -1;
  while (low <= high) {
    const mid = Math.floor((low + high) / 2);
    const clickTs = clickTimestamps[mid];
    if (clickTs <= ts) {
      nearestIndex = mid;
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }

  if (nearestIndex < 0) {
    return 1;
  }

  const dt = ts - clickTimestamps[nearestIndex];
  if (dt < 0 || dt > CLICK_PULSE_TOTAL_MS) {
    return 1;
  }

  if (dt <= CLICK_PULSE_DOWN_MS) {
    const t = dt / CLICK_PULSE_DOWN_MS;
    return 1 - (1 - CLICK_PULSE_MIN_SCALE) * t;
  }

  const upDuration = Math.max(1, CLICK_PULSE_TOTAL_MS - CLICK_PULSE_DOWN_MS);
  const t = (dt - CLICK_PULSE_DOWN_MS) / upDuration;
  return CLICK_PULSE_MIN_SCALE + (1 - CLICK_PULSE_MIN_SCALE) * t;
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

function getSegmentDisplayZoom(segment: ZoomSegment): number {
  const normalized = normalizeZoomSegment(segment);
  const baseZoom = getZoomStrength(getSegmentBaseRect(normalized));
  let maxZoom = baseZoom;
  for (const point of getSegmentTargetPoints(normalized)) {
    maxZoom = Math.max(maxZoom, getZoomStrength(point.rect));
  }
  return maxZoom;
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

function clampCenterToViewport(
  centerX: number,
  centerY: number,
  viewportWidth: number,
  viewportHeight: number
): { x: number; y: number } {
  const halfW = clamp(viewportWidth / 2, 0, 0.5);
  const halfH = clamp(viewportHeight / 2, 0, 0.5);
  return {
    x: clamp(centerX, halfW, 1 - halfW),
    y: clamp(centerY, halfH, 1 - halfH),
  };
}

function buildRectFromCenterAndSize(
  centerX: number,
  centerY: number,
  width: number,
  height: number
): NormalizedRect {
  const safeWidth = clamp(width, MIN_RECT_SIZE, 1);
  const safeHeight = clamp(height, MIN_RECT_SIZE, 1);
  const clamped = clampCenterToViewport(centerX, centerY, safeWidth, safeHeight);
  return normalizeRect({
    x: clamped.x - safeWidth / 2,
    y: clamped.y - safeHeight / 2,
    width: safeWidth,
    height: safeHeight,
  });
}

function buildFollowCursorTargetPoints(
  segment: ZoomSegment,
  baseRect: NormalizedRect,
  cursorSamples: CursorSample[],
  sourceWidth: number,
  sourceHeight: number
): TargetPoint[] {
  const startTs = segment.startTs;
  const endTs = segment.endTs;
  if (endTs <= startTs) {
    return [
      { ts: startTs, rect: baseRect },
      { ts: startTs + 1, rect: baseRect },
    ];
  }

  if (cursorSamples.length === 0) {
    return [
      { ts: startTs, rect: baseRect },
      { ts: endTs, rect: baseRect },
    ];
  }

  const safeWidthPx = Math.max(1, sourceWidth);
  const safeHeightPx = Math.max(1, sourceHeight);
  const stepMs = Math.max(FOLLOW_SAMPLE_STEP_MS, 1);
  const deadRatio = FOLLOW_DEAD_ZONE_RATIO;
  const hardRatio = FOLLOW_HARD_EDGE_RATIO;
  const maxSpeedX = FOLLOW_MAX_SPEED_PX_PER_S / safeWidthPx;
  const maxSpeedY = FOLLOW_MAX_SPEED_PX_PER_S / safeHeightPx;

  let centerX = baseRect.x + baseRect.width / 2;
  let centerY = baseRect.y + baseRect.height / 2;
  const points: TargetPoint[] = [];
  let ts = startTs;
  let lastTs = startTs;

  while (ts <= endTs) {
    const dtSec = Math.max(0, ts - lastTs) / 1000;
    lastTs = ts;
    const cursor = interpolateCursor(cursorSamples, ts);
    const offsetX = cursor.x - centerX;
    const offsetY = cursor.y - centerY;
    const deadX = baseRect.width * 0.5 * deadRatio;
    const deadY = baseRect.height * 0.5 * deadRatio;
    const hardX = baseRect.width * 0.5 * hardRatio;
    const hardY = baseRect.height * 0.5 * hardRatio;

    if (Math.abs(offsetX) > deadX) {
      const excess = Math.abs(offsetX) - deadX;
      const range = Math.max(hardX - deadX, 0.0001);
      const speedFactor = clamp(excess / range, 0, 1);
      centerX += Math.sign(offsetX) * speedFactor * maxSpeedX * dtSec;
    }
    if (Math.abs(offsetY) > deadY) {
      const excess = Math.abs(offsetY) - deadY;
      const range = Math.max(hardY - deadY, 0.0001);
      const speedFactor = clamp(excess / range, 0, 1);
      centerY += Math.sign(offsetY) * speedFactor * maxSpeedY * dtSec;
    }

    const rect = buildRectFromCenterAndSize(centerX, centerY, baseRect.width, baseRect.height);
    centerX = rect.x + rect.width / 2;
    centerY = rect.y + rect.height / 2;
    points.push({ ts, rect });
    ts += stepMs;
  }

  if (points[points.length - 1].ts !== endTs) {
    const lastRect = points[points.length - 1].rect;
    points.push({ ts: endTs, rect: lastRect });
  }
  return points;
}

function chooseMarkerStepMs(pxPerMs: number): number {
  const targetSpacingPx = 90;
  const approxStepMs = targetSpacingPx / Math.max(pxPerMs, 0.0001);
  const options = [
    250,
    500,
    1_000,
    2_000,
    5_000,
    10_000,
    15_000,
    30_000,
    60_000,
    120_000,
    300_000,
    600_000,
    900_000,
    1_800_000,
  ];
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
  const [timelineZoomPercent, setTimelineZoomPercent] = useState(
    TIMELINE_DEFAULT_ZOOM_PERCENT
  );
  const [timelineViewportWidthPx, setTimelineViewportWidthPx] = useState(0);
  const [isRefreshingProjects, setIsRefreshingProjects] = useState(false);
  const [isLoadingProject, setIsLoadingProject] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isVideoPlaying, setIsVideoPlaying] = useState(false);
  const [previewVolume, setPreviewVolume] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const [videoError, setVideoError] = useState<string | null>(null);
  const [eventsError, setEventsError] = useState<string | null>(null);

  const videoRef = useRef<HTMLVideoElement | null>(null);
  const previewStageRef = useRef<HTMLDivElement | null>(null);
  const previewCanvasRef = useRef<HTMLDivElement | null>(null);
  const cursorRef = useRef<HTMLDivElement | null>(null);
  const timelinePlayheadRef = useRef<HTMLDivElement | null>(null);
  const timelineViewportRef = useRef<HTMLDivElement | null>(null);
  const rafRef = useRef<number | null>(null);
  const dragStateRef = useRef<SegmentDragState | null>(null);
  const playheadRef = useRef(0);
  const playheadStateRef = useRef(0);
  const lastStateSyncAtRef = useRef(0);
  const playbackClockRef = useRef<{ anchorPerfMs: number; anchorPreviewMs: number } | null>(null);
  const playRequestSeqRef = useRef(0);

  const timelineDurationMs = project?.durationMs ?? 0;
  const previewDurationMs = useMemo(() => {
    if (!Number.isFinite(videoDurationMs) || !videoDurationMs || videoDurationMs <= 0) {
      return timelineDurationMs;
    }
    return Math.round(videoDurationMs);
  }, [timelineDurationMs, videoDurationMs]);

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
  const previewCursorSizePx = useMemo(() => {
    if (!project) {
      return 12;
    }
    const minSide = Math.max(1, Math.min(previewFrameSize.width, previewFrameSize.height));
    return clamp(project.settings.cursor.size * minSide * CURSOR_SIZE_TO_FRAME_RATIO, 8, 280);
  }, [previewFrameSize.height, previewFrameSize.width, project]);
  const previewCursorAspect = VECTOR_CURSOR_WIDTH / VECTOR_CURSOR_HEIGHT;
  const previewCursorWidthPx = useMemo(
    () => Math.max(4, previewCursorSizePx * previewCursorAspect),
    [previewCursorAspect, previewCursorSizePx]
  );
  const previewCursorHeightPx = previewCursorSizePx;
  const previewCursorHotspotPx = { x: 0, y: 0 };

  const timelineVisibleWindowMs = useMemo(() => {
    if (previewDurationMs <= 0) {
      return TIMELINE_MAX_VISIBLE_WINDOW_MS;
    }
    const fullDurationMs = Math.max(1, Math.round(previewDurationMs));
    const maxZoomVisibleMs = Math.min(fullDurationMs, TIMELINE_MAX_VISIBLE_WINDOW_MS);
    if (fullDurationMs <= maxZoomVisibleMs) {
      return fullDurationMs;
    }

    const clampedPercent = clamp(
      timelineZoomPercent,
      TIMELINE_MIN_ZOOM_PERCENT,
      TIMELINE_MAX_ZOOM_PERCENT
    );
    const progress =
      (clampedPercent - TIMELINE_MIN_ZOOM_PERCENT) /
      (TIMELINE_MAX_ZOOM_PERCENT - TIMELINE_MIN_ZOOM_PERCENT);
    return Math.round(fullDurationMs - (fullDurationMs - maxZoomVisibleMs) * progress);
  }, [previewDurationMs, timelineZoomPercent]);

  const timelineLaneViewportWidthPx = useMemo(
    () =>
      Math.max(
        1,
        (timelineViewportWidthPx || 900) - TIMELINE_LABEL_WIDTH_PX - TIMELINE_LANE_RIGHT_MARGIN_PX
      ),
    [timelineViewportWidthPx]
  );

  const timelineLaneContentWidthPx = useMemo(() => {
    const viewportWidthPx = Math.max(1, timelineLaneViewportWidthPx || 1);
    if (previewDurationMs <= 0) {
      return viewportWidthPx;
    }
    const visibleWindowMs = clamp(timelineVisibleWindowMs, 1, previewDurationMs);
    const scaled = Math.round((previewDurationMs / visibleWindowMs) * viewportWidthPx);
    return Math.max(viewportWidthPx, scaled);
  }, [previewDurationMs, timelineLaneViewportWidthPx, timelineVisibleWindowMs]);

  const timelineContentWidthPx =
    timelineLaneContentWidthPx + TIMELINE_LABEL_WIDTH_PX + TIMELINE_LANE_RIGHT_MARGIN_PX;

  const pxPerPreviewMs = timelineLaneContentWidthPx / Math.max(previewDurationMs, 1);

  const timelineSegments = useMemo(
    () => sortSegments(project?.timeline.zoomSegments ?? []),
    [project?.timeline.zoomSegments]
  );
  const cursorSamples = useMemo(
    () => extractCursorSamples(eventsFile, project?.settings.cursor.smoothingFactor ?? 0.8),
    [eventsFile, project?.settings.cursor.smoothingFactor]
  );
  const clickTimestamps = useMemo(() => extractClickTimestamps(eventsFile), [eventsFile]);
  const runtimeSegments = useMemo(
    () =>
      toRuntimeSegments(
        timelineSegments,
        cursorSamples,
        project?.videoWidth ?? 1920,
        project?.videoHeight ?? 1080
      ),
    [timelineSegments, cursorSamples, project?.videoHeight, project?.videoWidth]
  );

  const selectedSegment = useMemo(() => {
    if (!project || !selectedSegmentId) {
      return null;
    }
    return project.timeline.zoomSegments.find((segment) => segment.id === selectedSegmentId) ?? null;
  }, [project, selectedSegmentId]);

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

  const previewCameraTrack = useMemo(
    () => buildSpringCameraTrack(runtimeSegments, timelineDurationMs),
    [runtimeSegments, timelineDurationMs]
  );
  const runtimeSegmentsById = useMemo(
    () => new Map(runtimeSegments.map((segment) => [segment.id, segment])),
    [runtimeSegments]
  );

  const renderPreviewFrame = useCallback(
    (previewMs: number) => {
      if (previewDurationMs <= 0 || timelineDurationMs <= 0) {
        return;
      }

      const clampedPreviewMs = clamp(previewMs, 0, previewDurationMs);
      playheadRef.current = clampedPreviewMs;
      const timelineMs = mapTimeMs(clampedPreviewMs, previewDurationMs, timelineDurationMs);
      const cursorTimelineMs = clamp(
        timelineMs + CURSOR_TIMING_OFFSET_MS,
        0,
        timelineDurationMs
      );
      const rect = sampleCameraTrack(previewCameraTrack, timelineMs);
      const scale = 1 / Math.max(rect.width, rect.height);
      const centerX = rect.x + rect.width / 2;
      const centerY = rect.y + rect.height / 2;
      const txPx = (0.5 - centerX * scale) * previewFrameSize.width;
      const tyPx = (0.5 - centerY * scale) * previewFrameSize.height;

      if (previewCanvasRef.current) {
        previewCanvasRef.current.style.transform = `translate3d(${txPx.toFixed(
          3
        )}px, ${tyPx.toFixed(3)}px, 0) scale(${scale.toFixed(6)})`;
      }

      if (cursorRef.current) {
        const cursor = interpolateCursor(cursorSamples, cursorTimelineMs);
        const cursorVideoX = cursor.x * previewFrameSize.width;
        const cursorVideoY = cursor.y * previewFrameSize.height;
        const cursorX = cursorVideoX * scale + txPx;
        const cursorY = cursorVideoY * scale + tyPx;
        const cursorPulseScale = sampleClickPulseScale(clickTimestamps, cursorTimelineMs);
        const cursorScale = Math.max(0.25, scale) * cursorPulseScale;
        const topLeftX = cursorX - previewCursorHotspotPx.x;
        const topLeftY = cursorY - previewCursorHotspotPx.y;
        cursorRef.current.style.transform = `translate3d(${topLeftX.toFixed(
          3
        )}px, ${topLeftY.toFixed(3)}px, 0) scale(${cursorScale.toFixed(4)})`;
      }

      if (timelinePlayheadRef.current) {
        const leftPx =
          TIMELINE_LABEL_WIDTH_PX +
          clamp(clampedPreviewMs * pxPerPreviewMs, 0, timelineLaneContentWidthPx);
        timelinePlayheadRef.current.style.transform = `translate3d(${(leftPx - 1).toFixed(
          2
        )}px, 0, 0)`;
      }
    },
    [
      clickTimestamps,
      cursorSamples,
      previewCameraTrack,
      previewDurationMs,
      previewFrameSize.height,
      previewFrameSize.width,
      previewCursorHotspotPx.x,
      previewCursorHotspotPx.y,
      pxPerPreviewMs,
      timelineLaneContentWidthPx,
      timelineContentWidthPx,
      timelineDurationMs,
    ]
  );

  const segmentVisuals = useMemo<TimelineSegmentVisual[]>(() => {
    if (!project || previewDurationMs <= 0 || timelineDurationMs <= 0) {
      return [];
    }

    const rawVisuals: RawTimelineSegmentVisual[] = timelineSegments.map((segment) => {
      const runtime = runtimeSegmentsById.get(segment.id);
      const visualBounds = runtime
        ? resolveSegmentVisualBounds(runtime, previewCameraTrack)
        : { startTs: segment.startTs, endTs: segment.endTs };
      let startTs = clamp(visualBounds.startTs, segment.startTs, timelineDurationMs);
      let endTs = clamp(visualBounds.endTs, startTs + 1, timelineDurationMs);
      const maxOwnTailEndTs = Math.min(
        timelineDurationMs,
        segment.endTs + TIMELINE_VISUAL_RETURN_TAIL_MS
      );
      endTs = clamp(endTs, startTs + 1, Math.max(startTs + 1, maxOwnTailEndTs));
      if (endTs <= startTs) {
        startTs = clamp(segment.startTs, 0, timelineDurationMs);
        endTs = clamp(
          Math.max(segment.endTs, startTs + 1),
          startTs + 1,
          timelineDurationMs
        );
      }

      const startPreviewMs = mapTimeMs(startTs, timelineDurationMs, previewDurationMs);
      const endPreviewMs = mapTimeMs(endTs, timelineDurationMs, previewDurationMs);
      const leftPx = clamp(startPreviewMs * pxPerPreviewMs, 0, timelineLaneContentWidthPx);
      const naturalWidthPx = Math.max(
        (endPreviewMs - startPreviewMs) * pxPerPreviewMs,
        TIMELINE_MIN_VISIBLE_SEGMENT_WIDTH_PX
      );

      return {
        id: segment.id,
        startPreviewMs,
        endPreviewMs,
        leftPx,
        naturalWidthPx,
        isAuto: segment.isAuto,
      };
    });

    return rawVisuals.map((visual) => {
      const leftPx = Math.max(0, visual.leftPx);
      let widthPx = Math.max(visual.naturalWidthPx, TIMELINE_MIN_SEGMENT_WIDTH_PX);

      const maxVisibleWidthPx = Math.max(
        TIMELINE_MIN_VISIBLE_SEGMENT_WIDTH_PX,
        timelineLaneContentWidthPx - leftPx
      );
      widthPx = Math.min(widthPx, maxVisibleWidthPx);

      return {
        id: visual.id,
        startPreviewMs: visual.startPreviewMs,
        endPreviewMs: visual.endPreviewMs,
        leftPx,
        widthPx,
        isAuto: visual.isAuto,
      };
    });
  }, [
    project,
    previewDurationMs,
    timelineDurationMs,
    timelineSegments,
    pxPerPreviewMs,
    previewCameraTrack,
    runtimeSegmentsById,
    timelineLaneContentWidthPx,
    timelineContentWidthPx,
  ]);

  const markerStepMs = useMemo(() => chooseMarkerStepMs(pxPerPreviewMs), [pxPerPreviewMs]);
  const timelineMarkers = useMemo(() => {
    if (previewDurationMs <= 0 || markerStepMs <= 0) {
      return [];
    }

    const markers: Array<{ ms: number; leftPx: number }> = [];
    for (let ms = 0; ms <= previewDurationMs; ms += markerStepMs) {
      markers.push({
        ms,
        leftPx:
          TIMELINE_LABEL_WIDTH_PX +
          clamp(ms * pxPerPreviewMs, 0, timelineLaneContentWidthPx),
      });
    }
    if (markers[markers.length - 1]?.ms !== previewDurationMs) {
      markers.push({
        ms: previewDurationMs,
        leftPx: TIMELINE_LABEL_WIDTH_PX + timelineLaneContentWidthPx,
      });
    }
    return markers;
  }, [previewDurationMs, markerStepMs, pxPerPreviewMs, timelineLaneContentWidthPx]);

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

  const loadProjectByPath = async (projectPath: string, _showLoadedInfo = true) => {
    setError(null);
    setVideoError(null);
    setEventsError(null);
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
      playheadRef.current = 0;
      playheadStateRef.current = 0;
      setPlayheadMs(0);
      setTimelineZoomPercent(TIMELINE_DEFAULT_ZOOM_PERCENT);
      setVideoDurationMs(null);
      setIsVideoPlaying(false);
      setLoadedProjectPath(projectPath);
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
        }
        return;
      }

      if (autoLoadLatest) {
        await loadProjectByPath(listed[0].projectPath, false);
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
      const preferredVideoPath =
        project?.videoPath?.trim() && project.videoPath.trim().length > 0
          ? project.videoPath.trim()
          : project?.proxyVideoPath?.trim() ?? "";

      if (!project || !loadedProjectPath || !preferredVideoPath) {
        setVideoSrc(null);
        setVideoDurationMs(null);
        setVideoError(null);
        return;
      }

      try {
        const sourcePath = preferredVideoPath;
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
  }, [project?.proxyVideoPath, project?.videoPath, loadedProjectPath]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) {
      return;
    }
    const clampedVolume = clamp(previewVolume, 0, 1);
    video.volume = clampedVolume;
    video.muted = clampedVolume <= 0.001;
  }, [previewVolume, videoSrc]);

  useEffect(() => {
    playheadStateRef.current = playheadMs;
  }, [playheadMs]);

  useEffect(() => {
    if (!videoRef.current || previewDurationMs <= 0 || isVideoPlaying) {
      return;
    }

    const video = videoRef.current;
    const targetTimeSec = clamp(playheadMs, 0, previewDurationMs) / 1000;
    if (Math.abs(video.currentTime - targetTimeSec) > 0.05) {
      video.currentTime = targetTimeSec;
    }
  }, [isVideoPlaying, playheadMs, previewDurationMs]);

  useEffect(() => {
    // Invalidate in-flight play() promises when source/project changes.
    playRequestSeqRef.current += 1;
    playbackClockRef.current = null;
    setIsVideoPlaying(false);
  }, [videoSrc]);

  useEffect(() => {
    if (previewDurationMs <= 0) {
      return;
    }
    setPlayheadMs((current) => {
      const clamped = clamp(current, 0, previewDurationMs);
      playheadRef.current = clamped;
      return clamped;
    });
  }, [previewDurationMs]);

  useEffect(() => {
    if (!isVideoPlaying || previewDurationMs <= 0) {
      return;
    }

    playbackClockRef.current = {
      anchorPerfMs: performance.now(),
      anchorPreviewMs: clamp(playheadRef.current, 0, previewDurationMs),
    };

    const updateFromVideo = () => {
      const video = videoRef.current;
      if (!video || video.paused || video.ended) {
        playbackClockRef.current = null;
        setIsVideoPlaying(false);
        return;
      }

      const clock = playbackClockRef.current;
      const playbackRate = Number.isFinite(video.playbackRate) ? video.playbackRate : 1;
      let nextMs = clamp(Math.round(video.currentTime * 1000), 0, previewDurationMs);
      if (clock) {
        const predictedMs = clamp(
          Math.round(clock.anchorPreviewMs + (performance.now() - clock.anchorPerfMs) * playbackRate),
          0,
          previewDurationMs
        );
        const decoderMs = clamp(Math.round(video.currentTime * 1000), 0, previewDurationMs);
        if (Math.abs(decoderMs - predictedMs) > 70) {
          playbackClockRef.current = {
            anchorPerfMs: performance.now(),
            anchorPreviewMs: decoderMs,
          };
          nextMs = decoderMs;
        } else {
          nextMs = predictedMs;
        }
      }
      renderPreviewFrame(nextMs);

      const now = performance.now();
      if (
        now - lastStateSyncAtRef.current >= PLAYHEAD_STATE_SYNC_INTERVAL_MS ||
        Math.abs(nextMs - playheadStateRef.current) >= PLAYHEAD_STATE_SYNC_INTERVAL_MS
      ) {
        lastStateSyncAtRef.current = now;
        setPlayheadMs(nextMs);
      }

      rafRef.current = requestAnimationFrame(updateFromVideo);
    };

    lastStateSyncAtRef.current = performance.now();
    rafRef.current = requestAnimationFrame(updateFromVideo);

    return () => {
      playbackClockRef.current = null;
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, [isVideoPlaying, previewDurationMs, renderPreviewFrame]);

  useEffect(() => {
    renderPreviewFrame(playheadRef.current || playheadMs);
  }, [playheadMs, renderPreviewFrame]);

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

        const neighbors = getSegmentNeighborBounds(
          current.timeline.zoomSegments,
          drag.segmentId,
          timelineDurationMs
        );

        const nextSegments = current.timeline.zoomSegments.map((segment) => {
          if (segment.id !== drag.segmentId) {
            return segment;
          }

          if (drag.mode === "move") {
            const length = drag.initialEndTs - drag.initialStartTs;
            const rawStartTs = drag.initialStartTs + deltaTimelineMs;
            const minStartTs = Math.max(0, neighbors.prevEndTs + MIN_SEGMENT_GAP_MS);
            const maxStartTs = Math.min(
              Math.max(0, timelineDurationMs - length),
              Math.max(minStartTs, neighbors.nextStartTs - MIN_SEGMENT_GAP_MS - length)
            );
            if (maxStartTs < minStartTs) {
              return {
                ...segment,
                isAuto: false,
              };
            }
            const startTs = clamp(rawStartTs, minStartTs, maxStartTs);
            return {
              ...segment,
              startTs,
              endTs: startTs + length,
              isAuto: false,
            };
          }

          if (drag.mode === "start") {
            const rawStartTs = drag.initialStartTs + deltaTimelineMs;
            const minStartTs = Math.max(0, neighbors.prevEndTs + MIN_SEGMENT_GAP_MS);
            const hardMaxStartTs = drag.initialEndTs - 1;
            const preferredMaxStartTs = drag.initialEndTs - MIN_SEGMENT_MS;
            const maxStartTs = Math.max(
              minStartTs,
              Math.min(hardMaxStartTs, preferredMaxStartTs)
            );
            return {
              ...segment,
              startTs: clamp(rawStartTs, minStartTs, maxStartTs),
              isAuto: false,
            };
          }

          const rawEndTs = drag.initialEndTs + deltaTimelineMs;
          const hardMinEndTs = drag.initialStartTs + 1;
          const preferredMinEndTs = drag.initialStartTs + MIN_SEGMENT_MS;
          const maxEndTs = Math.min(timelineDurationMs, neighbors.nextStartTs - MIN_SEGMENT_GAP_MS);
          const minEndTs = Math.min(
            Math.max(hardMinEndTs, preferredMinEndTs),
            Math.max(hardMinEndTs, maxEndTs)
          );
          return {
            ...segment,
            endTs: clamp(rawEndTs, minEndTs, Math.max(minEndTs, maxEndTs)),
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
    setIsSaving(true);
    try {
      const runtimeById = new Map(runtimeSegments.map((segment) => [segment.id, segment]));
      const projectForSave: Project = {
        ...project,
        timeline: {
          ...project.timeline,
          zoomSegments: sortSegments(
            project.timeline.zoomSegments.map((rawSegment) => {
              const segment = normalizeZoomSegment(rawSegment);
              if (segment.mode !== "follow-cursor") {
                return segment;
              }
              const runtime = runtimeById.get(segment.id);
              if (!runtime) {
                return segment;
              }
              return {
                ...segment,
                targetPoints: runtime.targetPoints,
              };
            })
          ),
        },
      };

      const savedPath = await invoke<string>("save_project", {
        project: projectForSave,
        projectPath: loadedProjectPath,
      });
      setProject(projectForSave);
      setLoadedProjectPath(savedPath);
      await refreshProjects(false);
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

    const livePlayheadTimelineMs = mapTimeMs(playheadRef.current, previewDurationMs, timelineDurationMs);
    const slot = findAvailableGapForSegment(
      project.timeline.zoomSegments,
      timelineDurationMs,
      livePlayheadTimelineMs
    );
    if (!slot) {
      setError("No free space on timeline for a new zoom segment.");
      return;
    }
    const { startTs, endTs } = slot;
    const nextId = `manual-${Date.now()}`;
    const rect = sampleCameraTrack(previewCameraTrack, livePlayheadTimelineMs) ?? DEFAULT_RECT;

    const newSegment: ZoomSegment = {
      id: nextId,
      startTs,
      endTs,
      initialRect: normalizeRect(rect),
      targetPoints: [],
      spring: { ...DEFAULT_SPRING },
      panTrajectory: [],
      mode: "fixed",
      trigger: "manual",
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
    playbackClockRef.current = null;
    playheadRef.current = clampedMs;
    setPlayheadMs(clampedMs);
    renderPreviewFrame(clampedMs);
    if (videoRef.current) {
      videoRef.current.currentTime = clampedMs / 1000;
    }
  };

  const seekBy = (deltaMs: number) => {
    seekToPreviewMs(playheadRef.current + deltaMs);
  };

  const togglePlayback = async () => {
    const video = videoRef.current;
    if (!video) {
      return;
    }

    const clampedVolume = clamp(previewVolume, 0, 1);
    video.volume = clampedVolume;
    video.muted = clampedVolume <= 0.001;
    setVideoError(null);

    if (video.paused || video.ended) {
      const requestSeq = playRequestSeqRef.current + 1;
      playRequestSeqRef.current = requestSeq;
      try {
        await video.play();
        if (requestSeq !== playRequestSeqRef.current) {
          return;
        }
        playbackClockRef.current = {
          anchorPerfMs: performance.now(),
          anchorPreviewMs: clamp(playheadRef.current, 0, previewDurationMs),
        };
        setIsVideoPlaying(true);
      } catch (err) {
        if (requestSeq !== playRequestSeqRef.current || isExpectedPlaybackAbort(err)) {
          return;
        }
        setVideoError(`Failed to play video: ${String(err)}`);
      }
      return;
    }

    playRequestSeqRef.current += 1;
    video.pause();
    playbackClockRef.current = null;
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

    const lane =
      (target.closest(".timeline-row-lane") as HTMLElement | null) ??
      (event.currentTarget.querySelector(".timeline-row-lane") as HTMLElement | null);
    if (!lane) {
      return;
    }

    const rect = lane.getBoundingClientRect();
    const localX = clamp(event.clientX - rect.left, 0, rect.width);
    const nextMs = Math.round((localX / Math.max(rect.width, 1)) * previewDurationMs);
    seekToPreviewMs(nextMs);
  };

  const previewVolumePercent = Math.round(clamp(previewVolume, 0, 1) * 100);

  return (
    <div className="edit-shell">
      {error && <div className="edit-banner edit-banner--error">{error}</div>}
      {videoError && <div className="edit-banner edit-banner--error">{videoError}</div>}
      {eventsError && <div className="edit-banner edit-banner--error">{eventsError}</div>}

      {!project && (
        <>
          <section className="editor-toolbar">
            <div className="project-picker">
              <label htmlFor="project-select-empty">Project</label>
              <select
                id="project-select-empty"
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
              <button
                className="btn-ghost"
                onClick={() => void refreshProjects(false)}
                disabled={isRefreshingProjects}
              >
                {isRefreshingProjects ? "Refreshing..." : "Refresh"}
              </button>
              <button className="btn-primary" onClick={handleSaveProject} disabled>
                Save
              </button>
            </div>
          </section>

          <section className="editor-empty">
            <h2>Project is not loaded</h2>
            <p>Create recording and choose it from dropdown.</p>
          </section>
        </>
      )}

      {project && (
        <>
          <section className="editor-main">
            <aside className="editor-sidebar">
              <section className="sidebar-project-toolbar">
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
                  <button
                    className="btn-ghost"
                    onClick={() => void refreshProjects(false)}
                    disabled={isRefreshingProjects}
                  >
                    {isRefreshingProjects ? "Refreshing..." : "Refresh"}
                  </button>
                  <button className="btn-primary" onClick={handleSaveProject} disabled={!project || isSaving}>
                    {isSaving ? "Saving..." : "Save"}
                  </button>
                </div>
              </section>

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
                    <span>Camera Mode</span>
                    <select
                      value={normalizeSegmentMode(selectedSegment.mode)}
                      onChange={(event) =>
                        updateSegment(selectedSegment.id, (segment) => ({
                          ...normalizeZoomSegment(segment),
                          mode: event.target.value as ZoomMode,
                          targetPoints:
                            event.target.value === "fixed" ? [] : segment.targetPoints ?? [],
                          isAuto: false,
                        }))
                      }
                    >
                      <option value="fixed">Locked</option>
                      <option value="follow-cursor">Follow cursor</option>
                    </select>
                  </label>

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
                  <div className="preview-canvas" ref={previewCanvasRef}>
                    {videoSrc ? (
                      <video
                        ref={videoRef}
                        className="preview-video"
                        src={videoSrc}
                        preload="metadata"
                        playsInline
                        muted={previewVolume <= 0.001}
                        onPlay={(event) => {
                          playbackClockRef.current = {
                            anchorPerfMs: performance.now(),
                            anchorPreviewMs: clamp(
                              Math.round(event.currentTarget.currentTime * 1000),
                              0,
                              previewDurationMs
                            ),
                          };
                          setIsVideoPlaying(true);
                        }}
                        onPause={() => {
                          playbackClockRef.current = null;
                          setIsVideoPlaying(false);
                        }}
                        onEnded={() => {
                          playbackClockRef.current = null;
                          setIsVideoPlaying(false);
                        }}
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
                          if (isVideoPlaying) {
                            return;
                          }
                          const nextMs = clamp(
                            Math.round(event.currentTarget.currentTime * 1000),
                            0,
                            previewDurationMs
                          );
                          playheadRef.current = nextMs;
                          setPlayheadMs(nextMs);
                          renderPreviewFrame(nextMs);
                        }}
                        onSeeking={(event) => {
                          const nextMs = clamp(
                            Math.round(event.currentTarget.currentTime * 1000),
                            0,
                            previewDurationMs
                          );
                          playheadRef.current = nextMs;
                          setPlayheadMs(nextMs);
                          renderPreviewFrame(nextMs);
                        }}
                        onError={() =>
                          setVideoError("Failed to load project video. Check file availability and asset scope.")
                        }
                      />
                    ) : (
                      <div className="preview-video-placeholder">Video source is unavailable for this project.</div>
                    )}

                    <div className="preview-overlay-grid" />
                  </div>
                  <div
                    ref={cursorRef}
                    className="preview-cursor preview-cursor--vector"
                    style={{
                      width: `${previewCursorWidthPx}px`,
                      height: `${previewCursorHeightPx}px`,
                      transformOrigin: `${previewCursorHotspotPx.x}px ${previewCursorHotspotPx.y}px`,
                      backgroundImage: `url("${VECTOR_CURSOR_DATA_URI}")`,
                    }}
                  />
                </div>
              </div>

              <div className="preview-controls">
                <div className="preview-controls-row">
                  <button
                    className="btn-ghost preview-control-btn"
                    onClick={() => seekBy(-5000)}
                    aria-label="Back 5 seconds"
                    title="Back 5 seconds"
                  >
                    <SeekBackIcon />
                  </button>
                  <button
                    className="btn-primary preview-control-btn"
                    onClick={() => void togglePlayback()}
                    aria-label={isVideoPlaying ? "Pause" : "Play"}
                    title={isVideoPlaying ? "Pause" : "Play"}
                  >
                    {isVideoPlaying ? <PauseIcon /> : <PlayIcon />}
                  </button>
                  <button
                    className="btn-ghost preview-control-btn"
                    onClick={() => seekBy(5000)}
                    aria-label="Forward 5 seconds"
                    title="Forward 5 seconds"
                  >
                    <SeekForwardIcon />
                  </button>
                </div>
                <div className="preview-volume-row">
                  <span className="preview-volume-icon" aria-hidden="true">
                    <VolumeIcon muted={previewVolumePercent === 0} />
                  </span>
                  <input
                    type="range"
                    className="preview-volume-slider"
                    min={0}
                    max={1}
                    step={0.01}
                    value={previewVolume}
                    onChange={(event) => setPreviewVolume(Number(event.target.value))}
                    aria-label="Preview volume"
                    disabled={!videoSrc}
                  />
                  <span className="preview-volume-value mono">{previewVolumePercent}%</span>
                </div>
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
                  min={TIMELINE_MIN_ZOOM_PERCENT}
                  max={TIMELINE_MAX_ZOOM_PERCENT}
                  step={1}
                  value={timelineZoomPercent}
                  onChange={(event) => setTimelineZoomPercent(Number(event.target.value))}
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
                        const zoom = getSegmentDisplayZoom(segment);
                        const modeLabel =
                          normalizeSegmentMode(segment.mode) === "follow-cursor" ? "Follow" : "Locked";

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
                            <span>
                              {visual.isAuto ? "A" : "M"} {modeLabel} {zoom.toFixed(1)}x
                            </span>
                            <div
                              className="timeline-segment-handle timeline-segment-handle--end"
                              onPointerDown={(event) => startDragSegment(event, segment, "end")}
                            />
                          </div>
                        );
                      })}
                    </div>
                  </div>

                  <div className="timeline-playhead" ref={timelinePlayheadRef} />
                </div>
              </div>
            </div>
          </section>
        </>
      )}
    </div>
  );
}
