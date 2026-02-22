//! Глобальный логгер ввода (мышь + клавиатура) на основе rdev.
//!
//! Архитектура:
//!   1. `spawn_rdev_thread` запускает один поток (`nsc-rdev-hook`) на всё время жизни приложения.
//!      Поток вызывает `rdev::listen` и пересылает сырые события через `SyncSender`.
//!   2. При вызове `start_session` создаётся новый канал + поток-процессор (`nsc-telemetry-proc`).
//!      Процессор обогащает Click-события UI-контекстом (через uiautomation) и накапливает их.
//!   3. `stop_session` отправляет `RawInput::Stop` в процессор и сбрасывает канал.
//!      Вызывающий ждёт JoinHandle процессора и получает итоговый `Vec<InputEvent>`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use crate::models::events::{InputEvent, MouseButton, ScrollDelta};

// ─── Внутренние типы ─────────────────────────────────────────────────────────

/// Сырые данные одного события ввода, передаваемые из rdev-потока в процессор.
pub enum RawInput {
    Move {
        ts_abs: u64,
        x: f64,
        y: f64,
    },
    Click {
        ts_abs: u64,
        x: f64,
        y: f64,
        button: rdev::Button,
    },
    MouseUp {
        ts_abs: u64,
        x: f64,
        y: f64,
        button: rdev::Button,
    },
    Scroll {
        ts_abs: u64,
        x: f64,
        y: f64,
        delta_x: i64,
        delta_y: i64,
    },
    KeyDown {
        ts_abs: u64,
        key: rdev::Key,
    },
    KeyUp {
        ts_abs: u64,
        key: rdev::Key,
    },
    /// Сигнал завершения: процессор выходит из цикла и возвращает накопленные события.
    Stop,
}

// ─── Разделяемое глобальное состояние ────────────────────────────────────────

/// Состояние, разделяемое между rdev-потоком и IPC-командами.
pub struct TelemetryGlobal {
    /// Канал в текущий процессор сессии; `None` — запись не идёт.
    pub current_tx: Mutex<Option<SyncSender<RawInput>>>,
    /// Последняя известная позиция мыши.
    /// rdev не передаёт координаты в Button/Wheel-событиях — храним отдельно.
    pub last_pos: Mutex<(f64, f64)>,
    /// True when recording is paused and incoming events must be ignored.
    pub is_paused: AtomicBool,
    /// Last observed state of Ctrl modifier from global keyboard hook.
    pub is_ctrl_pressed: AtomicBool,
}

impl TelemetryGlobal {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            current_tx: Mutex::new(None),
            last_pos: Mutex::new((0.0, 0.0)),
            is_paused: AtomicBool::new(false),
            is_ctrl_pressed: AtomicBool::new(false),
        })
    }
}

/// Tauri managed state, оборачивающий `Arc<TelemetryGlobal>`.
pub struct TelemetryState(pub Arc<TelemetryGlobal>);

// ─── rdev-поток ───────────────────────────────────────────────────────────────

/// Запускает один фоновый поток с глобальными хуками ввода.
/// Вызывается ОДИН РАЗ при старте приложения.
pub fn spawn_rdev_thread(global: Arc<TelemetryGlobal>) {
    std::thread::Builder::new()
        .name("nsc-rdev-hook".to_string())
        .spawn(move || {
            if let Err(e) = rdev::listen(move |event| {
                handle_rdev_event(&global, event);
            }) {
                log::error!("rdev::listen error: {e:?}");
            }
        })
        .expect("Failed to spawn rdev thread");
}

/// Обрабатывает одно событие из rdev: при активной сессии отправляет его в процессор.
fn handle_rdev_event(global: &Arc<TelemetryGlobal>, event: rdev::Event) {
    match &event.event_type {
        rdev::EventType::KeyPress(key) if is_ctrl_key(*key) => {
            global.is_ctrl_pressed.store(true, Ordering::Relaxed);
        }
        rdev::EventType::KeyRelease(key) if is_ctrl_key(*key) => {
            global.is_ctrl_pressed.store(false, Ordering::Relaxed);
        }
        _ => {}
    }

    if global.is_paused.load(Ordering::Relaxed) {
        return;
    }

    let ts_abs = event
        .time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Клонируем Sender, пока держим блокировку, и сразу отпускаем.
    let tx = {
        let guard = global.current_tx.lock().unwrap();
        match guard.as_ref() {
            Some(tx) => tx.clone(),
            None => return, // Нет активной сессии — игнорируем.
        }
    };

    match event.event_type {
        rdev::EventType::MouseMove { x, y } => {
            *global.last_pos.lock().unwrap() = (x, y);
            tx.send(RawInput::Move { ts_abs, x, y }).ok();
        }
        rdev::EventType::ButtonPress(button) => {
            let (x, y) = *global.last_pos.lock().unwrap();
            tx.send(RawInput::Click {
                ts_abs,
                x,
                y,
                button,
            })
            .ok();
        }
        rdev::EventType::ButtonRelease(button) => {
            let (x, y) = *global.last_pos.lock().unwrap();
            tx.send(RawInput::MouseUp {
                ts_abs,
                x,
                y,
                button,
            })
            .ok();
        }
        rdev::EventType::Wheel { delta_x, delta_y } => {
            let (x, y) = *global.last_pos.lock().unwrap();
            tx.send(RawInput::Scroll {
                ts_abs,
                x,
                y,
                delta_x,
                delta_y,
            })
            .ok();
        }
        rdev::EventType::KeyPress(key) => {
            tx.send(RawInput::KeyDown { ts_abs, key }).ok();
        }
        rdev::EventType::KeyRelease(key) => {
            tx.send(RawInput::KeyUp { ts_abs, key }).ok();
        }
    }
}

