use crate::models::events::InputEvent;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPoint {
    pub ts: u64,
    pub x: f64,
    pub y: f64,
    pub is_click: bool,
}

pub fn collect_cursor_points(events: &[InputEvent]) -> Vec<CursorPoint> {
    let mut points = events
        .iter()
        .filter_map(|event| match event {
            InputEvent::Move { ts, x, y } => Some(CursorPoint {
                ts: *ts,
                x: *x,
                y: *y,
                is_click: false,
            }),
            InputEvent::Click { ts, x, y, .. } => Some(CursorPoint {
                ts: *ts,
                x: *x,
                y: *y,
                is_click: true,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    points.sort_by_key(|point| point.ts);
    dedupe_points(points)
}

pub fn smooth_cursor_path(events: &[InputEvent], smoothing_factor: f64) -> Vec<CursorPoint> {
    let points = collect_cursor_points(events);
    smooth_cursor_points(&points, smoothing_factor)
}

pub fn smooth_cursor_points(points: &[CursorPoint], smoothing_factor: f64) -> Vec<CursorPoint> {
    if points.len() < 2 {
        return points.to_vec();
    }

    let factor = smoothing_factor.clamp(0.0, 1.0);
    if factor <= f64::EPSILON {
        return points.to_vec();
    }

    let filtered = adaptive_holt_filter(points, factor);
    let samples_per_segment = ((2.0 + factor * 6.0).round() as usize).max(2);
    let interpolated = bezier_interpolate(&filtered, samples_per_segment);
    snap_click_points(interpolated, points)
}

/// Kept for compatibility with previous API.
/// RDP-based simplification is intentionally disabled to preserve hand micro-dynamics.
pub fn simplify_with_click_anchors(points: &[CursorPoint], _epsilon: f64) -> Vec<CursorPoint> {
    dedupe_points(points.to_vec())
}

/// Kept for compatibility with previous API.
/// Internally switched from Catmull-Rom sampling to cubic Bezier interpolation.
pub fn catmull_rom_interpolate(
    points: &[CursorPoint],
    samples_per_segment: usize,
) -> Vec<CursorPoint> {
    bezier_interpolate(points, samples_per_segment)
}

fn adaptive_holt_filter(points: &[CursorPoint], smoothing_factor: f64) -> Vec<CursorPoint> {
    let alpha_base = (0.72 - smoothing_factor * 0.58).clamp(0.08, 0.95);
    let beta_base = (0.34 - smoothing_factor * 0.25).clamp(0.03, 0.9);

    let mut output = Vec::with_capacity(points.len());
    let first = points[0];
    output.push(first);

    let mut level_x = first.x;
    let mut level_y = first.y;
    let mut trend_x = 0.0;
    let mut trend_y = 0.0;

    for idx in 1..points.len() {
        let point = points[idx];
        let prev_input = points[idx - 1];

        if point.is_click {
            level_x = point.x;
            level_y = point.y;
            trend_x = 0.0;
            trend_y = 0.0;
            output.push(point);
            continue;
        }

        let dt_ms = point.ts.saturating_sub(prev_input.ts).max(1) as f64;
        let dt_scale = (dt_ms / 16.0).clamp(0.25, 4.0);
        let speed = (point.x - prev_input.x).hypot(point.y - prev_input.y) / dt_ms;
        let speed_boost = (speed / 3.0).clamp(0.0, 1.0);

        // Faster hand movement should reduce lag; slower movement should smooth more.
        let alpha = (alpha_base + (1.0 - alpha_base) * speed_boost * 0.68).clamp(0.05, 0.98);
        let beta = (beta_base + (1.0 - beta_base) * speed_boost * 0.42).clamp(0.02, 0.95);

        let prev_level_x = level_x;
        let prev_level_y = level_y;

        let prediction_x = level_x + trend_x * dt_scale;
        let prediction_y = level_y + trend_y * dt_scale;
        level_x = alpha * point.x + (1.0 - alpha) * prediction_x;
        level_y = alpha * point.y + (1.0 - alpha) * prediction_y;
        trend_x = beta * (level_x - prev_level_x) + (1.0 - beta) * trend_x;
        trend_y = beta * (level_y - prev_level_y) + (1.0 - beta) * trend_y;

        output.push(CursorPoint {
            ts: point.ts,
            x: level_x,
            y: level_y,
            is_click: false,
        });
    }

    output
}

fn bezier_interpolate(points: &[CursorPoint], samples_per_segment: usize) -> Vec<CursorPoint> {
    if points.len() < 2 {
        return points.to_vec();
    }

    let samples = samples_per_segment.max(2);
    let mut result = Vec::with_capacity((points.len() - 1) * samples + 1);

    for idx in 0..(points.len() - 1) {
        let p0 = if idx == 0 {
            points[idx]
        } else {
            points[idx - 1]
        };
        let p1 = points[idx];
        let p2 = points[idx + 1];
        let p3 = if idx + 2 < points.len() {
            points[idx + 2]
        } else {
            points[idx + 1]
        };

        let c1x = p1.x + (p2.x - p0.x) / 6.0;
        let c1y = p1.y + (p2.y - p0.y) / 6.0;
        let c2x = p2.x - (p3.x - p1.x) / 6.0;
        let c2y = p2.y - (p3.y - p1.y) / 6.0;

        for step in 0..=samples {
            if idx > 0 && step == 0 {
                continue;
            }

            let t = step as f64 / samples as f64;
            let omt = 1.0 - t;
            let x = omt.powi(3) * p1.x
                + 3.0 * omt.powi(2) * t * c1x
                + 3.0 * omt * t.powi(2) * c2x
                + t.powi(3) * p2.x;
            let y = omt.powi(3) * p1.y
                + 3.0 * omt.powi(2) * t * c1y
                + 3.0 * omt * t.powi(2) * c2y
                + t.powi(3) * p2.y;

            let ts = lerp_ts(p1.ts, p2.ts, t);
            let is_click = (step == 0 && p1.is_click) || (step == samples && p2.is_click);

            result.push(CursorPoint { ts, x, y, is_click });
        }
    }

    result
}

fn lerp_ts(start: u64, end: u64, t: f64) -> u64 {
    let start_f = start as f64;
    let end_f = end as f64;
    (start_f + (end_f - start_f) * t).round() as u64
}

fn snap_click_points(mut points: Vec<CursorPoint>, reference: &[CursorPoint]) -> Vec<CursorPoint> {
    for click_point in reference.iter().copied().filter(|point| point.is_click) {
        if let Some(existing) = points.iter_mut().find(|point| point.ts == click_point.ts) {
            *existing = click_point;
        } else {
            points.push(click_point);
        }
    }

    dedupe_points(points)
}

fn dedupe_points(mut points: Vec<CursorPoint>) -> Vec<CursorPoint> {
    if points.is_empty() {
        return points;
    }

    points.sort_by_key(|point| point.ts);
    let mut deduped: Vec<CursorPoint> = Vec::with_capacity(points.len());

    for point in points {
        if let Some(last) = deduped.last_mut() {
            if point.ts == last.ts {
                if point.is_click || !last.is_click {
                    *last = point;
                }
                continue;
            }
        }
        deduped.push(point);
    }

    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::events::{InputEvent, MouseButton};

    fn move_event(ts: u64, x: f64, y: f64) -> InputEvent {
        InputEvent::Move { ts, x, y }
    }

    fn click_event(ts: u64, x: f64, y: f64) -> InputEvent {
        InputEvent::Click {
            ts,
            x,
            y,
            button: MouseButton::Left,
            ui_context: None,
        }
    }

    #[test]
    fn simplify_keeps_click_points() {
        let points = vec![
            CursorPoint {
                ts: 0,
                x: 0.0,
                y: 0.0,
                is_click: false,
            },
            CursorPoint {
                ts: 10,
                x: 5.0,
                y: 0.2,
                is_click: false,
            },
            CursorPoint {
                ts: 20,
                x: 10.0,
                y: 0.0,
                is_click: true,
            },
            CursorPoint {
                ts: 30,
                x: 15.0,
                y: 0.3,
                is_click: false,
            },
        ];

        let simplified = simplify_with_click_anchors(&points, 5.0);
        assert!(simplified
            .iter()
            .any(|point| point.ts == 20 && point.is_click));
    }

    #[test]
    fn interpolation_keeps_control_points_at_segment_edges() {
        let points = vec![
            CursorPoint {
                ts: 0,
                x: 0.0,
                y: 0.0,
                is_click: false,
            },
            CursorPoint {
                ts: 100,
                x: 50.0,
                y: 20.0,
                is_click: true,
            },
            CursorPoint {
                ts: 200,
                x: 100.0,
                y: 0.0,
                is_click: false,
            },
        ];

        let smoothed = catmull_rom_interpolate(&points, 4);
        assert!(smoothed.iter().any(|point| {
            point.ts == 100
                && (point.x - 50.0).abs() < 0.0001
                && (point.y - 20.0).abs() < 0.0001
                && point.is_click
        }));
    }

    #[test]
    fn smoothing_factor_zero_returns_raw_points() {
        let events = vec![
            move_event(0, 0.0, 0.0),
            move_event(10, 10.0, 5.0),
            click_event(20, 20.0, 10.0),
        ];

        let points = collect_cursor_points(&events);
        let smoothed = smooth_cursor_path(&events, 0.0);
        assert_eq!(smoothed, points);
    }

    #[test]
    fn smoothing_preserves_exact_click_coordinates() {
        let events = vec![
            move_event(0, 10.0, 10.0),
            move_event(20, 30.0, 20.0),
            click_event(40, 50.0, 40.0),
            move_event(60, 80.0, 50.0),
        ];

        let smoothed = smooth_cursor_path(&events, 1.0);
        let click_point = smoothed
            .iter()
            .find(|point| point.ts == 40)
            .expect("missing click point");

        assert!((click_point.x - 50.0).abs() < 0.0001);
        assert!((click_point.y - 40.0).abs() < 0.0001);
        assert!(click_point.is_click);
    }
}
