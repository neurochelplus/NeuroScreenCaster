//! Схема событий телеметрии (events.json).
//! schemaVersion: 1

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Ограничивающий прямоугольник UI-элемента в экранных координатах.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundingRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Контекст UI-элемента, полученный через UI Automation при клике.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContext {
    pub app_name: Option<String>,
    pub control_name: Option<String>,
    pub bounding_rect: Option<BoundingRect>,
}

/// Кнопка мыши.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Направление скролла.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollDelta {
    pub dx: f64,
    pub dy: f64,
}

/// Тип события ввода.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputEvent {
    /// Движение мыши.
    Move {
        /// Миллисекунды от начала записи.
        ts: u64,
        x: f64,
        y: f64,
    },
    /// Нажатие кнопки мыши.
    Click {
        ts: u64,
        x: f64,
        y: f64,
        button: MouseButton,
        /// Контекст UI-элемента (заполняется асинхронно).
        #[serde(rename = "uiContext", alias = "ui_context")]
        ui_context: Option<UiContext>,
    },
    /// Отпускание кнопки мыши.
    MouseUp {
        ts: u64,
        x: f64,
        y: f64,
        button: MouseButton,
    },
    /// Прокрутка колеса мыши.
    Scroll {
        ts: u64,
        x: f64,
        y: f64,
        delta: ScrollDelta,
    },
    /// Нажатие клавиши.
    KeyDown {
        ts: u64,
        #[serde(rename = "keyCode", alias = "key_code")]
        key_code: String,
    },
    /// Отпускание клавиши.
    KeyUp {
        ts: u64,
        #[serde(rename = "keyCode", alias = "key_code")]
        key_code: String,
    },
}

impl InputEvent {
    /// Возвращает временную метку события.
    pub fn ts(&self) -> u64 {
        match self {
            InputEvent::Move { ts, .. } => *ts,
            InputEvent::Click { ts, .. } => *ts,
            InputEvent::MouseUp { ts, .. } => *ts,
            InputEvent::Scroll { ts, .. } => *ts,
            InputEvent::KeyDown { ts, .. } => *ts,
            InputEvent::KeyUp { ts, .. } => *ts,
        }
    }
}

/// Корневой контейнер файла events.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsFile {
    pub schema_version: u32,
    /// UUID записи — совпадает с project.json.
    pub recording_id: String,
    /// Unix timestamp (мс) старта записи — точка синхронизации.
    pub start_time_ms: u64,
    /// Разрешение экрана на момент записи.
    pub screen_width: u32,
    pub screen_height: u32,
    /// DPI scale (например 1.25 для 125%).
    pub scale_factor: f64,
    pub events: Vec<InputEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_click_event_with_camel_case_ui_context() {
        let event = InputEvent::Click {
            ts: 123,
            x: 10.0,
            y: 20.0,
            button: MouseButton::Left,
            ui_context: Some(UiContext {
                app_name: Some("App".to_string()),
                control_name: Some("Button".to_string()),
                bounding_rect: Some(BoundingRect {
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4,
                }),
            }),
        };

        let json = serde_json::to_string(&event).expect("serialize click");
        assert!(json.contains("\"uiContext\""));
        assert!(!json.contains("\"ui_context\""));
    }

    #[test]
    fn serializes_key_event_with_camel_case_key_code() {
        let event = InputEvent::KeyDown {
            ts: 100,
            key_code: "KeyA".to_string(),
        };

        let json = serde_json::to_string(&event).expect("serialize keyDown");
        assert!(json.contains("\"keyCode\""));
        assert!(!json.contains("\"key_code\""));
    }

    #[test]
    fn accepts_legacy_snake_case_fields_during_deserialization() {
        let click_legacy = r#"{
            "type":"click",
            "ts":1,
            "x":100.0,
            "y":200.0,
            "button":"left",
            "ui_context":null
        }"#;

        let key_legacy = r#"{
            "type":"keyDown",
            "ts":2,
            "key_code":"KeyB"
        }"#;

        let click_event: InputEvent =
            serde_json::from_str(click_legacy).expect("deserialize legacy click");
        let key_event: InputEvent =
            serde_json::from_str(key_legacy).expect("deserialize legacy keyDown");

        match click_event {
            InputEvent::Click { ui_context, .. } => assert!(ui_context.is_none()),
            _ => panic!("expected click event"),
        }

        match key_event {
            InputEvent::KeyDown { key_code, .. } => assert_eq!(key_code, "KeyB"),
            _ => panic!("expected keyDown event"),
        }
    }
}