// ─── Управление сессией ───────────────────────────────────────────────────────

/// Начинает новую сессию телеметрии.
///
/// Создаёт канал и запускает поток-процессор. Возвращает `JoinHandle`, при
/// `.join()` которого получаем `Vec<InputEvent>` — все накопленные события.
pub fn start_session(
    global: &Arc<TelemetryGlobal>,
    start_ms: u64,
) -> std::thread::JoinHandle<Vec<InputEvent>> {
    global.is_paused.store(false, Ordering::Relaxed);
    let (tx, rx) = sync_channel::<RawInput>(8192);
    *global.current_tx.lock().unwrap() = Some(tx);

    std::thread::Builder::new()
        .name("nsc-telemetry-proc".to_string())
        .spawn(move || {
            let mut events = Vec::<InputEvent>::new();

            for raw in rx {
                match raw {
                    RawInput::Stop => break,

                    RawInput::Move { ts_abs, x, y } => {
                        events.push(InputEvent::Move {
                            ts: ts_abs.saturating_sub(start_ms),
                            x,
                            y,
                        });
                    }

                    RawInput::Click {
                        ts_abs,
                        x,
                        y,
                        button,
                    } => {
                        let ui_context = crate::telemetry::ui_context::get_ui_context(x, y);
                        events.push(InputEvent::Click {
                            ts: ts_abs.saturating_sub(start_ms),
                            x,
                            y,
                            button: rdev_button(button),
                            ui_context,
                        });
                    }

                    RawInput::MouseUp {
                        ts_abs,
                        x,
                        y,
                        button,
                    } => {
                        events.push(InputEvent::MouseUp {
                            ts: ts_abs.saturating_sub(start_ms),
                            x,
                            y,
                            button: rdev_button(button),
                        });
                    }

                    RawInput::Scroll {
                        ts_abs,
                        x,
                        y,
                        delta_x,
                        delta_y,
                    } => {
                        events.push(InputEvent::Scroll {
                            ts: ts_abs.saturating_sub(start_ms),
                            x,
                            y,
                            delta: ScrollDelta {
                                dx: delta_x as f64,
                                dy: delta_y as f64,
                            },
                        });
                    }

                    RawInput::KeyDown { ts_abs, key } => {
                        events.push(InputEvent::KeyDown {
                            ts: ts_abs.saturating_sub(start_ms),
                            key_code: format!("{key:?}"),
                        });
                    }

                    RawInput::KeyUp { ts_abs, key } => {
                        events.push(InputEvent::KeyUp {
                            ts: ts_abs.saturating_sub(start_ms),
                            key_code: format!("{key:?}"),
                        });
                    }
                }
            }

            events
        })
        .expect("Failed to spawn telemetry processor thread")
}

/// Сигнализирует текущей сессии завершиться: отправляет `Stop` и сбрасывает канал.
/// После этого вызывающий должен дождаться `JoinHandle` процессора.
pub fn stop_session(global: &Arc<TelemetryGlobal>) {
    global.is_paused.store(false, Ordering::Relaxed);
    let tx = global.current_tx.lock().unwrap().take();
    if let Some(tx) = tx {
        tx.send(RawInput::Stop).ok();
    }
}

pub fn set_paused(global: &Arc<TelemetryGlobal>, paused: bool) {
    global.is_paused.store(paused, Ordering::Relaxed);
}

// ─── Вспомогательные функции ──────────────────────────────────────────────────

/// Переводит `rdev::Button` в модельный `MouseButton`.
fn rdev_button(button: rdev::Button) -> MouseButton {
    match button {
        rdev::Button::Right => MouseButton::Right,
        rdev::Button::Middle => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn is_ctrl_key(key: rdev::Key) -> bool {
    matches!(key, rdev::Key::ControlLeft | rdev::Key::ControlRight)
}
