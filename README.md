# NeuroScreenCaster

NeuroScreenCaster — desktop-приложение для записи экрана на Windows с архитектурой **Metadata-First**:

- видео пишется отдельно от системного курсора;
- события ввода (мышь/клавиатура/UI-context) сохраняются в `events.json`;
- автозумы строятся постфактум через `SmartCameraEngine`;
- результат редактируется на таймлайне и экспортируется в MP4.

Подробная спецификация логики автозума: `GUIDE.md`.

## Актуальное состояние (текущая версия)

### 1. Запись

- Экран записи показывает live preview выбранного монитора.
- Перед стартом доступны настройки:
  - Auto Zoom Trigger: `1 click` (по умолчанию), `2 clicks in 3 seconds`, `Ctrl + click`;
  - Quality: `Low` / `Balanced` / `High`;
  - FPS: `30` или `60` (по умолчанию `60`).
- Во время записи приложение можно ставить на `Pause`/`Resume`.
- Внизу экрана Windows показывается отдельная плашка управления записью:
  - таймер;
  - кнопки `Pause/Resume` и `Stop`;
  - прозрачный фон, без лишней прямоугольной рамки;
  - при зажатом `Ctrl` плашка скрывается.

### 2. Автозумы (Smart Camera Engine)

Основной модуль: `src-tauri/src/algorithm/camera_engine.rs`.

- Состояния камеры:
  - `FreeRoam`;
  - `LockedFocus`.
- Семантический фокус:
  - если есть `uiContext.boundingRect`, фокус строится по нему (+ padding);
  - если нет bounding box, используется fallback `2.0x` по точке клика.
- Жесткое ограничение зума:
  - `max_zoom_limit = 2.0x`.
- Предвосхищение (pre-roll):
  - до `400ms` до клика, если скорость курсора падает ниже порога.
- Anti-spam:
  - минимальный интервал между стартами новых zoom-переходов: `2s`.
- Выход из lock при отсутствии новых кликов:
  - окно неактивности сокращено до ~`2s`.
- Safe-zone / containment:
  - если новый target уже в безопасной зоне текущего viewport, ретаргет не выполняется.

### 3. Редактор (Edit)

Экран: `src/screens/Edit.tsx`.

- Редактирование сегментов зума на таймлайне.
- Переключение режима сегмента:
  - `Locked` (`fixed`);
  - `Follow cursor` (`follow-cursor`).
- Предпросмотр камеры на spring-треке.
- Курсор в preview:
  - единый векторный стиль (черный, белая обводка);
  - масштабируется вместе с зумом камеры;
  - есть click pulse (сжатие/возврат) с якорем по кончику курсора.

### 4. Экспорт

Команда: `src-tauri/src/commands/export.rs`.

- Камера рендерится через spring-динамику.
- Курсор в экспорте совпадает со стилем preview:
  - векторный черный курсор с белой обводкой;
  - масштаб от текущего зума;
  - click pulse (с якорем в кончике).
- Для стабильности длинных проектов используется fallback-сэмплирование трека камеры (с повышенной плотностью точек), чтобы снизить рывки.

## Технологии

| Слой | Технология |
|---|---|
| Desktop shell | Rust + Tauri v2 |
| Screen capture | `windows-capture` (WGC) |
| Input telemetry | `rdev` |
| UI context | `uiautomation` |
| Frontend | React 18 + TypeScript + Vite |
| Export | FFmpeg (filter graph + spring camera) |

## Системные требования

- Windows 10/11 (WGC: Windows 10 1903+)
- Node.js 18+
- Rust stable (`rustup`)
- Visual Studio Build Tools (C++ build tools)
- WebView2
- FFmpeg sidecar: `src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe`

## Установка и запуск

```bash
git clone https://github.com/neurochelplus/NeuroScreenCaster.git
cd NeuroScreenCaster
npm install
npx @tauri-apps/cli dev
```

Сборка релиза:

```bash
npx @tauri-apps/cli build
```

## Структура проекта

```text
NeuroScreenCaster/
├── src/
│   ├── components/
│   │   └── RecordingOverlay.tsx
│   ├── screens/
│   │   ├── Record.tsx
│   │   ├── Edit.tsx
│   │   └── Export.tsx
│   ├── recordingOverlay.ts
│   └── types/
├── src-tauri/
│   ├── src/
│   │   ├── algorithm/
│   │   │   ├── camera_engine.rs
│   │   │   └── cursor_smoothing.rs
│   │   ├── capture/
│   │   │   ├── recorder.rs
│   │   │   └── preview.rs
│   │   ├── commands/
│   │   │   ├── capture.rs
│   │   │   ├── cursor.rs
│   │   │   └── export.rs
│   │   ├── models/
│   │   └── telemetry/
│   └── binaries/
├── GUIDE.md
└── README.md
```

## Скрипты

| Команда | Описание |
|---|---|
| `npm run dev` | Vite dev server |
| `npm run build` | Сборка фронтенда |
| `npx @tauri-apps/cli dev` | Полный dev-режим |
| `npx @tauri-apps/cli build` | Сборка приложения |

## Лицензия

MIT
