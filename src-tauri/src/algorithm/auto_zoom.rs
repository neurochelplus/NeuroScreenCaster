use crate::models::events::{BoundingRect, InputEvent, UiContext};
use crate::models::project::{NormalizedRect, PanKeyframe, ZoomEasing, ZoomSegment};

#[derive(Debug, Clone)]
pub struct AutoZoomConfig {
    pub cluster_gap_ms: u64,
    pub context_hold_ms: u64,
    pub context_max_focus_area_ratio: f64,
    pub context_max_control_name_len: usize,
    pub hold_ms: u64,
    pub min_segment_ms: u64,
    pub min_zoom_strength: f64,
    pub velocity_window_ms: u64,
    pub min_lookahead_ms: u64,
    pub base_lookahead_ms: u64,
    pub max_lookahead_ms: u64,
    pub lookahead_velocity_factor: f64,
    pub cluster_radius_ratio: f64,
    pub fallback_focus_ratio: f64,
    pub min_viewport_ratio: f64,
    pub smart_padding_ratio: f64,
    pub min_padding_px: f64,
    pub max_padding_px: f64,
    pub max_ui_rect_area_ratio: f64,
    pub missing_control_max_area_ratio: f64,
    pub missing_control_max_side_ratio: f64,
    pub scroll_pan_step_ratio: f64,
}

