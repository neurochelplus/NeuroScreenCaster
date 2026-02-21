# NeuroScreenCaster

Рекордер экрана для Windows с архитектурой **Metadata-First** и движком **Smart Camera**. Записывайте экран, автоматически генерируйте «умные» зумы на основе телеметрии кликов/скролла, редактируйте таймлайн и экспортируйте готовое MP4 — без повторной записи.

Вдохновлен [Screen Studio](https://www.screen.studio/) и [CANVID](https://www.canvid.com/), реализован как open-source десктоп-приложение.

## Как это работает

1. **Запись** — захват чистого видеопотока (без системного курсора) через Windows Graphics Capture с параллельным логированием телеметрии мыши/клавиатуры и UI-контекста.
2. **Анализ и редактирование** — авто-зум строит сегменты внимания и точки целей камеры (`targetPoints`) с учетом кликов, скролла и контекста UI. В редакторе доступна ручная правка сегментов и параметров курсора.
3. **Smart Camera Preview** — предпросмотр считает камеру покадрово через spring physics (mass/stiffness/damping), поэтому переходы не «снэпятся» на резких изменениях цели.
4. **Экспорт** — FFmpeg рендерит финальный MP4 с тем же spring-подходом камеры и виртуальным курсором, чтобы поведение совпадало с предпросмотром.

## Возможности

- Захват экрана через WGC (Windows Graphics Capture) без системного курсора
- Глобальная телеметрия ввода: движение мыши, клики, скролл, клавиатура
- UI Automation контекст при кликах (имя элемента, bounding rectangle)
- Авто-зум с семантической кластеризацией кликов (контекст + дистанция + временной интервал)
- State-based камера на spring physics вместо keyframes (mass/stiffness/damping)
- Микро-трекинг курсора внутри активного зума («breathing edge panning»)
- Интеграция скролла как смещения target, без отдельной keyframe-анимации pan
- Строгий lock соотношения сторон target-области (без «растягивания» кадра)
- Адаптивные fallback-правила для грубого/неполного UI-контекста (когда bounding rect слишком крупный)
- Сглаживание траектории курсора (Ramer-Douglas-Peucker + Catmull-Rom сплайн)
- Неразрушающий редактор таймлайна с ручной правкой зум-сегментов
- Настраиваемый размер курсора, степень сглаживания и фон (сплошной/градиент)
- Превью композиции в реальном времени с физической моделью камеры
- Экспорт MP4 с индикатором прогресса
- Поддержка High-DPI (масштабирование Windows 100/125/150%)

## Стек технологий

| Слой | Технология |
|------|-----------|
| Десктоп-оболочка | Rust + [Tauri v2](https://v2.tauri.app/) |
| Захват экрана | [windows-capture](https://crates.io/crates/windows-capture) 1.5 (WGC) |
| Хуки ввода | [rdev](https://crates.io/crates/rdev) |
| UI-контекст | [uiautomation](https://crates.io/crates/uiautomation) |
| Фронтенд | React 18 + TypeScript + Vite |
| Композиция видео | FFmpeg filter graph + spring-выражения камеры |
| Кодирование видео | FFmpeg (встроен как sidecar) |

## Системные требования

- **Windows 10/11** (WGC требует Windows 10 1903+)
- **Node.js** 18+
- **Rust** stable (через [rustup](https://rustup.rs/))
- **Visual Studio Build Tools** с компонентом "C++ build tools" (необходим для компиляции Rust и нативных зависимостей)
- **WebView2** (предустановлен в Windows 11; для Windows 10 — [скачать](https://developer.microsoft.com/en-us/microsoft-edge/webview2/))
- **FFmpeg** — бинарник необходимо поместить в `src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe` (файл не входит в репозиторий, скачайте с [ffmpeg.org](https://ffmpeg.org/download.html) или [gyan.dev](https://www.gyan.dev/ffmpeg/builds/))

## Установка и запуск

```bash
# Клонировать репозиторий
git clone https://github.com/your-username/NeuroScreenCaster.git
cd NeuroScreenCaster

# Установить фронтенд-зависимости
npm install

# Скачать FFmpeg и поместить в нужную директорию
# (пример для PowerShell)
# curl -L -o src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe https://...

# Запуск в режиме разработки (Vite dev server + Tauri окно)
npx @tauri-apps/cli dev
```

> Rust-зависимости (crates) скачиваются автоматически при первой сборке через `cargo`.

### Сборка релиза

```bash
# Собрать релизный бинарник (результат в src-tauri/target/release/bundle/)
npx @tauri-apps/cli build
```

Установщик/исполняемый файл будет в `src-tauri/target/release/bundle/`.

## Структура проекта

```
NeuroScreenCaster/
├── src/                        # React-фронтенд
│   ├── screens/                # Экраны: Record, Edit, Export
│   ├── components/             # UI-компоненты + модули алгоритмов
│   └── types/                  # TypeScript-контракты (events.ts, project.ts)
├── src-tauri/                  # Rust-бэкенд (Tauri)
│   ├── src/
│   │   ├── capture/            # WGC-рекордер + пайп в FFmpeg
│   │   ├── telemetry/          # rdev-хуки + UI Automation контекст
│   │   ├── commands/           # Tauri IPC обработчики команд
│   │   ├── models/             # Rust-структуры данных (events, project)
│   │   └── algorithm/          # Авто-зум + сглаживание курсора
│   └── binaries/               # FFmpeg sidecar
├── PLAN.md                     # План разработки (этапы 0–7)
└── package.json
```

## Формат записи

Каждая запись создает папку в `{Видео}/NeuroScreenCaster/{recording_id}/`:

| Файл | Описание |
|------|----------|
| `raw.mp4` | H.264 30fps захват экрана без курсора |
| `project.json` | Метаданные проекта, таймлайн, параметры рендера |
| `events.json` | Телеметрия ввода (мышь, клавиатура, UI-контекст) |

## Использование

1. **Запись** — выберите монитор и нажмите Start. Приложение записывает экран и события ввода одновременно. Нажмите Stop для завершения.
2. **Редактирование** — откройте запись в редакторе. Зум-сегменты генерируются автоматически из кликов. Перетаскивайте сегменты на таймлайне, настраивайте размер курсора, сглаживание и стиль фона.
3. **Экспорт** — задайте параметры и нажмите Export. Финальный MP4 рендерится с умной spring-камерой и виртуальным курсором.

## Скрипты

| Команда | Описание |
|---------|----------|
| `npm run dev` | Запуск Vite dev-сервера |
| `npm run build` | Сборка фронтенда (TypeScript + Vite) |
| `npx @tauri-apps/cli dev` | Полная dev-среда (фронтенд + Tauri) |
| `npx @tauri-apps/cli build` | Сборка релиза |
| `npm run qa:smoke` | Smoke QA-проверки |

## Лицензия

MIT
