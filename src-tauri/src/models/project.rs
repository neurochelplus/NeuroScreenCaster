//! Схема проекта (project.json).
//! schemaVersion: 1

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Прямоугольная область в нормализованных координатах (0.0–1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

fn default_normalized_rect() -> NormalizedRect {
    NormalizedRect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    }
}

/// Easing-функция для анимации зума.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZoomEasing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Default for ZoomEasing {
    fn default() -> Self {
        ZoomEasing::EaseInOut
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanKeyframe {
    pub ts: u64,
    pub offset_x: f64,
    pub offset_y: f64,
}

/// Один зум-сегмент на таймлайне.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoomSegment {
    pub id: String,
    /// Начало сегмента (мс от начала записи).
    pub start_ts: u64,
    /// Конец сегмента (мс).
    pub end_ts: u64,
    /// Целевая область просмотра (нормализованные координаты).
    #[serde(default = "default_normalized_rect", alias = "targetRect")]
    pub initial_rect: NormalizedRect,
    #[serde(default)]
    pub pan_trajectory: Vec<PanKeyframe>,
    #[serde(default)]
    pub easing: ZoomEasing,
    /// true — создан алгоритмом, false — пользователем вручную.
    #[serde(default)]
    pub is_auto: bool,
}

/// Таймлайн проекта.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
    pub zoom_segments: Vec<ZoomSegment>,
}

/// Настройки курсора.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorSettings {
    /// Относительный размер курсора (1.0 = нормальный).
    pub size: f64,
    pub color: String,
    /// 0.0 = нет сглаживания, 1.0 = максимальное.
    pub smoothing_factor: f64,
}

impl Default for CursorSettings {
    fn default() -> Self {
        CursorSettings {
            size: 1.0,
            color: "#FFFFFF".to_string(),
            smoothing_factor: 0.8,
        }
    }
}

/// Тип фона.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Background {
    Solid {
        color: String,
    },
    Gradient {
        from: String,
        to: String,
        direction: String,
    },
}

impl Default for Background {
    fn default() -> Self {
        Background::Solid {
            color: "#1a1a2e".to_string(),
        }
    }
}

/// Настройки экспорта.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: String,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            width: 1920,
            height: 1080,
            fps: 30,
            codec: "h264".to_string(),
        }
    }
}

/// Настройки проекта.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    pub cursor: CursorSettings,
    pub background: Background,
    pub export: ExportSettings,
}

/// Корневой объект project.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    /// Unix timestamp (мс) создания проекта.
    pub created_at: u64,
    /// Путь к сырому видеофайлу относительно папки проекта.
    pub video_path: String,
    /// Путь к файлу событий относительно папки проекта.
    pub events_path: String,
    /// Длительность записи (мс).
    pub duration_ms: u64,
    /// Разрешение захваченного видео.
    pub video_width: u32,
    pub video_height: u32,
    pub timeline: Timeline,
    pub settings: ProjectSettings,
}
