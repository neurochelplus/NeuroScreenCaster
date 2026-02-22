use crate::models::events::{BoundingRect, InputEvent};
use crate::models::project::{
    CameraSpring, NormalizedRect, TargetPoint, ZoomMode, ZoomSegment, ZoomTrigger,
};

#[derive(Debug, Clone, Copy)]
pub enum CameraState {
    FreeRoam,
    LockedFocus {
        focus_center_x: f64,
        focus_center_y: f64,
        focus_zoom: f64,
        cluster_end_ts: u64,
    },
}

impl CameraState {
    pub fn is_locked(self) -> bool {
        matches!(self, CameraState::LockedFocus { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub current_pos: f64,
    pub target_pos: f64,
    pub velocity: f64,
    pub k: f64,
    pub c: f64,
    pub m: f64,
}

impl Spring {
    pub fn new(
        current_pos: f64,
        target_pos: f64,
        velocity: f64,
        stiffness: f64,
        damping: f64,
        mass: f64,
    ) -> Self {
        Self {
            current_pos,
            target_pos,
            velocity,
            k: stiffness.max(0.0001),
            c: damping.max(0.0),
            m: mass.max(0.0001),
        }
    }

    pub fn critical_damping(stiffness: f64, mass: f64) -> f64 {
        2.0 * (stiffness.max(0.0001) * mass.max(0.0001)).sqrt()
    }

    pub fn tick(&mut self, dt: f64) -> f64 {
        let safe_dt = dt.max(0.000_001);
        let acceleration =
            (self.k * (self.target_pos - self.current_pos) - self.c * self.velocity) / self.m;
        self.velocity += acceleration * safe_dt;
        self.current_pos += self.velocity * safe_dt;
        self.current_pos
    }
}

#[derive(Debug, Clone)]
pub struct SmartCameraConfig {
    pub fixed_dt_ms: u64,
    pub dead_zone_ratio: f64,
    pub hard_edge_ratio: f64,
    pub hard_edge_pan_speed_px_per_s: f64,
    pub escape_distance_ratio: f64,
    pub scroll_shift_ratio: f64,
    pub scroll_idle_reset_ms: u64,
    pub global_scroll_duration_ms: u64,
    pub global_scroll_viewport_travel_ratio: f64,
    pub semantic_padding_ratio: f64,
    pub fallback_zoom: f64,
    pub free_roam_zoom: f64,
    pub max_zoom_limit: f64,
    pub safe_zone_margin_ratio: f64,
    pub max_lookahead_ms: u64,
    pub velocity_threshold_px_per_ms: f64,
    pub activation_window_ms: u64,
    pub min_clicks_to_activate: usize,
    pub click_cluster_gap_ms: u64,
    pub min_zoom_interval_ms: u64,
    pub min_lock_duration_ms: u64,
    pub lock_recent_window_ms: u64,
    pub spring_mass: f64,
    pub spring_stiffness: f64,
    pub spring_damping: f64,
    pub segment_target_sample_ms: u64,
}

impl Default for SmartCameraConfig {
    fn default() -> Self {
        let mass = 1.0;
        let stiffness = 170.0;
        let damping = Spring::critical_damping(stiffness, mass);
        Self {
            fixed_dt_ms: 8,
            dead_zone_ratio: 0.40,
            hard_edge_ratio: 0.35,
            hard_edge_pan_speed_px_per_s: 1_200.0,
            escape_distance_ratio: 0.80,
            scroll_shift_ratio: 0.10,
            scroll_idle_reset_ms: 300,
            global_scroll_duration_ms: 3_000,
            global_scroll_viewport_travel_ratio: 1.5,
            semantic_padding_ratio: 0.20,
            fallback_zoom: 2.0,
            free_roam_zoom: 1.0,
            max_zoom_limit: 2.5,
            safe_zone_margin_ratio: 0.15,
            max_lookahead_ms: 400,
            velocity_threshold_px_per_ms: 0.55,
            activation_window_ms: 3_000,
            min_clicks_to_activate: 2,
            click_cluster_gap_ms: 300,
            min_zoom_interval_ms: 2_000,
            min_lock_duration_ms: 1_800,
            lock_recent_window_ms: 2_200,
            spring_mass: mass,
            spring_stiffness: stiffness,
            spring_damping: damping,
            segment_target_sample_ms: 75,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CameraSample {
    pub ts: u64,
    pub state: CameraState,
    pub center_x: f64,
    pub center_y: f64,
    pub zoom: f64,
    pub target_center_x: f64,
    pub target_center_y: f64,
    pub target_zoom: f64,
}

#[derive(Debug, Clone, Copy)]
struct RectPx {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl RectPx {
    fn center_x(self) -> f64 {
        self.x + self.width * 0.5
    }

    fn center_y(self) -> f64 {
        self.y + self.height * 0.5
    }

    fn union(self, other: RectPx) -> RectPx {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        RectPx {
            x: left,
            y: top,
            width: (right - left).max(1.0),
            height: (bottom - top).max(1.0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CursorSample {
    ts: u64,
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy)]
struct VelocitySample {
    ts: u64,
    speed_px_per_ms: f64,
}

#[derive(Debug, Clone, Copy)]
struct FocusClick {
    ts: u64,
    x: f64,
    y: f64,
    bounds: Option<RectPx>,
}

#[derive(Debug, Clone, Copy)]
struct FocusCluster {
    start_ts: u64,
    end_ts: u64,
    avg_x: f64,
    avg_y: f64,
    anchor_x: f64,
    anchor_y: f64,
    bounds: Option<RectPx>,
    click_count: usize,
}

#[derive(Debug, Clone, Copy)]
struct FocusTransition {
    start_ts: u64,
    trigger_ts: u64,
    cluster_end_ts: u64,
    center_x: f64,
    center_y: f64,
    zoom: f64,
    focus_rect: RectNorm,
}

#[derive(Debug, Clone, Copy)]
struct RectNorm {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl RectNorm {
    fn right(self) -> f64 {
        self.x + self.width
    }

    fn bottom(self) -> f64 {
        self.y + self.height
    }

    fn contains(self, other: RectNorm) -> bool {
        let eps = 1e-6;
        other.x >= self.x - eps
            && other.y >= self.y - eps
            && other.right() <= self.right() + eps
            && other.bottom() <= self.bottom() + eps
    }
}

pub fn process_camera_targets(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    duration_ms: u64,
    output_aspect_ratio: f64,
    config: &SmartCameraConfig,
) -> Vec<CameraSample> {
    if events.is_empty() || screen_width == 0 || screen_height == 0 || duration_ms == 0 {
        return Vec::new();
    }

    let width = screen_width as f64;
    let height = screen_height as f64;
    let safe_aspect = output_aspect_ratio.max(0.1);
    let safe_step_ms = config.fixed_dt_ms.max(1);
    let dt_seconds = safe_step_ms as f64 / 1000.0;

    let cursor_samples = collect_cursor_samples(events);
    let velocity_samples = build_velocity_samples(&cursor_samples);
    let transitions = build_focus_transitions(
        events,
        screen_width,
        screen_height,
        safe_aspect,
        &velocity_samples,
        config,
    );

    let mut sorted_events: Vec<&InputEvent> = events.iter().collect();
    sorted_events.sort_by_key(|event| event.ts());

    let mut state = CameraState::FreeRoam;
    let mut target_center_x = 0.5;
    let mut target_center_y = 0.5;
    let mut target_zoom = config.free_roam_zoom.max(1.0);
    let mut free_roam_center_x = 0.5;
    let mut free_roam_center_y = 0.5;
    let mut cursor_x = width * 0.5;
    let mut cursor_y = height * 0.5;

    let mut spring_x = Spring::new(
        target_center_x,
        target_center_x,
        0.0,
        config.spring_stiffness,
        config.spring_damping,
        config.spring_mass,
    );
    let mut spring_y = Spring::new(
        target_center_y,
        target_center_y,
        0.0,
        config.spring_stiffness,
        config.spring_damping,
        config.spring_mass,
    );
    let mut spring_z = Spring::new(
        target_zoom,
        target_zoom,
        0.0,
        config.spring_stiffness,
        config.spring_damping,
        config.spring_mass,
    );

    let mut event_idx = 0usize;
    let mut transition_idx = 0usize;
    let mut samples: Vec<CameraSample> = Vec::new();
    let mut scroll_session_start_ts: Option<u64> = None;
    let mut last_scroll_ts: Option<u64> = None;
    let mut scroll_accum_abs_dy = 0.0;
    let mut force_zoom_out_from_scroll = false;

    let mut ts = 0u64;
    loop {
        while event_idx < sorted_events.len() && sorted_events[event_idx].ts() <= ts {
            let event = sorted_events[event_idx];
            match event {
                InputEvent::Move { x, y, .. }
                | InputEvent::Click { x, y, .. }
                | InputEvent::MouseUp { x, y, .. }
                | InputEvent::Scroll { x, y, .. } => {
                    cursor_x = x.clamp(0.0, width);
                    cursor_y = y.clamp(0.0, height);
                }
                _ => {}
            }

            if let InputEvent::Scroll { ts, delta, .. } = event {
                let reset_scroll_session = last_scroll_ts.is_none_or(|last_ts| {
                    ts.saturating_sub(last_ts) > config.scroll_idle_reset_ms.max(1)
                });
                if reset_scroll_session {
                    scroll_session_start_ts = Some(*ts);
                    scroll_accum_abs_dy = 0.0;
                }
                scroll_accum_abs_dy += delta.dy.abs();
                last_scroll_ts = Some(*ts);

                let session_start = scroll_session_start_ts.unwrap_or(*ts);
                let session_duration = ts.saturating_sub(session_start);
                let viewport_travel_threshold =
                    height * config.global_scroll_viewport_travel_ratio.max(0.0);
                if session_duration >= config.global_scroll_duration_ms.max(1)
                    || scroll_accum_abs_dy >= viewport_travel_threshold
                {
                    force_zoom_out_from_scroll = true;
                    scroll_session_start_ts = None;
                    last_scroll_ts = None;
                    scroll_accum_abs_dy = 0.0;
                }

                if let CameraState::LockedFocus {
                    focus_center_x,
                    focus_center_y,
                    focus_zoom,
                    cluster_end_ts,
                } = state
                {
                    let mut next_center_y = focus_center_y
                        - normalize_scroll_delta(delta.dy) * config.scroll_shift_ratio;
                    let (view_w, view_h) = viewport_size_from_zoom(
                        focus_zoom,
                        screen_width,
                        screen_height,
                        safe_aspect,
                    );
                    let (clamped_x, clamped_y) =
                        clamp_center_to_viewport(focus_center_x, next_center_y, view_w, view_h);
                    next_center_y = clamped_y;
                    state = CameraState::LockedFocus {
                        focus_center_x: clamped_x,
                        focus_center_y: next_center_y,
                        focus_zoom,
                        cluster_end_ts: cluster_end_ts
                            .max(ts.saturating_add(config.scroll_idle_reset_ms.max(1))),
                    };
                }
            } else if last_scroll_ts.is_some_and(|last_ts| {
                event.ts().saturating_sub(last_ts) > config.scroll_idle_reset_ms.max(1)
            }) {
                scroll_session_start_ts = None;
                last_scroll_ts = None;
                scroll_accum_abs_dy = 0.0;
            }
            event_idx += 1;
        }

        if force_zoom_out_from_scroll {
            state = CameraState::FreeRoam;
            force_zoom_out_from_scroll = false;
        }

        while transition_idx < transitions.len() && transitions[transition_idx].start_ts <= ts {
            let focus = transitions[transition_idx];
            if let CameraState::LockedFocus {
                focus_center_x,
                focus_center_y,
                focus_zoom,
                cluster_end_ts,
            } = state
            {
                let viewport = current_viewport_rect(
                    spring_x.current_pos,
                    spring_y.current_pos,
                    spring_z.current_pos,
                    screen_width,
                    screen_height,
                    safe_aspect,
                );
                let safe_zone = inset_rect(viewport, config.safe_zone_margin_ratio);
                if safe_zone.contains(focus.focus_rect) {
                    state = CameraState::LockedFocus {
                        focus_center_x,
                        focus_center_y,
                        focus_zoom,
                        cluster_end_ts: cluster_end_ts
                            .max(focus.cluster_end_ts)
                            .max(focus.trigger_ts),
                    };
                    transition_idx += 1;
                    continue;
                }
            }

            state = CameraState::LockedFocus {
                focus_center_x: focus.center_x,
                focus_center_y: focus.center_y,
                focus_zoom: clamp_locked_zoom(focus.zoom, config),
                cluster_end_ts: focus.cluster_end_ts.max(focus.trigger_ts),
            };
            transition_idx += 1;
        }

        match state {
            CameraState::FreeRoam => {
                let cursor_nx = (cursor_x / width).clamp(0.0, 1.0);
                let cursor_ny = (cursor_y / height).clamp(0.0, 1.0);
                if breaches_dead_zone(cursor_nx, cursor_ny, config.dead_zone_ratio) {
                    let (view_w, view_h) = viewport_size_from_zoom(
                        config.free_roam_zoom,
                        screen_width,
                        screen_height,
                        safe_aspect,
                    );
                    let (clamped_x, clamped_y) =
                        clamp_center_to_viewport(cursor_nx, cursor_ny, view_w, view_h);
                    free_roam_center_x = clamped_x;
                    free_roam_center_y = clamped_y;
                }
                target_center_x = free_roam_center_x;
                target_center_y = free_roam_center_y;
                target_zoom = config.free_roam_zoom.max(1.0);
            }
            CameraState::LockedFocus {
                focus_center_x,
                focus_center_y,
                focus_zoom,
                cluster_end_ts,
            } => {
                let distance_px =
                    (cursor_x - focus_center_x * width).hypot(cursor_y - focus_center_y * height);
                let escape_threshold = width.hypot(height) * config.escape_distance_ratio.max(0.0);
                let timed_out =
                    ts > cluster_end_ts.saturating_add(config.lock_recent_window_ms.max(1));

                if timed_out || distance_px > escape_threshold {
                    state = CameraState::FreeRoam;
                    target_center_x = free_roam_center_x;
                    target_center_y = free_roam_center_y;
                    target_zoom = config.free_roam_zoom.max(1.0);
                } else {
                    let (next_focus_x, next_focus_y) = apply_locked_hard_edge_pan(
                        focus_center_x,
                        focus_center_y,
                        focus_zoom,
                        cursor_x,
                        cursor_y,
                        screen_width,
                        screen_height,
                        safe_aspect,
                        dt_seconds,
                        config,
                    );
                    state = CameraState::LockedFocus {
                        focus_center_x: next_focus_x,
                        focus_center_y: next_focus_y,
                        focus_zoom,
                        cluster_end_ts,
                    };
                    target_center_x = next_focus_x;
                    target_center_y = next_focus_y;
                    target_zoom = clamp_locked_zoom(focus_zoom, config);
                }
            }
        }

        spring_x.target_pos = target_center_x;
        spring_y.target_pos = target_center_y;
        spring_z.target_pos = target_zoom.max(1.0);
        let current_center_x = spring_x.tick(dt_seconds);
        let current_center_y = spring_y.tick(dt_seconds);
        let current_zoom = spring_z.tick(dt_seconds).max(1.0);

        samples.push(CameraSample {
            ts,
            state,
            center_x: current_center_x,
            center_y: current_center_y,
            zoom: current_zoom,
            target_center_x,
            target_center_y,
            target_zoom,
        });

        if ts >= duration_ms {
            break;
        }

        let next_ts = ts.saturating_add(safe_step_ms);
        ts = next_ts.min(duration_ms);
    }

    samples
}

pub fn build_smart_camera_segments(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    duration_ms: u64,
    output_aspect_ratio: f64,
    config: &SmartCameraConfig,
) -> Vec<ZoomSegment> {
    let samples = process_camera_targets(
        events,
        screen_width,
        screen_height,
        duration_ms,
        output_aspect_ratio,
        config,
    );
    if samples.is_empty() {
        return Vec::new();
    }

    let mut segments: Vec<ZoomSegment> = Vec::new();
    let mut start_idx: Option<usize> = None;

    for (idx, sample) in samples.iter().enumerate() {
        if sample.state.is_locked() {
            if start_idx.is_none() {
                start_idx = Some(idx);
            }
        } else if let Some(start) = start_idx.take() {
            push_locked_segment(
                &samples[start..idx],
                &mut segments,
                screen_width,
                screen_height,
                output_aspect_ratio,
                config,
            );
        }
    }

    if let Some(start) = start_idx {
        push_locked_segment(
            &samples[start..],
            &mut segments,
            screen_width,
            screen_height,
            output_aspect_ratio,
            config,
        );
    }

    for (idx, segment) in segments.iter_mut().enumerate() {
        segment.id = format!("auto-{}", idx + 1);
    }

    segments
}

fn push_locked_segment(
    locked_samples: &[CameraSample],
    output: &mut Vec<ZoomSegment>,
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
    config: &SmartCameraConfig,
) {
    if locked_samples.is_empty() {
        return;
    }

    let start_ts = locked_samples.first().map(|sample| sample.ts).unwrap_or(0);
    let end_ts = locked_samples
        .last()
        .map(|sample| sample.ts)
        .unwrap_or(start_ts)
        .max(start_ts.saturating_add(1));
    let first_rect = rect_from_center_zoom(
        locked_samples[0].target_center_x,
        locked_samples[0].target_center_y,
        locked_samples[0].target_zoom,
        screen_width,
        screen_height,
        output_aspect_ratio,
    );

    let mut target_points: Vec<TargetPoint> = Vec::new();
    let mut last_point_ts = 0u64;
    let sample_step = config
        .segment_target_sample_ms
        .max(config.fixed_dt_ms)
        .max(1);
    for (idx, sample) in locked_samples.iter().enumerate() {
        if idx == 0
            || idx + 1 == locked_samples.len()
            || sample.ts.saturating_sub(last_point_ts) >= sample_step
        {
            target_points.push(TargetPoint {
                ts: sample.ts,
                rect: rect_from_center_zoom(
                    sample.target_center_x,
                    sample.target_center_y,
                    sample.target_zoom,
                    screen_width,
                    screen_height,
                    output_aspect_ratio,
                ),
            });
            last_point_ts = sample.ts;
        }
    }

    output.push(ZoomSegment {
        id: String::new(),
        start_ts,
        end_ts,
        initial_rect: first_rect,
        target_points,
        spring: CameraSpring {
            mass: config.spring_mass.max(0.0001),
            stiffness: config.spring_stiffness.max(0.0001),
            damping: config.spring_damping.max(0.0),
        },
        pan_trajectory: Vec::new(),
        legacy_easing: None,
        mode: ZoomMode::FollowCursor,
        trigger: ZoomTrigger::AutoClick,
        is_auto: true,
    });
}

fn build_focus_transitions(
    events: &[InputEvent],
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
    velocities: &[VelocitySample],
    config: &SmartCameraConfig,
) -> Vec<FocusTransition> {
    let clicks = collect_focus_clicks(events);
    if clicks.is_empty() {
        return Vec::new();
    }

    let gated_clicks = filter_clicks_by_activation_window(
        &clicks,
        config.activation_window_ms.max(1),
        config.min_clicks_to_activate.max(1),
        config.click_cluster_gap_ms.max(1),
    );
    if gated_clicks.is_empty() {
        return Vec::new();
    }

    let clusters = cluster_focus_clicks(&gated_clicks, config.click_cluster_gap_ms.max(1));
    let mut transitions = Vec::with_capacity(clusters.len());
    let mut last_transition_start: Option<u64> = None;
    for cluster in clusters {
        let start_ts = choose_preroll_start(cluster.start_ts, velocities, config);
        if let Some(last_start) = last_transition_start {
            if start_ts.saturating_sub(last_start) < config.min_zoom_interval_ms.max(1) {
                continue;
            }
        }

        let (center_x, center_y, zoom) = semantic_target_from_cluster(
            cluster,
            screen_width,
            screen_height,
            output_aspect_ratio,
            config,
        );
        let zoom = clamp_locked_zoom(zoom, config);
        let free_roam_zoom = config.free_roam_zoom.max(1.0);
        // Ignore no-op transitions that keep full-frame context.
        if zoom <= free_roam_zoom + 0.001 {
            continue;
        }
        let focus_rect = focus_rect_from_cluster(cluster, screen_width, screen_height);
        let cluster_tail_bonus_ms = if cluster.click_count > 1 { 250 } else { 0 };
        let min_cluster_end = cluster
            .start_ts
            .saturating_add(config.min_lock_duration_ms.max(1))
            .saturating_add(cluster_tail_bonus_ms);
        transitions.push(FocusTransition {
            start_ts,
            trigger_ts: cluster.start_ts,
            cluster_end_ts: cluster.end_ts.max(min_cluster_end),
            center_x,
            center_y,
            zoom,
            focus_rect,
        });
        last_transition_start = Some(start_ts);
    }

    transitions.sort_by_key(|transition| transition.start_ts);
    transitions
}

fn collect_focus_clicks(events: &[InputEvent]) -> Vec<FocusClick> {
    let mut clicks = events
        .iter()
        .filter_map(|event| {
            if let InputEvent::Click {
                ts,
                x,
                y,
                ui_context,
                ..
            } = event
            {
                let bounds = ui_context
                    .as_ref()
                    .and_then(|ctx| ctx.bounding_rect.as_ref())
                    .and_then(rect_from_bounds);
                return Some(FocusClick {
                    ts: *ts,
                    x: *x,
                    y: *y,
                    bounds,
                });
            }
            None
        })
        .collect::<Vec<_>>();

    clicks.sort_by_key(|click| click.ts);
    clicks
}

fn filter_clicks_by_activation_window(
    clicks: &[FocusClick],
    window_ms: u64,
    min_clicks: usize,
    rapid_gap_ms: u64,
) -> Vec<FocusClick> {
    if clicks.len() < min_clicks.max(1) {
        return Vec::new();
    }

    let mut selected_indices = vec![false; clicks.len()];
    for (idx, click) in clicks.iter().enumerate() {
        let window_start = click.ts.saturating_sub(window_ms.max(1));
        let mut left = idx;
        while left > 0 && clicks[left - 1].ts >= window_start {
            left -= 1;
        }
        let count = idx + 1 - left;
        if count < min_clicks {
            continue;
        }

        selected_indices[idx] = true;
        if idx > 0 && click.ts.saturating_sub(clicks[idx - 1].ts) <= rapid_gap_ms.max(1) {
            selected_indices[idx - 1] = true;
        }
    }

    clicks
        .iter()
        .enumerate()
        .filter_map(|(idx, click)| selected_indices[idx].then_some(*click))
        .collect()
}

fn cluster_focus_clicks(clicks: &[FocusClick], gap_ms: u64) -> Vec<FocusCluster> {
    if clicks.is_empty() {
        return Vec::new();
    }

    let mut clusters: Vec<FocusCluster> = Vec::new();
    let mut current_start = clicks[0].ts;
    let mut current_end = clicks[0].ts;
    let mut current_sum_x = clicks[0].x;
    let mut current_sum_y = clicks[0].y;
    let mut current_count = 1usize;
    let mut current_anchor_x = clicks[0].x;
    let mut current_anchor_y = clicks[0].y;
    let mut current_bounds = clicks[0].bounds;

    for click in clicks.iter().skip(1) {
        let gap = click.ts.saturating_sub(current_end);
        if gap <= gap_ms {
            current_end = click.ts;
            current_sum_x += click.x;
            current_sum_y += click.y;
            current_count += 1;
            current_anchor_x = click.x;
            current_anchor_y = click.y;
            current_bounds = match (current_bounds, click.bounds) {
                (Some(left), Some(right)) => Some(left.union(right)),
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
            };
            continue;
        }

        clusters.push(FocusCluster {
            start_ts: current_start,
            end_ts: current_end,
            avg_x: current_sum_x / current_count as f64,
            avg_y: current_sum_y / current_count as f64,
            anchor_x: current_anchor_x,
            anchor_y: current_anchor_y,
            bounds: current_bounds,
            click_count: current_count,
        });

        current_start = click.ts;
        current_end = click.ts;
        current_sum_x = click.x;
        current_sum_y = click.y;
        current_count = 1;
        current_anchor_x = click.x;
        current_anchor_y = click.y;
        current_bounds = click.bounds;
    }

    clusters.push(FocusCluster {
        start_ts: current_start,
        end_ts: current_end,
        avg_x: current_sum_x / current_count as f64,
        avg_y: current_sum_y / current_count as f64,
        anchor_x: current_anchor_x,
        anchor_y: current_anchor_y,
        bounds: current_bounds,
        click_count: current_count,
    });

    clusters
}

fn collect_cursor_samples(events: &[InputEvent]) -> Vec<CursorSample> {
    let mut samples = events
        .iter()
        .filter_map(|event| match event {
            InputEvent::Move { ts, x, y }
            | InputEvent::Click { ts, x, y, .. }
            | InputEvent::MouseUp { ts, x, y, .. }
            | InputEvent::Scroll { ts, x, y, .. } => Some(CursorSample {
                ts: *ts,
                x: *x,
                y: *y,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    samples.sort_by_key(|sample| sample.ts);
    samples
}

fn build_velocity_samples(samples: &[CursorSample]) -> Vec<VelocitySample> {
    if samples.len() < 2 {
        return Vec::new();
    }

    let mut velocities = Vec::with_capacity(samples.len().saturating_sub(1));
    for pair in samples.windows(2) {
        let left = pair[0];
        let right = pair[1];
        let dt_ms = right.ts.saturating_sub(left.ts) as f64;
        if dt_ms <= 0.0 {
            continue;
        }
        let distance = (right.x - left.x).hypot(right.y - left.y);
        velocities.push(VelocitySample {
            ts: right.ts,
            speed_px_per_ms: distance / dt_ms,
        });
    }
    velocities
}

fn choose_preroll_start(
    click_ts: u64,
    velocities: &[VelocitySample],
    config: &SmartCameraConfig,
) -> u64 {
    let window_start = click_ts.saturating_sub(config.max_lookahead_ms.max(1));
    let in_window = velocities
        .iter()
        .copied()
        .filter(|sample| sample.ts >= window_start && sample.ts <= click_ts)
        .collect::<Vec<_>>();

    if in_window.is_empty() {
        return click_ts;
    }

    let threshold = config.velocity_threshold_px_per_ms.max(0.0);
    if let Some(last) = in_window.last() {
        if last.speed_px_per_ms > threshold {
            return click_ts;
        }
    }

    for pair in in_window.windows(2) {
        let left = pair[0];
        let right = pair[1];
        if left.speed_px_per_ms > threshold && right.speed_px_per_ms <= threshold {
            return right.ts.min(click_ts);
        }
    }

    in_window
        .first()
        .map(|sample| sample.ts.min(click_ts))
        .unwrap_or(click_ts)
}

fn semantic_target_from_cluster(
    cluster: FocusCluster,
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
    config: &SmartCameraConfig,
) -> (f64, f64, f64) {
    let width = screen_width.max(1) as f64;
    let height = screen_height.max(1) as f64;
    let safe_aspect = output_aspect_ratio.max(0.1);

    if let Some(bounds) = cluster.bounds {
        let mut padded_w = bounds.width * (1.0 + config.semantic_padding_ratio.max(0.0));
        let mut padded_h = bounds.height * (1.0 + config.semantic_padding_ratio.max(0.0));
        if padded_w <= 0.0 || padded_h <= 0.0 {
            return fallback_target(
                cluster.avg_x,
                cluster.avg_y,
                screen_width,
                screen_height,
                safe_aspect,
                config,
            );
        }

        let ratio = padded_w / padded_h.max(1.0);
        if ratio < safe_aspect {
            padded_w = padded_h * safe_aspect;
        } else {
            padded_h = padded_w / safe_aspect;
        }

        let width_norm = (padded_w / width).clamp(0.01, 1.0);
        let height_norm = (padded_h / height).clamp(0.01, 1.0);
        let zoom = clamp_locked_zoom(1.0 / width_norm.max(height_norm).max(0.0001), config);
        let center_x = bounds.center_x() / width;
        let center_y = bounds.center_y() / height;
        let (view_w, view_h) =
            viewport_size_from_zoom(zoom, screen_width, screen_height, safe_aspect);
        let (clamped_x, clamped_y) = clamp_center_to_viewport(center_x, center_y, view_w, view_h);
        return (clamped_x, clamped_y, zoom);
    }

    fallback_target(
        cluster.avg_x,
        cluster.avg_y,
        screen_width,
        screen_height,
        safe_aspect,
        config,
    )
}

fn fallback_target(
    click_x: f64,
    click_y: f64,
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
    config: &SmartCameraConfig,
) -> (f64, f64, f64) {
    let width = screen_width.max(1) as f64;
    let height = screen_height.max(1) as f64;
    let zoom = clamp_locked_zoom(config.fallback_zoom, config);
    let center_x = (click_x / width).clamp(0.0, 1.0);
    let center_y = (click_y / height).clamp(0.0, 1.0);
    let (view_w, view_h) =
        viewport_size_from_zoom(zoom, screen_width, screen_height, output_aspect_ratio);
    let (clamped_x, clamped_y) = clamp_center_to_viewport(center_x, center_y, view_w, view_h);
    (clamped_x, clamped_y, zoom)
}

fn clamp_locked_zoom(zoom: f64, config: &SmartCameraConfig) -> f64 {
    zoom.max(1.0).min(config.max_zoom_limit.max(1.0))
}

#[allow(clippy::too_many_arguments)]
fn apply_locked_hard_edge_pan(
    focus_center_x: f64,
    focus_center_y: f64,
    focus_zoom: f64,
    cursor_x: f64,
    cursor_y: f64,
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
    dt_seconds: f64,
    config: &SmartCameraConfig,
) -> (f64, f64) {
    let width = screen_width.max(1) as f64;
    let height = screen_height.max(1) as f64;
    let cursor_nx = (cursor_x / width).clamp(0.0, 1.0);
    let cursor_ny = (cursor_y / height).clamp(0.0, 1.0);

    let (view_w, view_h) = viewport_size_from_zoom(
        clamp_locked_zoom(focus_zoom, config),
        screen_width,
        screen_height,
        output_aspect_ratio,
    );
    let hard_edge_ratio = config.hard_edge_ratio.clamp(0.05, 0.95);
    let hard_edge_x = (view_w * 0.5 * hard_edge_ratio).max(1.0 / width);
    let hard_edge_y = (view_h * 0.5 * hard_edge_ratio).max(1.0 / height);
    let max_step_x = (config.hard_edge_pan_speed_px_per_s.max(0.0) * dt_seconds.max(0.0)) / width;
    let max_step_y = (config.hard_edge_pan_speed_px_per_s.max(0.0) * dt_seconds.max(0.0)) / height;

    let mut next_center_x = focus_center_x;
    let mut next_center_y = focus_center_y;
    let offset_x = cursor_nx - focus_center_x;
    let offset_y = cursor_ny - focus_center_y;

    if offset_x.abs() > hard_edge_x {
        next_center_x += offset_x.signum() * max_step_x;
    }
    if offset_y.abs() > hard_edge_y {
        next_center_y += offset_y.signum() * max_step_y;
    }

    clamp_center_to_viewport(next_center_x, next_center_y, view_w, view_h)
}

fn focus_rect_from_cluster(
    cluster: FocusCluster,
    screen_width: u32,
    screen_height: u32,
) -> RectNorm {
    if let Some(bounds) = cluster.bounds {
        return normalize_rect_px(bounds, screen_width, screen_height);
    }
    normalize_point_rect(
        cluster.anchor_x,
        cluster.anchor_y,
        screen_width,
        screen_height,
    )
}

fn normalize_rect_px(bounds: RectPx, screen_width: u32, screen_height: u32) -> RectNorm {
    let width = screen_width.max(1) as f64;
    let height = screen_height.max(1) as f64;
    let min_w = 1.0 / width;
    let min_h = 1.0 / height;

    let left = (bounds.x / width).clamp(0.0, 1.0);
    let top = (bounds.y / height).clamp(0.0, 1.0);
    let right = ((bounds.x + bounds.width) / width).clamp(0.0, 1.0);
    let bottom = ((bounds.y + bounds.height) / height).clamp(0.0, 1.0);

    let x = left.min(right);
    let y = top.min(bottom);
    let width_norm = (right - left).abs().max(min_w).min(1.0 - x);
    let height_norm = (bottom - top).abs().max(min_h).min(1.0 - y);

    RectNorm {
        x,
        y,
        width: width_norm,
        height: height_norm,
    }
}

fn normalize_point_rect(x: f64, y: f64, screen_width: u32, screen_height: u32) -> RectNorm {
    let width = screen_width.max(1) as f64;
    let height = screen_height.max(1) as f64;
    let point_x = (x / width).clamp(0.0, 1.0);
    let point_y = (y / height).clamp(0.0, 1.0);
    let min_w = 1.0 / width;
    let min_h = 1.0 / height;
    let rect_x = (point_x - min_w * 0.5).clamp(0.0, 1.0 - min_w);
    let rect_y = (point_y - min_h * 0.5).clamp(0.0, 1.0 - min_h);
    RectNorm {
        x: rect_x,
        y: rect_y,
        width: min_w,
        height: min_h,
    }
}

fn current_viewport_rect(
    center_x: f64,
    center_y: f64,
    zoom: f64,
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
) -> RectNorm {
    let rect = rect_from_center_zoom(
        center_x,
        center_y,
        zoom,
        screen_width,
        screen_height,
        output_aspect_ratio,
    );
    RectNorm {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

fn inset_rect(rect: RectNorm, margin_ratio: f64) -> RectNorm {
    let safe_ratio = margin_ratio.clamp(0.0, 0.49);
    let inset_x = rect.width * safe_ratio;
    let inset_y = rect.height * safe_ratio;
    let width = (rect.width - inset_x * 2.0).max(1e-4);
    let height = (rect.height - inset_y * 2.0).max(1e-4);
    RectNorm {
        x: (rect.x + inset_x).clamp(0.0, 1.0 - width),
        y: (rect.y + inset_y).clamp(0.0, 1.0 - height),
        width,
        height,
    }
}

fn viewport_size_from_zoom(
    zoom: f64,
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
) -> (f64, f64) {
    let safe_zoom = zoom.max(1.0);
    let screen_aspect = screen_width.max(1) as f64 / screen_height.max(1) as f64;
    let safe_output_aspect = output_aspect_ratio.max(0.1);

    let mut width_norm = 1.0 / safe_zoom;
    let mut height_norm = (width_norm * screen_aspect) / safe_output_aspect;

    if height_norm > 1.0 {
        height_norm = 1.0 / safe_zoom;
        width_norm = (height_norm * safe_output_aspect) / screen_aspect.max(0.1);
    }

    (width_norm.clamp(0.01, 1.0), height_norm.clamp(0.01, 1.0))
}

fn clamp_center_to_viewport(center_x: f64, center_y: f64, view_w: f64, view_h: f64) -> (f64, f64) {
    let half_w = (view_w * 0.5).clamp(0.0, 0.5);
    let half_h = (view_h * 0.5).clamp(0.0, 0.5);
    (
        center_x.clamp(half_w, 1.0 - half_w),
        center_y.clamp(half_h, 1.0 - half_h),
    )
}

fn rect_from_center_zoom(
    center_x: f64,
    center_y: f64,
    zoom: f64,
    screen_width: u32,
    screen_height: u32,
    output_aspect_ratio: f64,
) -> NormalizedRect {
    let (view_w, view_h) =
        viewport_size_from_zoom(zoom, screen_width, screen_height, output_aspect_ratio);
    let (clamped_center_x, clamped_center_y) =
        clamp_center_to_viewport(center_x, center_y, view_w, view_h);
    NormalizedRect {
        x: (clamped_center_x - view_w * 0.5).clamp(0.0, 1.0 - view_w),
        y: (clamped_center_y - view_h * 0.5).clamp(0.0, 1.0 - view_h),
        width: view_w,
        height: view_h,
    }
}

fn breaches_dead_zone(cursor_nx: f64, cursor_ny: f64, dead_zone_ratio: f64) -> bool {
    let half = dead_zone_ratio.clamp(0.0, 0.95) * 0.5;
    cursor_nx < 0.5 - half
        || cursor_nx > 0.5 + half
        || cursor_ny < 0.5 - half
        || cursor_ny > 0.5 + half
}

fn normalize_scroll_delta(delta: f64) -> f64 {
    if delta.abs() >= 100.0 {
        (delta / 120.0).clamp(-6.0, 6.0)
    } else {
        delta.clamp(-6.0, 6.0)
    }
}

fn rect_from_bounds(bounds: &BoundingRect) -> Option<RectPx> {
    if bounds.width == 0 || bounds.height == 0 {
        return None;
    }
    Some(RectPx {
        x: bounds.x as f64,
        y: bounds.y as f64,
        width: bounds.width as f64,
        height: bounds.height as f64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::events::{MouseButton, ScrollDelta, UiContext};

    fn click_with_bounds(ts: u64, x: f64, y: f64, rect: Option<BoundingRect>) -> InputEvent {
        InputEvent::Click {
            ts,
            x,
            y,
            button: MouseButton::Left,
            ui_context: Some(UiContext {
                app_name: Some("app".to_string()),
                control_name: Some("btn".to_string()),
                bounding_rect: rect,
            }),
        }
    }

    #[test]
    fn spring_tick_converges_to_target() {
        let mut spring = Spring::new(
            0.0,
            1.0,
            0.0,
            170.0,
            Spring::critical_damping(170.0, 1.0),
            1.0,
        );
        for _ in 0..240 {
            spring.tick(1.0 / 120.0);
        }
        assert!((spring.current_pos - 1.0).abs() < 0.01);
    }

    #[test]
    fn semantic_target_uses_bounds_center_and_padding() {
        let events = vec![click_with_bounds(
            1_000,
            300.0,
            300.0,
            Some(BoundingRect {
                x: 400,
                y: 200,
                width: 240,
                height: 120,
            }),
        )];

        let cfg = SmartCameraConfig {
            escape_distance_ratio: 0.6,
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 2_000, 16.0 / 9.0, &cfg);
        assert!(!track.is_empty());
        let locked = track
            .iter()
            .find(|sample| sample.state.is_locked())
            .expect("expected locked sample");
        assert!((locked.target_center_x - ((400.0 + 120.0) / 1_920.0)).abs() < 0.01);
        assert!((locked.target_center_y - ((200.0 + 60.0) / 1_080.0)).abs() < 0.01);
        assert!(locked.target_zoom > 2.0);
    }

    #[test]
    fn fallback_uses_fixed_two_x_zoom() {
        let events = vec![click_with_bounds(1_000, 960.0, 540.0, None)];
        let cfg = SmartCameraConfig {
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 2_000, 16.0 / 9.0, &cfg);
        let locked = track
            .iter()
            .find(|sample| sample.state.is_locked())
            .expect("expected locked sample");
        assert!((locked.target_zoom - 2.0).abs() < 0.001);
    }

    #[test]
    fn tiny_bounds_zoom_is_clamped_to_max_limit() {
        let events = vec![click_with_bounds(
            1_000,
            960.0,
            540.0,
            Some(BoundingRect {
                x: 952,
                y: 532,
                width: 16,
                height: 16,
            }),
        )];
        let cfg = SmartCameraConfig {
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 2_000, 16.0 / 9.0, &cfg);
        let locked = track
            .iter()
            .find(|sample| sample.state.is_locked())
            .expect("expected locked sample");
        assert!(locked.target_zoom <= cfg.max_zoom_limit + 0.000_1);
        assert!((locked.target_zoom - cfg.max_zoom_limit).abs() < 0.001);
    }

    #[test]
    fn fullscreen_focus_is_ignored_before_real_zoom_transition() {
        let events = vec![
            click_with_bounds(
                1_000,
                960.0,
                540.0,
                Some(BoundingRect {
                    x: 0,
                    y: 0,
                    width: 1_920,
                    height: 1_080,
                }),
            ),
            click_with_bounds(
                2_000,
                120.0,
                200.0,
                Some(BoundingRect {
                    x: 80,
                    y: 160,
                    width: 120,
                    height: 80,
                }),
            ),
        ];
        let cfg = SmartCameraConfig {
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let segments = build_smart_camera_segments(&events, 1_920, 1_080, 3_000, 16.0 / 9.0, &cfg);
        assert_eq!(segments.len(), 1, "expected only one effective auto-zoom segment");
        let segment = &segments[0];
        assert!(
            segment.start_ts >= 1_600,
            "segment should start near second click preroll window, got {}",
            segment.start_ts
        );
        assert!(
            segment.target_points.iter().any(|point| point.rect.width < 0.99),
            "segment must contain actual zoom-in target points"
        );
        assert_eq!(segment.mode, ZoomMode::FollowCursor);
    }

    #[test]
    fn single_click_is_ignored_by_default_activation_rule() {
        let events = vec![click_with_bounds(1_000, 960.0, 540.0, None)];
        let cfg = SmartCameraConfig::default();
        let track = process_camera_targets(&events, 1_920, 1_080, 2_000, 16.0 / 9.0, &cfg);
        assert!(
            track.iter().all(|sample| !sample.state.is_locked()),
            "single click must not activate zoom when min_clicks_to_activate=2"
        );
    }

    #[test]
    fn two_clicks_within_activation_window_trigger_zoom() {
        let events = vec![
            click_with_bounds(1_000, 600.0, 400.0, None),
            click_with_bounds(2_100, 620.0, 410.0, None),
        ];
        let cfg = SmartCameraConfig::default();
        let track = process_camera_targets(&events, 1_920, 1_080, 3_500, 16.0 / 9.0, &cfg);
        assert!(
            track.iter().any(|sample| sample.state.is_locked()),
            "expected locked samples for 2 clicks inside 3s activation window"
        );
    }

    #[test]
    fn click_inside_safe_zone_keeps_locked_target() {
        let events = vec![
            click_with_bounds(
                1_000,
                960.0,
                500.0,
                Some(BoundingRect {
                    x: 760,
                    y: 390,
                    width: 400,
                    height: 220,
                }),
            ),
            click_with_bounds(
                1_700,
                960.0,
                510.0,
                Some(BoundingRect {
                    x: 920,
                    y: 490,
                    width: 80,
                    height: 40,
                }),
            ),
        ];
        let cfg = SmartCameraConfig {
            min_zoom_interval_ms: 1,
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 2_400, 16.0 / 9.0, &cfg);

        let before = track
            .iter()
            .find(|sample| sample.ts >= 1_500 && sample.state.is_locked())
            .expect("missing locked sample before second click");
        let after = track
            .iter()
            .find(|sample| sample.ts >= 1_760 && sample.state.is_locked())
            .expect("missing locked sample after second click");

        assert!((after.target_center_x - before.target_center_x).abs() < 1e-6);
        assert!((after.target_center_y - before.target_center_y).abs() < 1e-6);
        assert!((after.target_zoom - before.target_zoom).abs() < 1e-6);
    }

    #[test]
    fn click_outside_safe_zone_retargets_locked_focus() {
        let events = vec![
            click_with_bounds(
                1_000,
                960.0,
                500.0,
                Some(BoundingRect {
                    x: 760,
                    y: 390,
                    width: 400,
                    height: 220,
                }),
            ),
            click_with_bounds(
                1_700,
                1_560.0,
                780.0,
                Some(BoundingRect {
                    x: 1_500,
                    y: 740,
                    width: 120,
                    height: 80,
                }),
            ),
        ];
        let cfg = SmartCameraConfig {
            min_zoom_interval_ms: 1,
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 2_400, 16.0 / 9.0, &cfg);

        let before = track
            .iter()
            .find(|sample| sample.ts >= 1_500 && sample.state.is_locked())
            .expect("missing locked sample before second click");
        let after = track
            .iter()
            .find(|sample| sample.ts >= 1_760 && sample.state.is_locked())
            .expect("missing locked sample after second click");

        assert!(
            (after.target_center_x - before.target_center_x).abs() > 0.01
                || (after.target_center_y - before.target_center_y).abs() > 0.01
                || (after.target_zoom - before.target_zoom).abs() > 0.01
        );
    }

    #[test]
    fn high_velocity_click_disables_preroll() {
        let events = vec![
            InputEvent::Move {
                ts: 900,
                x: 100.0,
                y: 100.0,
            },
            InputEvent::Move {
                ts: 980,
                x: 1_500.0,
                y: 100.0,
            },
            click_with_bounds(1_000, 1_520.0, 110.0, None),
        ];
        let cfg = SmartCameraConfig {
            velocity_threshold_px_per_ms: 0.5,
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 1_600, 16.0 / 9.0, &cfg);
        let first_locked_ts = track
            .iter()
            .find(|sample| sample.state.is_locked())
            .map(|sample| sample.ts)
            .expect("expected locked sample");
        assert!(first_locked_ts >= 1_000);
    }

    #[test]
    fn low_velocity_deceleration_enables_preroll() {
        let events = vec![
            InputEvent::Move {
                ts: 500,
                x: 200.0,
                y: 200.0,
            },
            InputEvent::Move {
                ts: 700,
                x: 800.0,
                y: 200.0,
            },
            InputEvent::Move {
                ts: 900,
                x: 860.0,
                y: 200.0,
            },
            click_with_bounds(1_000, 870.0, 210.0, None),
        ];
        let cfg = SmartCameraConfig {
            velocity_threshold_px_per_ms: 1.0,
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 1_600, 16.0 / 9.0, &cfg);
        let first_locked_ts = track
            .iter()
            .find(|sample| sample.state.is_locked())
            .map(|sample| sample.ts)
            .expect("expected locked sample");
        assert!(first_locked_ts < 1_000);
        assert!(1_000 - first_locked_ts <= 400);
    }

    #[test]
    fn locked_focus_scroll_and_escape_work() {
        let events = vec![
            click_with_bounds(
                1_000,
                300.0,
                300.0,
                Some(BoundingRect {
                    x: 220,
                    y: 200,
                    width: 160,
                    height: 120,
                }),
            ),
            InputEvent::Scroll {
                ts: 1_200,
                x: 300.0,
                y: 300.0,
                delta: ScrollDelta {
                    dx: 0.0,
                    dy: -120.0,
                },
            },
            InputEvent::Move {
                ts: 1_700,
                x: 1_900.0,
                y: 1_060.0,
            },
        ];

        let cfg = SmartCameraConfig {
            escape_distance_ratio: 0.6,
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 2_400, 16.0 / 9.0, &cfg);

        let before_scroll = track
            .iter()
            .find(|sample| sample.ts >= 1_050 && sample.state.is_locked())
            .expect("missing locked sample before scroll");
        let after_scroll = track
            .iter()
            .find(|sample| sample.ts >= 1_250 && sample.state.is_locked())
            .expect("missing locked sample after scroll");
        assert!(after_scroll.target_center_y > before_scroll.target_center_y);

        let escaped = track
            .iter()
            .find(|sample| sample.ts >= 1_800)
            .expect("missing sample after escape");
        assert!(
            !escaped.state.is_locked(),
            "expected FREE_ROAM after escape, got {:?}",
            escaped.state
        );
    }

    #[test]
    fn long_or_large_scroll_exits_zoom_to_full_context() {
        let events = vec![
            click_with_bounds(
                1_000,
                600.0,
                300.0,
                Some(BoundingRect {
                    x: 520,
                    y: 220,
                    width: 180,
                    height: 120,
                }),
            ),
            click_with_bounds(
                1_350,
                620.0,
                320.0,
                Some(BoundingRect {
                    x: 540,
                    y: 240,
                    width: 180,
                    height: 120,
                }),
            ),
            InputEvent::Scroll {
                ts: 1_600,
                x: 620.0,
                y: 320.0,
                delta: ScrollDelta {
                    dx: 0.0,
                    dy: -700.0,
                },
            },
            InputEvent::Scroll {
                ts: 1_700,
                x: 620.0,
                y: 320.0,
                delta: ScrollDelta {
                    dx: 0.0,
                    dy: -700.0,
                },
            },
            InputEvent::Scroll {
                ts: 1_780,
                x: 620.0,
                y: 320.0,
                delta: ScrollDelta {
                    dx: 0.0,
                    dy: -500.0,
                },
            },
        ];
        let cfg = SmartCameraConfig {
            min_clicks_to_activate: 1,
            ..SmartCameraConfig::default()
        };
        let track = process_camera_targets(&events, 1_920, 1_080, 2_400, 16.0 / 9.0, &cfg);

        let before = track
            .iter()
            .find(|sample| sample.ts >= 1_650 && sample.state.is_locked())
            .expect("expected locked sample before global scroll break");
        let after = track
            .iter()
            .find(|sample| sample.ts >= 1_900)
            .expect("missing sample after global scroll break");
        assert!(before.state.is_locked());
        assert!(
            !after.state.is_locked(),
            "expected FREE_ROAM after global scroll, got {:?}",
            after.state
        );
    }
}
