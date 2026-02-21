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

    let resampled = resample_points(points, 120.0);
    if resampled.len() < 2 {
        return resampled;
    }

    let window = (3.0 + (factor * 2.0).round()) as usize;
    let filtered = simple_moving_average_filter(&resampled, window.clamp(3, 5));
    let samples_per_segment = ((2.0 + factor * 6.0).round() as usize).max(2);
    let interpolated = catmull_rom_interpolate_impl(&filtered, samples_per_segment);
    snap_click_points(interpolated, &resampled)
}

/// Kept for compatibility with previous API.
/// RDP-based simplification is intentionally disabled to preserve hand micro-dynamics.
pub fn simplify_with_click_anchors(points: &[CursorPoint], _epsilon: f64) -> Vec<CursorPoint> {
    dedupe_points(points.to_vec())
}

/// Kept for compatibility with previous API.
/// Uses Catmull-Rom sampling that passes through input control points.
pub fn catmull_rom_interpolate(
    points: &[CursorPoint],
    samples_per_segment: usize,
) -> Vec<CursorPoint> {
    catmull_rom_interpolate_impl(points, samples_per_segment)
}

fn resample_points(points: &[CursorPoint], hz: f64) -> Vec<CursorPoint> {
    if points.len() < 2 {
        return points.to_vec();
    }

    let safe_hz = if hz.is_finite() {
        hz.clamp(30.0, 240.0)
    } else {
        120.0
    };
    let step_ms = 1_000.0 / safe_hz;
    let mut sorted = dedupe_points(points.to_vec());
    if sorted.len() < 2 {
        return sorted;
    }

    sorted.sort_by_key(|point| point.ts);
    let start_ts = sorted.first().map(|point| point.ts).unwrap_or(0);
    let end_ts = sorted.last().map(|point| point.ts).unwrap_or(start_ts);
    if end_ts <= start_ts {
        return sorted;
    }

    let mut sample_ts: Vec<u64> = Vec::new();
    let mut t = start_ts as f64;
    let end_f = end_ts as f64;
    while t < end_f {
        sample_ts.push(t.round() as u64);
        t += step_ms;
    }
    sample_ts.push(end_ts);
    for click in sorted.iter().filter(|point| point.is_click) {
        sample_ts.push(click.ts);
    }
    sample_ts.sort_unstable();
    sample_ts.dedup();

    let mut result: Vec<CursorPoint> = Vec::with_capacity(sample_ts.len());
    let mut segment_index = 0usize;
    for ts in sample_ts {
        let mut point = sample_at_ts(&sorted, ts, &mut segment_index);
        point.is_click = sorted
            .iter()
            .any(|original| original.is_click && original.ts == ts);
        result.push(point);
    }

    dedupe_points(result)
}

fn sample_at_ts(points: &[CursorPoint], ts: u64, segment_index: &mut usize) -> CursorPoint {
    if points.is_empty() {
        return CursorPoint {
            ts,
            x: 0.0,
            y: 0.0,
            is_click: false,
        };
    }

    if ts <= points[0].ts {
        let mut point = points[0];
        point.ts = ts;
        point.is_click = false;
        return point;
    }

    let last = points[points.len() - 1];
    if ts >= last.ts {
        let mut point = last;
        point.ts = ts;
        point.is_click = false;
        return point;
    }

    while *segment_index + 1 < points.len() && points[*segment_index + 1].ts < ts {
        *segment_index += 1;
    }
    let left = points[*segment_index];
    let right = points[(*segment_index + 1).min(points.len() - 1)];

    if right.ts <= left.ts {
        return CursorPoint {
            ts,
            x: right.x,
            y: right.y,
            is_click: false,
        };
    }

    let ratio = (ts.saturating_sub(left.ts) as f64 / (right.ts - left.ts) as f64).clamp(0.0, 1.0);
    CursorPoint {
        ts,
        x: left.x + (right.x - left.x) * ratio,
        y: left.y + (right.y - left.y) * ratio,
        is_click: false,
    }
}

