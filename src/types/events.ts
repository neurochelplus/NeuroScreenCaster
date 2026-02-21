/**
 * Контракт events.json — schemaVersion: 1
 * Телеметрия ввода, синхронизированная с raw-видео.
 */

export const EVENTS_SCHEMA_VERSION = 1 as const;

export type MouseButton = "left" | "right" | "middle";

export interface BoundingRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface UiContext {
  appName: string | null;
  controlName: string | null;
  boundingRect: BoundingRect | null;
}

export interface ScrollDelta {
  dx: number;
  dy: number;
}

// --- Дискриминированные союзы событий ---

export interface MoveEvent {
  type: "move";
  /** Миллисекунды от начала записи. */
  ts: number;
  x: number;
  y: number;
}

export interface ClickEvent {
  type: "click";
  ts: number;
  x: number;
  y: number;
  button: MouseButton;
  /** Заполняется асинхронно через UI Automation; null — не удалось получить. */
  uiContext: UiContext | null;
}

export interface MouseUpEvent {
  type: "mouseUp";
  ts: number;
  x: number;
  y: number;
  button: MouseButton;
}

export interface ScrollEvent {
  type: "scroll";
  ts: number;
  x: number;
  y: number;
  delta: ScrollDelta;
}

export interface KeyDownEvent {
  type: "keyDown";
  ts: number;
  keyCode: string;
}

export interface KeyUpEvent {
  type: "keyUp";
  ts: number;
  keyCode: string;
}

export type InputEvent =
  | MoveEvent
  | ClickEvent
  | MouseUpEvent
  | ScrollEvent
  | KeyDownEvent
  | KeyUpEvent;

/**
 * Корневой объект файла events.json.
 */
export interface EventsFile {
  schemaVersion: typeof EVENTS_SCHEMA_VERSION;
  /** UUID записи — совпадает с project.json. */
  recordingId: string;
  /** Unix timestamp (мс) старта записи — точка синхронизации с видео. */
  startTimeMs: number;
  screenWidth: number;
  screenHeight: number;
  /** DPI scale монитора (1.0, 1.25, 1.5...). */
  scaleFactor: number;
  events: InputEvent[];
}

// --- Утилиты ---

/** Извлекает все click-события из потока. */
export function getClickEvents(events: InputEvent[]): ClickEvent[] {
  return events.filter((e): e is ClickEvent => e.type === "click");
}

/** Извлекает все move-события из потока. */
export function getMoveEvents(events: InputEvent[]): MoveEvent[] {
  return events.filter((e): e is MoveEvent => e.type === "move");
}
