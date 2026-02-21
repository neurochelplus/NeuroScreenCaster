# QA Checklist (Stage 7)

## Automated smoke check

Run the latest project smoke checks:

```bash
npm run qa:smoke
```

Run against a specific project:

```bash
npm run qa:smoke -- --project "C:\\Users\\<you>\\Videos\\NeuroScreenCaster\\<id>\\project.json"
```

Optional export file validation:

```bash
npm run qa:smoke -- --check-export
```

What it checks:

- `project.json` and `events.json` schema/version sanity.
- `project.id` vs `events.recordingId` consistency.
- Source video probe (duration, resolution, fps) via FFmpeg.
- Duration sync (`project.durationMs` vs source video duration).
- Telemetry timeline and timestamp ordering sanity.
- Cursor coordinate bounds relative to `screenWidth/screenHeight`.
- `scaleFactor` validity and potential High-DPI logical-coordinate warning.
- Zoom segment timing and `targetRect` bounds.

## Manual E2E checklist (Record -> Edit -> Export)

1. Record a 10-20s clip with clicks near all corners and center.
2. Open Edit screen and verify:
   - Video loads.
   - Cursor path and click points are visually aligned.
   - Auto-zoom segments are placed at expected timestamps.
3. Adjust one zoom segment (move/resize on timeline), save project.
4. Export MP4 (`h264`, 1080p, 30fps).
5. Verify exported video:
   - Zoom transitions are smooth.
   - Cursor overlay is visible, smooth, and click-aligned.
   - Duration is close to source (no major drift).

## High-DPI matrix

Run the same scenario at Windows scaling:

- 100%
- 125%
- 150%

At each scale, verify:

- Cursor and zoom target locations in export match preview.
- No consistent drift toward corners.
- `events.json` contains realistic `scaleFactor` (>1.0 for 125/150%).

## Release preflight

1. `npm run build`
2. `cargo test` in `src-tauri`
3. `npm run qa:smoke -- --check-export`
4. One manual E2E pass on a fresh recording