fn simple_moving_average_filter(points: &[CursorPoint], window_size: usize) -> Vec<CursorPoint> {
    if points.len() < 3 || window_size <= 1 {
        return points.to_vec();
    }

    let radius = window_size / 2;
    let mut filtered = Vec::with_capacity(points.len());

    for idx in 0..points.len() {
        let point = points[idx];
        if point.is_click {
            filtered.push(point);
            continue;
        }

        let start = idx.saturating_sub(radius);
        let end = (idx + radius + 1).min(points.len());

        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut count = 0usize;
        for sample in &points[start..end] {
            sum_x += sample.x;
            sum_y += sample.y;
            count += 1;
        }

        filtered.push(CursorPoint {
            ts: point.ts,
            x: sum_x / count as f64,
            y: sum_y / count as f64,
            is_click: false,
        });
    }

    filtered
}

fn catmull_rom_interpolate_impl(
    points: &[CursorPoint],
    samples_per_segment: usize,
) -> Vec<CursorPoint> {
    if points.len() < 2 {
        return points.to_vec();
    }

    let samples = samples_per_segment.max(2);
    let mut result = Vec::with_capacity((points.len() - 1) * samples + 1);
    const ALPHA: f64 = 0.5;

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

        let t0 = 0.0;
        let t1 = next_knot(t0, p0, p1, ALPHA);
        let t2 = next_knot(t1, p1, p2, ALPHA);
        let t3 = next_knot(t2, p2, p3, ALPHA);

        let mut curve_points: Vec<(f64, f64)> = Vec::with_capacity(samples + 1);
        for step in 0..=samples {
            let ratio = step as f64 / samples as f64;
            let t = t1 + (t2 - t1) * ratio;

            let a1 = interpolate_point(p0, p1, t0, t1, t);
            let a2 = interpolate_point(p1, p2, t1, t2, t);
            let a3 = interpolate_point(p2, p3, t2, t3, t);
            let b1 = interpolate_xy(a1, a2, t0, t2, t);
            let b2 = interpolate_xy(a2, a3, t1, t3, t);
            curve_points.push(interpolate_xy(b1, b2, t1, t2, t));
        }

        let mut cumulative = vec![0.0f64; curve_points.len()];
        for step in 1..curve_points.len() {
            let prev = curve_points[step - 1];
            let current = curve_points[step];
            cumulative[step] =
                cumulative[step - 1] + (current.0 - prev.0).hypot(current.1 - prev.1);
        }
        let total_len = cumulative.last().copied().unwrap_or(0.0);

        for step in 0..=samples {
            if idx > 0 && step == 0 {
                continue;
            }

            let position = curve_points[step];
            let ratio = if total_len <= 1e-9 {
                step as f64 / samples as f64
            } else {
                cumulative[step] / total_len
            };
            let ts = lerp_ts(p1.ts, p2.ts, ratio);
            let is_click = (step == 0 && p1.is_click) || (step == samples && p2.is_click);

            result.push(CursorPoint {
                ts,
                x: position.0,
                y: position.1,
                is_click,
            });
        }
    }

    result
}

fn next_knot(current: f64, a: CursorPoint, b: CursorPoint, alpha: f64) -> f64 {
    let distance = (b.x - a.x).hypot(b.y - a.y);
    current + distance.max(0.0001).powf(alpha)
}

fn interpolate_point(a: CursorPoint, b: CursorPoint, t_a: f64, t_b: f64, t: f64) -> (f64, f64) {
    interpolate_xy((a.x, a.y), (b.x, b.y), t_a, t_b, t)
}

fn interpolate_xy(a: (f64, f64), b: (f64, f64), t_a: f64, t_b: f64, t: f64) -> (f64, f64) {
    let span = (t_b - t_a).abs().max(1e-6);
    let r = ((t - t_a) / span).clamp(0.0, 1.0);
    (a.0 + (b.0 - a.0) * r, a.1 + (b.1 - a.1) * r)
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

    #[test]
    fn resampler_generates_stable_time_grid() {
        let points = vec![
            CursorPoint {
                ts: 0,
                x: 0.0,
                y: 0.0,
                is_click: false,
            },
            CursorPoint {
                ts: 37,
                x: 100.0,
                y: 0.0,
                is_click: false,
            },
            CursorPoint {
                ts: 142,
                x: 200.0,
                y: 50.0,
                is_click: false,
            },
        ];

        let resampled = resample_points(&points, 120.0);
        assert!(resampled.len() > points.len());
        let deltas: Vec<u64> = resampled
            .windows(2)
            .map(|pair| pair[1].ts.saturating_sub(pair[0].ts))
            .collect();
        assert!(deltas.iter().all(|delta| *delta >= 7 && *delta <= 10));
    }
}
