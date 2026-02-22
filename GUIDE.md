# GUIDE: Логика автозумов (текущая версия)

Документ описывает фактическое поведение автозумов в текущем коде.

## 1. Где формируются автозумы

- После остановки записи (`stop_recording`) события из `events.json` передаются в:
  - `build_smart_camera_segments(...)`
  - файл: `src-tauri/src/algorithm/camera_engine.rs`
- Сегменты сохраняются в `project.json` в `timeline.zoomSegments`.

## 2. Состояния камеры

Камера работает в двух состояниях:

- `FreeRoam`
- `LockedFocus`

### FreeRoam

- Камера остается в общем контексте (`zoom ~ 1.0`).
- Центр обновляется только при выходе курсора за dead-zone.

### LockedFocus

- Камера фиксируется на целевом фокусе.
- Новые клики могут перестраивать цель (с containment-проверкой safe-zone).
- Scroll сдвигает `Y_target` в lock-состоянии.
- При длительном глобальном скролле или по таймауту lock — возврат в `FreeRoam`.

## 3. Триггеры автозума

В момент записи доступно 3 режима активации:

- `single-click` (по умолчанию)
- `multi-click-window` (2 клика в 3 секунды)
- `ctrl-click`

Настройка передается из `Record` UI в backend:

- `src/screens/Record.tsx`
- `src-tauri/src/commands/capture.rs`

## 4. Семантический фокус и зум

### 4.1 Базовая цель

- Если у клика есть `uiContext.boundingRect`, цель строится по его центру.
- К bounding box добавляется semantic padding.
- Если `boundingRect` отсутствует, используется fallback: зум по точке клика.

### 4.2 Ограничения

- fallback zoom: `2.0x`
- `max_zoom_limit`: `2.0x` (жесткий clamp)

## 5. Временная логика

- `max_lookahead_ms`: до `400ms` для pre-roll при замедлении курсора.
- `min_zoom_interval_ms`: `2000ms` между стартами соседних автопереходов.
- Таймаут отсутствия новых кликов для выхода из lock: около `2s`.

## 6. Плавность движения

Камера интегрируется как spring-модель:

- фиксированный шаг интеграции;
- отдельные оси `x`, `y`, `zoom`;
- параметры пружины хранятся в `SmartCameraConfig`.

## 7. Что видит редактор

В `Edit` (`src/screens/Edit.tsx`) сегмент можно переключать:

- `Locked` (`fixed`)
- `Follow cursor` (`follow-cursor`)

Для `follow-cursor` target points генерируются/обновляются и сохраняются при `Save`.

## 8. Курсор в preview и export

Курсор унифицирован:

- векторный стиль (черный с белой обводкой);
- click pulse (сжатие/возврат) с якорем по кончику;
- масштабируется вместе с зумом камеры как в `Edit` preview, так и в export.

## 9. Основные файлы

- Автозумы и состояния камеры:
  - `src-tauri/src/algorithm/camera_engine.rs`
- Генерация проекта после записи:
  - `src-tauri/src/commands/capture.rs`
- Preview/редактор:
  - `src/screens/Edit.tsx`
- Экспорт:
  - `src-tauri/src/commands/export.rs`
