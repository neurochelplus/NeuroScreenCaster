use std::collections::BTreeSet;

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

    let epsilon = 0.25 + factor * 10.0;
    let simplified = simplify_with_click_anchors(points, epsilon);
    let samples_per_segment = ((2.0 + factor * 6.0).round() as usize).max(2);
    let interpolated = catmull_rom_interpolate(&simplified, samples_per_segment);
    snap_click_points(interpolated, &simplified)
}

pub fn simplify_with_click_anchors(points: &[CursorPoint], epsilon: f64) -> Vec<CursorPoint> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let mut anchors = BTreeSet::new();
    anchors.insert(0);
    anchors.insert(points.len() - 1);
    for (idx, point) in points.iter().enumerate() {
        if point.is_click {
            anchors.insert(idx);
        }
    }

    let anchor_indices = anchors.into_iter().collect::<Vec<_>>();
    let mut keep = BTreeSet::new();
    for idx in &anchor_indices {
        keep.insert(*idx);
    }

    for window in anchor_indices.windows(2) {
        rdp_between(points, window[0], window[1], epsilon, &mut keep);
    }

    keep.into_iter().map(|idx| points[idx]).collect()
}

pub fn catmull_rom_interpolate(
    points: &[CursorPoint],
    samples_per_segment: usize,
) -> Vec<CursorPoint> {
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

        for step in 0..=samples {
            if idx > 0 && step == 0 {
                continue;
            }

            let t = step as f64 / samples as f64;
            let x = catmull_value(p0.x, p1.x, p2.x, p3.x, t);
            let y = catmull_value(p0.y, p1.y, p2.y, p3.y, t);
            let ts = lerp_ts(p1.ts, p2.ts, t);
            let is_click = (step == 0 && p1.is_click) || (step == samples && p2.is_click);

            result.push(CursorPoint { ts, x, y, is_click });
        }
    }

    result
}

fn rdp_between(
    points: &[CursorPoint],
    start: usize,
    end: usize,
    epsilon: f64,
    keep: &mut BTreeSet<usize>,
) {
    if end <= start + 1 {
        keep.insert(start);
        keep.insert(end);
        return;
    }

    let mut farthest_idx = start;
    let mut max_distance = 0.0;
    for idx in (start + 1)..end {
        let distance = perpendicular_distance(points[idx], points[start], points[end]);
        if distance > max_distance {
            max_distance = distance;
            farthest_idx = idx;
        }
    }

    if max_distance > epsilon {
        rdp_between(points, start, farthest_idx, epsilon, keep);
        rdp_between(points, farthest_idx, end, epsilon, keep);
        return;
    }

    keep.insert(start);
    keep.insert(end);
}

fn perpendicular_distance(
    point: CursorPoint,
    line_start: CursorPoint,
    line_end: CursorPoint,
) -> f64 {
    let dx = line_end.x - line_start.x;
    let dy = line_end.y - line_start.y;
    if dx.abs() < f64::EPSILON && dy.abs() < f64::EPSILON {
        return (point.x - line_start.x).hypot(point.y - line_start.y);
    }

    let length_sq = dx * dx + dy * dy;
    let t = (((point.x - line_start.x) * dx + (point.y - line_start.y) * dy) / length_sq)
        .clamp(0.0, 1.0);
    let projection_x = line_start.x + t * dx;
    let projection_y = line_start.y + t * dy;
    (point.x - projection_x).hypot(point.y - projection_y)
}

fn catmull_value(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;

    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
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
            CursorPoint {
                ts: 40,
                x: 20.0,
                y: 0.0,
                is_click: false,
            },
        ];

        let simplified = simplify_with_click_anchors(&points, 5.0);
        assert!(simplified
            .iter()
            .any(|point| point.ts == 20 && point.is_click));
    }

    #[test]
    fn catmull_rom_keeps_control_points_at_segment_edges() {
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
