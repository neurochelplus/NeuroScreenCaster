/**
 * Контракт project.json — schemaVersion: 1
 * Неразрушающий проект записи с таймлайном и настройками рендера.
 */

export const PROJECT_SCHEMA_VERSION = 1 as const;

// --- Примитивы ---

/** Прямоугольник в нормализованных координатах [0.0–1.0]. */
export interface NormalizedRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

// --- Таймлайн ---

export interface PanKeyframe {
  ts: number;
  offsetX: number;
  offsetY: number;
}

export interface TargetPoint {
  ts: number;
  rect: NormalizedRect;
}

export interface CameraSpring {
  mass: number;
  stiffness: number;
  damping: number;
}

export interface ZoomSegment {
  id: string;
  /** Начало сегмента (мс от начала записи). */
  startTs: number;
  /** Конец сегмента (мс). */
  endTs: number;
  /** Целевая область просмотра (нормализованные координаты). */
  initialRect?: NormalizedRect;
  targetRect?: NormalizedRect;
  targetPoints?: TargetPoint[];
  spring?: CameraSpring;
  panTrajectory?: PanKeyframe[];
  /** true — создан алгоритмом авто-зума; false — пользователем вручную. */
  isAuto: boolean;
}

export interface Timeline {
  zoomSegments: ZoomSegment[];
}

// --- Настройки ---

export interface CursorSettings {
  /** Относительный размер, 1.0 = нормальный. */
  size: number;
  color: string;
  /** [0.0, 1.0] — сила сглаживания траектории. */
  smoothingFactor: number;
}

export type Background =
  | { type: "solid"; color: string }
  | { type: "gradient"; from: string; to: string; direction: string };

export interface ExportSettings {
  width: number;
  height: number;
  fps: number;
  codec: "h264" | "h265" | "vp9";
}

export interface ProjectSettings {
  cursor: CursorSettings;
  background: Background;
  export: ExportSettings;
}

// --- Корневой объект ---

export interface Project {
  schemaVersion: typeof PROJECT_SCHEMA_VERSION;
  id: string;
  name: string;
  /** Unix timestamp (мс) создания проекта. */
  createdAt: number;
  /** Путь к сырому видеофайлу относительно папки проекта. */
  videoPath: string;
  /** Путь к прокси-видео для монтажа (опционально). */
  proxyVideoPath?: string;
  /** Путь к events.json относительно папки проекта. */
  eventsPath: string;
  /** Длительность записи (мс). */
  durationMs: number;
  videoWidth: number;
  videoHeight: number;
  timeline: Timeline;
  settings: ProjectSettings;
}

// --- Фабрики / дефолты ---

export function defaultCursorSettings(): CursorSettings {
  return { size: 1.0, color: "#FFFFFF", smoothingFactor: 0.8 };
}

export function defaultBackground(): Background {
  return { type: "solid", color: "#1a1a2e" };
}

export function defaultExportSettings(): ExportSettings {
  return { width: 1920, height: 1080, fps: 30, codec: "h264" };
}

export function defaultProjectSettings(): ProjectSettings {
  return {
    cursor: defaultCursorSettings(),
    background: defaultBackground(),
    export: defaultExportSettings(),
  };
}

export function createProject(
  id: string,
  name: string,
  videoPath: string,
  eventsPath: string,
  videoWidth: number,
  videoHeight: number,
  durationMs: number
): Project {
  return {
    schemaVersion: PROJECT_SCHEMA_VERSION,
    id,
    name,
    createdAt: Date.now(),
    videoPath,
    eventsPath,
    durationMs,
    videoWidth,
    videoHeight,
    timeline: { zoomSegments: [] },
    settings: defaultProjectSettings(),
  };
}
