# NeuroScreenCaster

NeuroScreenCaster — desktop-приложение для записи экрана на Windows с архитектурой **Metadata-First**:

- видео записывается отдельно от курсора,
- телеметрия ввода (мышь/клавиатура/UI-context) сохраняется в `events.json`,
- авто-зумы строятся постфактум через **Smart Camera Engine**,
- результат можно редактировать на таймлайне и экспортировать в MP4.

## Что изменилось в текущей версии

- Старый модуль `auto_zoom.rs` удален.
- Основной движок автозума: `src-tauri/src/algorithm/camera_engine.rs`.
- В `ZoomSegment` добавлены:
  - `mode`: `fixed` | `follow-cursor`
  - `trigger`: `auto-click` | `auto-scroll` | `manual`
- В редакторе можно переключать режим камеры для конкретного сегмента.

Подробная специфика текущего поведения: `GUIDE.md`.

## Автозумы: текущая логика

### 1. Базовый pipeline

1. Во время записи сохраняются:
   - `raw.mp4` (без системного курсора)
   - `events.json` (move/click/scroll/key + UI context)
2. После `Stop` вызывается `build_smart_camera_segments(...)`.
3. Сегменты попадают в `project.json` -> `timeline.zoomSegments`.

### 2. Триггеры

- Активация по кликам: **минимум 2 клика в окне 3 секунды**.
- Быстрые клики группируются: `<= 300ms` между кликами.
- Антиспам: минимум `2s` между стартами новых auto-переходов.
- Pre-roll до клика: до `400ms` при замедлении курсора.

### 3. Фокус и кадрирование

- Если у клика есть `uiContext.boundingRect`, камера фокусируется на нем (с padding).
- Если `boundingRect` нет — fallback на клик с zoom `2.0x`.
- Жесткий clamp зума: максимум `2.5x`.
- Containment test: если новый target полностью внутри safe-zone текущего viewport, ретаргет не выполняется.

### 4. Поведение камеры

Состояния:

- `FreeRoam`
- `LockedFocus`

`LockedFocus`:

- камера залочена на фокус,
- soft-zone в зуме не используется,
- pan только при пробитии hard-edge,
- scroll двигает `Y_target` синхронно с контентом,
- глобальный scroll (`>3s` или `>150%` высоты экрана) выводит камеру обратно в общий контекст.

### 5. Пружина

- Фиксированный шаг интеграции: `8ms`.
- Параметры по умолчанию:
  - `mass=1`
  - `stiffness=170`
  - `damping=26`

## Редактор

Экран `Edit` (`src/screens/Edit.tsx`) поддерживает:

- ручное создание/удаление сегментов,
- изменение позиции и силы зума,
- выбор режима сегмента:
  - `Locked` (`fixed`)
  - `Follow cursor` (`follow-cursor`)
- предпросмотр камеры через spring-track,
- редактирование параметров курсора.

Для `follow-cursor` траектория target points генерируется в редакторе на основе курсора и сохраняется в проект при `Save`.

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
- Visual Studio Build Tools (`C++ build tools`)
- WebView2
- FFmpeg sidecar: `src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe`

## Установка и запуск

```bash
git clone https://github.com/your-username/NeuroScreenCaster.git
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
│   ├── screens/
│   └── types/
├── src-tauri/
│   ├── src/
│   │   ├── algorithm/
│   │   │   ├── camera_engine.rs
│   │   │   └── cursor_smoothing.rs
│   │   ├── capture/
│   │   ├── commands/
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