impl Default for AutoZoomConfig {
    fn default() -> Self {
        Self {
            cluster_gap_ms: 650,
            context_hold_ms: 2_600,
            context_max_focus_area_ratio: 0.25,
            context_max_control_name_len: 256,
            hold_ms: 550,
            min_segment_ms: 900,
            min_zoom_strength: 1.08,
            velocity_window_ms: 1_000,
            min_lookahead_ms: 220,
            base_lookahead_ms: 480,
            max_lookahead_ms: 1_700,
            lookahead_velocity_factor: 300.0,
            cluster_radius_ratio: 0.115,
            fallback_focus_ratio: 0.18,
            min_viewport_ratio: 0.14,
            smart_padding_ratio: 0.15,
            min_padding_px: 50.0,
            max_padding_px: 300.0,
            max_ui_rect_area_ratio: 0.45,
            missing_control_max_area_ratio: 0.25,
            missing_control_max_side_ratio: 0.72,
            scroll_pan_step_ratio: 0.10,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ScreenMetrics {
    width: f64,
    height: f64,
    scale_factor: f64,
}

impl ScreenMetrics {
    fn new(width: u32, height: u32, scale_factor: f64) -> Self {
        Self {
            width: width as f64,
            height: height as f64,
            scale_factor: if scale_factor.is_finite() {
                scale_factor.clamp(0.5, 4.0)
            } else {
                1.0
            },
        }
    }

    fn min_side(self) -> f64 {
        self.width.min(self.height).max(1.0)
    }

    fn diagonal(self) -> f64 {
        self.width.hypot(self.height)
    }

    fn adaptive_scale(self) -> f64 {
        let dpi_factor = 1.0 + (self.scale_factor - 1.0).max(0.0) * 0.35;
        let res_factor = (self.diagonal() / 2_202.9).clamp(0.75, 2.2);
        (dpi_factor * res_factor).sqrt()
    }

    fn cluster_radius_px(self, config: &AutoZoomConfig) -> f64 {
        let base = self.min_side() * config.cluster_radius_ratio * self.adaptive_scale();
        base.clamp(self.min_side() * 0.06, self.min_side() * 0.45)
    }

    fn fallback_size_px(self, config: &AutoZoomConfig) -> (f64, f64) {
        let height = (self.min_side() * config.fallback_focus_ratio * self.adaptive_scale())
            .clamp(self.height * 0.08, self.height * 0.45);
        let width = (height * (4.0 / 3.0)).clamp(self.width * 0.08, self.width * 0.45);
        (width, height)
    }

    fn min_viewport_px(self, config: &AutoZoomConfig, aspect_ratio: f64) -> (f64, f64) {
        let safe_aspect = aspect_ratio.max(0.1);
        let scale = self.adaptive_scale();
        let base = self.min_side() * config.min_viewport_ratio * scale;
        let mut min_height = base.max(self.height * 0.08);
        let mut min_width = min_height * safe_aspect;

        if min_width > self.width {
            min_width = self.width;
            min_height = min_width / safe_aspect;
        }
        if min_height > self.height {
            min_height = self.height;
            min_width = min_height * safe_aspect;
        }

        (min_width.max(1.0), min_height.max(1.0))
    }

    fn smart_padding_px(self, rect: RectPx, config: &AutoZoomConfig) -> f64 {
        let base = rect.width.max(rect.height) * config.smart_padding_ratio * self.adaptive_scale();
        let min_pad = config.min_padding_px * self.adaptive_scale().clamp(0.8, 1.8);
        let max_pad = config.max_padding_px * self.adaptive_scale().clamp(0.9, 1.8);
        base.clamp(min_pad, max_pad)
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

    fn center_x(self) -> f64 {
        self.x + self.width / 2.0
    }

    fn center_y(self) -> f64 {
        self.y + self.height / 2.0
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

    fn expand_to_aspect(self, aspect_ratio: f64) -> RectPx {
        let safe_aspect = aspect_ratio.max(0.1);
        let current = self.width / self.height.max(1.0);

        if (current - safe_aspect).abs() < f64::EPSILON {
            return self;
        }

        if current < safe_aspect {
            let width = self.height * safe_aspect;
            RectPx {
                x: self.center_x() - width / 2.0,
                y: self.y,
                width,
                height: self.height,
            }
        } else {
            let height = self.width / safe_aspect;
            RectPx {
                x: self.x,
                y: self.center_y() - height / 2.0,
                width: self.width,
                height,
            }
        }
    }

    fn clamp_to_screen_with_aspect(
        self,
        screen_width: f64,
        screen_height: f64,
        min_width: f64,
        min_height: f64,
        aspect_ratio: f64,
    ) -> RectPx {
        let safe_aspect = aspect_ratio.max(0.1);
        let mut width = self.width.max(min_width).max(1.0);
        let mut height = self.height.max(min_height).max(1.0);

        if width / height < safe_aspect {
            width = height * safe_aspect;
        } else {
            height = width / safe_aspect;
        }

        if width > screen_width {
            width = screen_width.max(1.0);
            height = width / safe_aspect;
        }
        if height > screen_height {
            height = screen_height.max(1.0);
            width = height * safe_aspect;
        }

        let max_x = (screen_width - width).max(0.0);
        let max_y = (screen_height - height).max(0.0);

        RectPx {
            x: (self.center_x() - width / 2.0).clamp(0.0, max_x),
            y: (self.center_y() - height / 2.0).clamp(0.0, max_y),
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

#[derive(Debug, Clone)]
struct ClickSample {
    ts: u64,
    x: f64,
    y: f64,
    focus_rect: RectPx,
    ui_context: Option<UiContext>,
}

#[derive(Debug, Clone)]
struct SemanticCluster {
    app_context: Option<String>,
    events: Vec<ClickSample>,
    bounds: RectPx,
}

impl SemanticCluster {
    fn from_sample(sample: ClickSample) -> Self {
        let app_context = context_key(sample.ui_context.as_ref());
        let bounds = sample.focus_rect;
        Self {
            app_context,
            events: vec![sample],
            bounds,
        }
    }

    fn push(&mut self, sample: ClickSample) {
        if self.app_context.is_none() {
            self.app_context = context_key(sample.ui_context.as_ref());
        }
        self.bounds = self.bounds.union(sample.focus_rect);
        self.events.push(sample);
    }
}

#[derive(Debug, Clone, Copy)]
struct PointerSample {
    ts: u64,
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy)]
struct ScrollSample {
    ts: u64,
    dx: f64,
    dy: f64,
}

pub fn build_auto_zoom_segments(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    duration_ms: u64,
) -> Vec<ZoomSegment> {
    build_auto_zoom_segments_with_context(
        events,
        screen_width,
        screen_height,
        1.0,
        duration_ms,
        16.0 / 9.0,
    )
}

pub fn build_auto_zoom_segments_with_context(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    scale_factor: f64,
    duration_ms: u64,
    output_aspect_ratio: f64,
) -> Vec<ZoomSegment> {
    build_auto_zoom_segments_with_context_and_config(
        events,
        screen_width,
        screen_height,
        scale_factor,
        duration_ms,
        output_aspect_ratio,
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
    build_auto_zoom_segments_with_context_and_config(
        events,
        screen_width,
        screen_height,
        1.0,
        duration_ms,
        16.0 / 9.0,
        config,
    )
}

pub fn build_auto_zoom_segments_with_context_and_config(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    scale_factor: f64,
    duration_ms: u64,
    output_aspect_ratio: f64,
    config: &AutoZoomConfig,
) -> Vec<ZoomSegment> {
    if screen_width == 0 || screen_height == 0 || duration_ms == 0 {
        return Vec::new();
    }

    let metrics = ScreenMetrics::new(screen_width, screen_height, scale_factor);
    let safe_aspect_ratio = if output_aspect_ratio.is_finite() && output_aspect_ratio > 0.05 {
        output_aspect_ratio
    } else {
        16.0 / 9.0
    };

    let samples = collect_click_samples(events, config, metrics);
    if samples.is_empty() {
        return Vec::new();
    }

    let pointer_samples = collect_pointer_samples(events);
    let scroll_samples = collect_scroll_samples(events);

    let clusters = cluster_clicks(&samples, config, metrics);
    let mut segments = Vec::with_capacity(clusters.len());
    let mut previous_end: Option<u64> = None;

    for cluster in clusters {
        if cluster.events.is_empty() {
            continue;
        }

        let first = &cluster.events[0];
        let last = cluster.events.last().expect("cluster must not be empty");

        let velocity = average_velocity_px_per_ms(
            &pointer_samples,
            first.ts.saturating_sub(config.velocity_window_ms),
            first.ts,
        );
        let lookahead_ms = dynamic_lookahead_ms(config, velocity);

        let mut start_ts = first.ts.saturating_sub(lookahead_ms);
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

        let padding = metrics.smart_padding_px(cluster.bounds, config);
        let (min_w, min_h) = metrics.min_viewport_px(config, safe_aspect_ratio);
        let initial_rect = cluster
            .bounds
            .expand(padding)
            .expand_to_aspect(safe_aspect_ratio)
            .clamp_to_screen_with_aspect(
                metrics.width,
                metrics.height,
                min_w,
                min_h,
                safe_aspect_ratio,
            )
            .to_normalized(metrics.width, metrics.height);
        let zoom_strength = 1.0 / initial_rect.width.max(initial_rect.height).max(0.0001);
        if zoom_strength < config.min_zoom_strength {
            continue;
        }

        let pan_trajectory = build_pan_trajectory(
            start_ts,
            end_ts,
            &scroll_samples,
            &initial_rect,
            config.scroll_pan_step_ratio,
        );

        segments.push(ZoomSegment {
            id: format!("auto-{}", segments.len() + 1),
            start_ts,
            end_ts,
            initial_rect,
            pan_trajectory,
            easing: ZoomEasing::EaseInOut,
            is_auto: true,
        });

        previous_end = Some(end_ts);
    }

    segments
}

fn collect_click_samples(
    events: &[InputEvent],
    config: &AutoZoomConfig,
    metrics: ScreenMetrics,
) -> Vec<ClickSample> {
    let (fallback_w, fallback_h) = metrics.fallback_size_px(config);

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
                    .filter(|rect| {
                        !should_replace_focus_with_fallback(
                            *rect,
                            ui_context.as_ref(),
                            metrics,
                            config,
                        )
                    })
                    .unwrap_or_else(|| fallback_rect(*x, *y, fallback_w, fallback_h));

                Some(ClickSample {
                    ts: *ts,
                    x: *x,
                    y: *y,
                    focus_rect,
                    ui_context: ui_context.clone(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    samples.sort_by_key(|sample| sample.ts);
    samples
}

fn collect_pointer_samples(events: &[InputEvent]) -> Vec<PointerSample> {
    let mut points = events
        .iter()
        .filter_map(|event| match event {
            InputEvent::Move { ts, x, y }
            | InputEvent::Click { ts, x, y, .. }
            | InputEvent::MouseUp { ts, x, y, .. }
            | InputEvent::Scroll { ts, x, y, .. } => Some(PointerSample {
                ts: *ts,
                x: *x,
                y: *y,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    points.sort_by_key(|sample| sample.ts);
    points
}

fn collect_scroll_samples(events: &[InputEvent]) -> Vec<ScrollSample> {
    let mut scrolls = events
        .iter()
        .filter_map(|event| match event {
            InputEvent::Scroll { ts, delta, .. } => Some(ScrollSample {
                ts: *ts,
                dx: delta.dx,
                dy: delta.dy,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    scrolls.sort_by_key(|sample| sample.ts);
    scrolls
}

fn context_key(ui_context: Option<&UiContext>) -> Option<String> {
    let context = ui_context?;

    normalize_context(context.app_name.as_deref())
        .or_else(|| normalize_context(context.control_name.as_deref()))
}

fn normalized_control_name(ui_context: Option<&UiContext>, max_len: usize) -> Option<String> {
    let value = normalize_context(ui_context?.control_name.as_deref())?;
    if value.len() > max_len {
        return None;
    }
    Some(value)
}

fn focus_area_ratio(rect: RectPx, metrics: ScreenMetrics) -> f64 {
    (rect.width * rect.height / (metrics.width * metrics.height).max(1.0)).clamp(0.0, 4.0)
}

fn should_replace_focus_with_fallback(
    rect: RectPx,
    ui_context: Option<&UiContext>,
    metrics: ScreenMetrics,
    config: &AutoZoomConfig,
) -> bool {
    let area_ratio = focus_area_ratio(rect, metrics);
    let has_reliable_control =
        normalized_control_name(ui_context, config.context_max_control_name_len).is_some();

    if !has_reliable_control {
        let width_ratio = (rect.width / metrics.width).clamp(0.0, 1.0);
        let height_ratio = (rect.height / metrics.height).clamp(0.0, 1.0);
        if area_ratio > config.missing_control_max_area_ratio
            || width_ratio > config.missing_control_max_side_ratio
            || height_ratio > config.missing_control_max_side_ratio
        {
            return true;
        }
    }

    if area_ratio <= config.max_ui_rect_area_ratio {
        return false;
    }

    !has_reliable_control
}

fn context_merge_confident(
    sample: &ClickSample,
    metrics: ScreenMetrics,
    config: &AutoZoomConfig,
) -> bool {
    normalized_control_name(
        sample.ui_context.as_ref(),
        config.context_max_control_name_len,
    )
    .is_some()
        || focus_area_ratio(sample.focus_rect, metrics) <= config.context_max_focus_area_ratio
}

fn normalize_context(value: Option<&str>) -> Option<String> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }

    Some(raw.to_ascii_lowercase())
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

fn cluster_clicks(
    samples: &[ClickSample],
    config: &AutoZoomConfig,
    metrics: ScreenMetrics,
) -> Vec<SemanticCluster> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut clusters: Vec<SemanticCluster> = Vec::new();
    let mut current = SemanticCluster::from_sample(samples[0].clone());
    let max_distance = metrics.cluster_radius_px(config);

    for sample in samples.iter().skip(1).cloned() {
        let prev = current
            .events
            .last()
            .expect("semantic cluster should contain at least one sample");

        let gap_ms = sample.ts.saturating_sub(prev.ts);
        let distance_px = (sample.x - prev.x).hypot(sample.y - prev.y);

        let sample_context = context_key(sample.ui_context.as_ref());
        let same_context = match (current.app_context.as_deref(), sample_context.as_deref()) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
        let context_close = same_context
            && gap_ms <= config.context_hold_ms
            && context_merge_confident(prev, metrics, config)
            && context_merge_confident(&sample, metrics, config);
        let proximity_close = gap_ms <= config.cluster_gap_ms && distance_px <= max_distance;

        if context_close || proximity_close {
            current.push(sample);
        } else {
            clusters.push(current);
            current = SemanticCluster::from_sample(sample);
        }
    }

    if !current.events.is_empty() {
        clusters.push(current);
    }

    clusters
}

fn average_velocity_px_per_ms(
    samples: &[PointerSample],
    window_start: u64,
    window_end: u64,
) -> f64 {
    if samples.is_empty() || window_end <= window_start {
        return 0.0;
    }

    let mut prev: Option<PointerSample> = None;
    let mut total_distance = 0.0;
    let mut total_duration = 0.0;

    for sample in samples {
        if sample.ts < window_start || sample.ts > window_end {
            continue;
        }

        if let Some(last) = prev {
            let dt = sample.ts.saturating_sub(last.ts) as f64;
            if dt > 0.0 {
                total_distance += (sample.x - last.x).hypot(sample.y - last.y);
                total_duration += dt;
            }
        }

        prev = Some(*sample);
    }

    if total_duration <= 0.0 {
        0.0
    } else {
        (total_distance / total_duration).clamp(0.0, 20.0)
    }
}

fn dynamic_lookahead_ms(config: &AutoZoomConfig, velocity_px_per_ms: f64) -> u64 {
    let raw =
        config.base_lookahead_ms as f64 + velocity_px_per_ms * config.lookahead_velocity_factor;
    (raw.round() as u64).clamp(config.min_lookahead_ms, config.max_lookahead_ms)
}

fn build_pan_trajectory(
    start_ts: u64,
    end_ts: u64,
    scrolls: &[ScrollSample],
    initial_rect: &NormalizedRect,
    scroll_pan_step_ratio: f64,
) -> Vec<PanKeyframe> {
    if start_ts >= end_ts {
        return Vec::new();
    }

    let mut trajectory: Vec<PanKeyframe> = Vec::new();
    let mut offset_x = 0.0;
    let mut offset_y = 0.0;

    let min_offset_x = -initial_rect.x;
    let max_offset_x = (1.0 - initial_rect.width - initial_rect.x).max(min_offset_x);
    let min_offset_y = -initial_rect.y;
    let max_offset_y = (1.0 - initial_rect.height - initial_rect.y).max(min_offset_y);

    for scroll in scrolls {
        if scroll.ts < start_ts || scroll.ts > end_ts {
            continue;
        }

        let normalized_dx = normalize_scroll_delta(scroll.dx);
        let normalized_dy = normalize_scroll_delta(scroll.dy);

        offset_x += normalized_dx * initial_rect.width * scroll_pan_step_ratio;
        offset_y += -normalized_dy * initial_rect.height * scroll_pan_step_ratio;

        offset_x = offset_x.clamp(min_offset_x, max_offset_x);
        offset_y = offset_y.clamp(min_offset_y, max_offset_y);

        push_pan_keyframe(
            &mut trajectory,
            PanKeyframe {
                ts: scroll.ts,
                offset_x,
                offset_y,
            },
        );
    }

    if trajectory.is_empty() {
        return Vec::new();
    }

    trajectory.insert(
        0,
        PanKeyframe {
            ts: start_ts,
            offset_x: 0.0,
            offset_y: 0.0,
        },
    );
    push_pan_keyframe(
        &mut trajectory,
        PanKeyframe {
            ts: end_ts,
            offset_x,
            offset_y,
        },
    );

    trajectory
}

fn push_pan_keyframe(trajectory: &mut Vec<PanKeyframe>, keyframe: PanKeyframe) {
    if let Some(last) = trajectory.last_mut() {
        if last.ts == keyframe.ts {
            *last = keyframe;
            return;
        }
    }
    trajectory.push(keyframe);
}

fn normalize_scroll_delta(raw_delta: f64) -> f64 {
    if raw_delta.abs() >= 100.0 {
        (raw_delta / 120.0).clamp(-6.0, 6.0)
    } else {
        raw_delta.clamp(-6.0, 6.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::events::{InputEvent, MouseButton, ScrollDelta, UiContext};

    fn click(
        ts: u64,
        x: f64,
        y: f64,
        app_name: Option<&str>,
        rect: Option<BoundingRect>,
    ) -> InputEvent {
        InputEvent::Click {
            ts,
            x,
            y,
            button: MouseButton::Left,
            ui_context: Some(UiContext {
                app_name: app_name.map(|value| value.to_string()),
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
            click(1_000, 300.0, 300.0, None, None),
            click(1_240, 360.0, 320.0, None, None),
            click(4_000, 1_500.0, 900.0, None, None),
        ];

        let segments = build_auto_zoom_segments(&events, 1_920, 1_080, 8_000);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].end_ts <= segments[1].start_ts);
    }

    #[test]
    fn keeps_same_app_clicks_in_one_cluster_even_when_far() {
        let config = AutoZoomConfig {
            min_zoom_strength: 1.0,
            ..AutoZoomConfig::default()
        };
        let events = vec![
            click(
                1_000,
                200.0,
                220.0,
                Some("code.exe"),
                Some(BoundingRect {
                    x: 100,
                    y: 120,
                    width: 180,
                    height: 100,
                }),
            ),
            click(
                2_100,
                1_700.0,
                900.0,
                Some("code.exe"),
                Some(BoundingRect {
                    x: 1_600,
                    y: 860,
                    width: 200,
                    height: 120,
                }),
            ),
        ];

        let segments = build_auto_zoom_segments_with_context_and_config(
            &events,
            1_920,
            1_080,
            1.0,
            5_000,
            16.0 / 9.0,
            &config,
        );
        assert_eq!(segments.len(), 1);

        let rect = &segments[0].initial_rect;
        assert!(rect.width > 0.75);
        assert!(rect.height > 0.40);
    }

    #[test]
    fn uses_ui_rect_when_available() {
        let config = AutoZoomConfig {
            min_lookahead_ms: 0,
            base_lookahead_ms: 0,
            hold_ms: 150,
            min_segment_ms: 150,
            smart_padding_ratio: 0.0,
            min_padding_px: 0.0,
            max_padding_px: 0.0,
            min_viewport_ratio: 0.001,
            ..AutoZoomConfig::default()
        };
        let events = vec![click(
            500,
            1_000.0,
            500.0,
            None,
            Some(BoundingRect {
                x: 900,
                y: 450,
                width: 300,
                height: 120,
            }),
        )];

        let segments = build_auto_zoom_segments_with_context_and_config(
            &events,
            1_920,
            1_080,
            1.0,
            2_000,
            300.0 / 120.0,
            &config,
        );
        assert_eq!(segments.len(), 1);
        let rect = &segments[0].initial_rect;

        assert!(approx_eq(rect.x, 900.0 / 1_920.0));
        assert!(approx_eq(rect.y, 450.0 / 1_080.0));
        assert!(approx_eq(rect.width, 300.0 / 1_920.0));
        assert!(approx_eq(rect.height, 120.0 / 1_080.0));
    }

    #[test]
    fn enforces_aspect_ratio_and_bounds() {
        let events = vec![click(100, 5.0, 5.0, None, None)];

        let segments =
            build_auto_zoom_segments_with_context(&events, 1_280, 720, 1.0, 2_000, 16.0 / 9.0);
        assert_eq!(segments.len(), 1);
        let rect = &segments[0].initial_rect;

        assert!(rect.x >= 0.0 && rect.y >= 0.0);
        assert!(rect.x + rect.width <= 1.0);
        assert!(rect.y + rect.height <= 1.0);
        let ratio = (rect.width * 1_280.0) / (rect.height.max(0.0001) * 720.0);
        assert!((ratio - (16.0 / 9.0)).abs() < 0.03);
    }

    #[test]
    fn builds_pan_trajectory_when_scroll_exists() {
        let events = vec![
            click(1_000, 400.0, 300.0, None, None),
            InputEvent::Scroll {
                ts: 1_400,
                x: 400.0,
                y: 300.0,
                delta: ScrollDelta {
                    dx: 0.0,
                    dy: -120.0,
                },
            },
            InputEvent::Scroll {
                ts: 1_700,
                x: 400.0,
                y: 300.0,
                delta: ScrollDelta {
                    dx: 0.0,
                    dy: -120.0,
                },
            },
        ];

        let segments = build_auto_zoom_segments(&events, 1_920, 1_080, 4_000);
        assert_eq!(segments.len(), 1);
        assert!(!segments[0].pan_trajectory.is_empty());

        let last = segments[0]
            .pan_trajectory
            .last()
            .expect("pan trajectory must have last keyframe");
        assert!(last.offset_y > 0.0);
    }

    #[test]
    fn increases_lookahead_with_higher_cursor_velocity() {
        let slow_events = vec![
            InputEvent::Move {
                ts: 100,
                x: 100.0,
                y: 100.0,
            },
            InputEvent::Move {
                ts: 900,
                x: 130.0,
                y: 100.0,
            },
            click(1_200, 135.0, 100.0, None, None),
        ];
        let fast_events = vec![
            InputEvent::Move {
                ts: 100,
                x: 100.0,
                y: 100.0,
            },
            InputEvent::Move {
                ts: 900,
                x: 1_300.0,
                y: 100.0,
            },
            click(1_200, 1_320.0, 100.0, None, None),
        ];

        let slow_segments = build_auto_zoom_segments(&slow_events, 1_920, 1_080, 5_000);
        let fast_segments = build_auto_zoom_segments(&fast_events, 1_920, 1_080, 5_000);

        assert_eq!(slow_segments.len(), 1);
        assert_eq!(fast_segments.len(), 1);
        assert!(fast_segments[0].start_ts < slow_segments[0].start_ts);
    }

    #[test]
    fn does_not_emit_fullscreen_segments_for_coarse_context() {
        let events = vec![
            click(
                1_000,
                1_000.0,
                500.0,
                Some("pid:5584"),
                Some(BoundingRect {
                    x: 0,
                    y: 0,
                    width: 1_920,
                    height: 1_078,
                }),
            ),
            click(
                3_000,
                400.0,
                500.0,
                Some("pid:5584"),
                Some(BoundingRect {
                    x: 0,
                    y: 60,
                    width: 718,
                    height: 977,
                }),
            ),
        ];

        let segments = build_auto_zoom_segments(&events, 1_920, 1_080, 6_000);
        assert!(!segments.is_empty());
        assert!(segments.iter().all(
            |segment| segment.initial_rect.width < 0.999 || segment.initial_rect.height < 0.999
        ));
    }

    #[test]
    fn empty_control_with_large_panel_rect_falls_back_to_click_focus() {
        let events = vec![InputEvent::Click {
            ts: 6_345,
            x: 312.0,
            y: 951.0,
            button: MouseButton::Left,
            ui_context: Some(UiContext {
                app_name: Some("pid:5584".to_string()),
                control_name: None,
                bounding_rect: Some(BoundingRect {
                    x: 0,
                    y: 60,
                    width: 718,
                    height: 977,
                }),
            }),
        }];

        let segments = build_auto_zoom_segments(&events, 1_920, 1_080, 12_000);
        assert_eq!(segments.len(), 1);
        let zoom_strength = 1.0
            / segments[0]
                .initial_rect
                .width
                .max(segments[0].initial_rect.height)
                .max(0.0001);
        assert!(zoom_strength > 1.5);
    }
}
