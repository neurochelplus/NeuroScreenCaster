//! Получение UI-контекста через Windows UI Automation.
//!
//! Функция `get_ui_context` вызывается синхронно из потока-процессора телеметрии
//! при каждом клике. Любые ошибки (COM, таймаут, Protected UI) дают `None`,
//! что считается допустимым fallback-ом.

use crate::models::events::{BoundingRect, UiContext};

/// Возвращает UI-контекст элемента в точке `(x, y)` экранных координат,
/// или `None` при любой ошибке.
pub fn get_ui_context(x: f64, y: f64) -> Option<UiContext> {
    // Оборачиваем в catch_unwind на случай паники внутри COM/UIA.
    std::panic::catch_unwind(|| query_uia(x, y)).ok().flatten()
}

fn query_uia(x: f64, y: f64) -> Option<UiContext> {
    use uiautomation::{types::Point, UIAutomation};

    let auto = UIAutomation::new().ok()?;
    let point = Point::new(x as i32, y as i32);
    let element = auto.element_from_point(point).ok()?;

    let app_name = element
        .get_process_id()
        .ok()
        .map(|pid| format!("pid:{pid}"));
    let control_name = element.get_name().ok().filter(|s| !s.is_empty());

    let bounding_rect = element.get_bounding_rectangle().ok().map(|r| BoundingRect {
        x: r.get_left(),
        y: r.get_top(),
        width: (r.get_right() - r.get_left()).max(0) as u32,
        height: (r.get_bottom() - r.get_top()).max(0) as u32,
    });

    Some(UiContext {
        app_name,
        control_name,
        bounding_rect,
    })
}
