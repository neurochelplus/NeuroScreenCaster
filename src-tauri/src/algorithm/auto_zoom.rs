use crate::models::events::{BoundingRect, InputEvent};
use crate::models::project::{NormalizedRect, ZoomEasing, ZoomSegment};

#[derive(Debug, Clone)]
pub struct AutoZoomConfig {
    pub cluster_gap_ms: u64,
    pub cluster_radius_px: f64,
    pub lookahead_ms: u64,
    pub hold_ms: u64,
    pub min_segment_ms: u64,
    pub padding_px: f64,
    pub fallback_width_px: f64,
    pub fallback_height_px: f64,
    pub min_viewport_width_px: f64,
    pub min_viewport_height_px: f64,
}

impl Default for AutoZoomConfig {
    fn default() -> Self {
        Self {
            cluster_gap_ms: 650,
            cluster_radius_px: 220.0,
            lookahead_ms: 600,
            hold_ms: 500,
            min_segment_ms: 900,
            padding_px: 80.0,
            fallback_width_px: 400.0,
            fallback_height_px: 300.0,
            min_viewport_width_px: 280.0,
            min_viewport_height_px: 220.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RectPx {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl RectPx {
    fn right(self) -> f64 {
        self.x + self.width
    }

    fn bottom(self) -> f64 {
        self.y + self.height
    }

    fn union(self, other: RectPx) -> RectPx {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());

        RectPx {
            x: left,
            y: top,
            width: (right - left).max(1.0),
            height: (bottom - top).max(1.0),
        }
    }

    fn expand(self, padding: f64) -> RectPx {
        RectPx {
            x: self.x - padding,
            y: self.y - padding,
            width: self.width + padding * 2.0,
            height: self.height + padding * 2.0,
        }
    }

    fn clamp_to_screen(
        self,
        screen_width: f64,
        screen_height: f64,
        min_width: f64,
        min_height: f64,
    ) -> RectPx {
        let width = self.width.max(min_width).min(screen_width.max(1.0));
        let height = self.height.max(min_height).min(screen_height.max(1.0));
        let max_x = (screen_width - width).max(0.0);
        let max_y = (screen_height - height).max(0.0);

        RectPx {
            x: self.x.clamp(0.0, max_x),
            y: self.y.clamp(0.0, max_y),
            width,
            height,
        }
    }

    fn to_normalized(self, screen_width: f64, screen_height: f64) -> NormalizedRect {
        let sw = screen_width.max(1.0);
        let sh = screen_height.max(1.0);

        NormalizedRect {
            x: (self.x / sw).clamp(0.0, 1.0),
            y: (self.y / sh).clamp(0.0, 1.0),
            width: (self.width / sw).clamp(0.0, 1.0),
            height: (self.height / sh).clamp(0.0, 1.0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ClickSample {
    ts: u64,
    x: f64,
    y: f64,
    focus_rect: RectPx,
}

pub fn build_auto_zoom_segments(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    duration_ms: u64,
) -> Vec<ZoomSegment> {
    build_auto_zoom_segments_with_config(
        events,
        screen_width,
        screen_height,
        duration_ms,
        &AutoZoomConfig::default(),
    )
}

pub fn build_auto_zoom_segments_with_config(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    duration_ms: u64,
    config: &AutoZoomConfig,
) -> Vec<ZoomSegment> {
    if screen_width == 0 || screen_height == 0 || duration_ms == 0 {
        return Vec::new();
    }

    let samples = collect_click_samples(events, config);
    if samples.is_empty() {
        return Vec::new();
    }

    let clusters = cluster_clicks(&samples, config);
    let mut segments = Vec::with_capacity(clusters.len());
    let mut previous_end: Option<u64> = None;
    let sw = screen_width as f64;
    let sh = screen_height as f64;

    for cluster in clusters {
        let first = cluster.first().expect("cluster must not be empty");
        let last = cluster.last().expect("cluster must not be empty");

        let mut start_ts = first.ts.saturating_sub(config.lookahead_ms);
        if let Some(prev_end) = previous_end {
            if start_ts <= prev_end {
                start_ts = prev_end.saturating_add(1);
            }
        }
        if start_ts >= duration_ms {
            break;
        }

        let mut end_ts = last.ts.saturating_add(config.hold_ms);
        let min_end_ts = start_ts.saturating_add(config.min_segment_ms);
        if end_ts < min_end_ts {
            end_ts = min_end_ts;
        }
        if end_ts > duration_ms {
            end_ts = duration_ms;
        }
        if end_ts <= start_ts {
            continue;
        }

        let union_rect = cluster
            .iter()
            .skip(1)
            .fold(cluster[0].focus_rect, |acc, sample| {
                acc.union(sample.focus_rect)
            });
        let target_rect = union_rect
            .expand(config.padding_px)
            .clamp_to_screen(
                sw,
                sh,
                config.min_viewport_width_px,
                config.min_viewport_height_px,
            )
            .to_normalized(sw, sh);

        segments.push(ZoomSegment {
            id: format!("auto-{}", segments.len() + 1),
            start_ts,
            end_ts,
            target_rect,
            easing: ZoomEasing::EaseInOut,
            is_auto: true,
        });
        previous_end = Some(end_ts);
    }

    segments
}

fn collect_click_samples(events: &[InputEvent], config: &AutoZoomConfig) -> Vec<ClickSample> {
    let mut samples = events
        .iter()
        .filter_map(|event| match event {
            InputEvent::Click {
                ts,
                x,
                y,
                ui_context,
                ..
            } => {
                let focus_rect = ui_context
                    .as_ref()
                    .and_then(|ctx| ctx.bounding_rect.as_ref())
                    .and_then(rect_from_ui_context)
                    .unwrap_or_else(|| {
                        fallback_rect(*x, *y, config.fallback_width_px, config.fallback_height_px)
                    });

                Some(ClickSample {
                    ts: *ts,
                    x: *x,
                    y: *y,
                    focus_rect,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    samples.sort_by_key(|sample| sample.ts);
    samples
}

fn rect_from_ui_context(rect: &BoundingRect) -> Option<RectPx> {
    if rect.width == 0 || rect.height == 0 {
        return None;
    }

    Some(RectPx {
        x: rect.x as f64,
        y: rect.y as f64,
        width: rect.width as f64,
        height: rect.height as f64,
    })
}

fn fallback_rect(x: f64, y: f64, width: f64, height: f64) -> RectPx {
    RectPx {
        x: x - width / 2.0,
        y: y - height / 2.0,
        width,
        height,
    }
}

fn cluster_clicks(samples: &[ClickSample], config: &AutoZoomConfig) -> Vec<Vec<ClickSample>> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut clusters = Vec::new();
    let mut current: Vec<ClickSample> = Vec::new();

    for sample in samples {
        if let Some(prev) = current.last() {
            let gap_ms = sample.ts.saturating_sub(prev.ts);
            let distance_px = (sample.x - prev.x).hypot(sample.y - prev.y);

            if gap_ms > config.cluster_gap_ms || distance_px > config.cluster_radius_px {
                clusters.push(current);
                current = Vec::new();
            }
        }
        current.push(*sample);
    }

    if !current.is_empty() {
        clusters.push(current);
    }

    clusters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::events::{InputEvent, MouseButton, UiContext};

    fn click(ts: u64, x: f64, y: f64, rect: Option<BoundingRect>) -> InputEvent {
        InputEvent::Click {
            ts,
            x,
            y,
            button: MouseButton::Left,
            ui_context: Some(UiContext {
                app_name: None,
                control_name: None,
                bounding_rect: rect,
            }),
        }
    }

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.0001
    }

    #[test]
    fn groups_fast_clicks_into_one_segment() {
        let events = vec![
            click(1_000, 300.0, 300.0, None),
            click(1_240, 360.0, 320.0, None),
            click(4_000, 1_500.0, 900.0, None),
        ];

        let segments = build_auto_zoom_segments(&events, 1_920, 1_080, 8_000);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ts, 400);
        assert!(segments[0].end_ts <= segments[1].start_ts);
    }

    #[test]
    fn uses_ui_rect_when_available() {
        let config = AutoZoomConfig {
            lookahead_ms: 0,
            hold_ms: 150,
            min_segment_ms: 150,
            padding_px: 0.0,
            min_viewport_width_px: 1.0,
            min_viewport_height_px: 1.0,
            ..AutoZoomConfig::default()
        };
        let events = vec![click(
            500,
            1_000.0,
            500.0,
            Some(BoundingRect {
                x: 900,
                y: 450,
                width: 300,
                height: 120,
            }),
        )];

        let segments = build_auto_zoom_segments_with_config(&events, 1_920, 1_080, 2_000, &config);
        assert_eq!(segments.len(), 1);
        let rect = &segments[0].target_rect;

        assert!(approx_eq(rect.x, 900.0 / 1_920.0));
        assert!(approx_eq(rect.y, 450.0 / 1_080.0));
        assert!(approx_eq(rect.width, 300.0 / 1_920.0));
        assert!(approx_eq(rect.height, 120.0 / 1_080.0));
    }

    #[test]
    fn keeps_target_rect_inside_screen_bounds() {
        let config = AutoZoomConfig {
            lookahead_ms: 0,
            hold_ms: 100,
            min_segment_ms: 100,
            padding_px: 120.0,
            fallback_width_px: 200.0,
            fallback_height_px: 150.0,
            ..AutoZoomConfig::default()
        };
        let events = vec![click(100, 5.0, 5.0, None)];

        let segments = build_auto_zoom_segments_with_config(&events, 1_280, 720, 2_000, &config);
        assert_eq!(segments.len(), 1);
        let rect = &segments[0].target_rect;

        assert!(rect.x >= 0.0 && rect.y >= 0.0);
        assert!(rect.width > 0.0 && rect.height > 0.0);
        assert!(rect.x + rect.width <= 1.0);
        assert!(rect.y + rect.height <= 1.0);
    }
}
