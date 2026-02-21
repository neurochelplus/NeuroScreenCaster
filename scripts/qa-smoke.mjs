#!/usr/bin/env node

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const PROJECT_SCHEMA_VERSION = 1;
const EVENTS_SCHEMA_VERSION = 1;

function usage() {
  console.log(
    [
      "Usage:",
      "  node scripts/qa-smoke.mjs [--latest] [--project <path>] [--root <projectsRoot>] [--check-export]",
      "",
      "Options:",
      "  --latest             Force auto-select latest project from root (default).",
      "  --project <path>     Path to project directory or project.json.",
      "  --root <path>        Projects root (default: ~/Videos/NeuroScreenCaster).",
      "  --check-export       Validate latest export-*.mp4 in project folder.",
      "  --help               Show this help.",
    ].join("\n")
  );
}

function parseArgs(argv) {
  const args = {
    latest: false,
    checkExport: false,
    projectPath: null,
    rootPath: path.join(os.homedir(), "Videos", "NeuroScreenCaster"),
  };

  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (token === "--help" || token === "-h") {
      usage();
      process.exit(0);
    }
    if (token === "--latest") {
      args.latest = true;
      continue;
    }
    if (token === "--check-export") {
      args.checkExport = true;
      continue;
    }
    if (token === "--project") {
      const value = argv[index + 1];
      if (!value) {
        throw new Error("--project requires a value");
      }
      args.projectPath = value;
      index += 1;
      continue;
    }
    if (token === "--root") {
      const value = argv[index + 1];
      if (!value) {
        throw new Error("--root requires a value");
      }
      args.rootPath = value;
      index += 1;
      continue;
    }

    throw new Error(`Unknown argument: ${token}`);
  }

  return args;
}

function readJson(filePath) {
  const raw = fs.readFileSync(filePath, "utf8");
  return JSON.parse(raw);
}

function isFiniteNumber(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function resolveProjectFilePath(args) {
  if (args.projectPath) {
    const candidate = path.resolve(args.projectPath);
    const stat = safeStat(candidate);
    if (!stat) {
      throw new Error(`Project path does not exist: ${candidate}`);
    }
    return stat.isDirectory() ? path.join(candidate, "project.json") : candidate;
  }

  const root = path.resolve(args.rootPath);
  const rootStat = safeStat(root);
  if (!rootStat || !rootStat.isDirectory()) {
    throw new Error(`Projects root not found: ${root}`);
  }

  const projectCandidates = fs
    .readdirSync(root, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => path.join(root, entry.name, "project.json"))
    .filter((projectFile) => fs.existsSync(projectFile))
    .map((projectFile) => ({
      projectFile,
      mtimeMs: safeStat(projectFile)?.mtimeMs ?? 0,
    }))
    .sort((left, right) => right.mtimeMs - left.mtimeMs);

  if (projectCandidates.length === 0) {
    throw new Error(`No project.json files found in: ${root}`);
  }

  return path.resolve(projectCandidates[0].projectFile);
}

function safeStat(targetPath) {
  try {
    return fs.statSync(targetPath);
  } catch {
    return null;
  }
}

function resolveFfmpegBinary() {
  const sidecar = path.resolve(
    process.cwd(),
    "src-tauri",
    "binaries",
    "ffmpeg-x86_64-pc-windows-msvc.exe"
  );
  return fs.existsSync(sidecar) ? sidecar : "ffmpeg";
}

function probeVideo(videoPath) {
  const ffmpeg = resolveFfmpegBinary();
  const probe = spawnSync(ffmpeg, ["-i", videoPath], { encoding: "utf8" });

  if (probe.error) {
    throw new Error(`Failed to run ffmpeg for probe: ${probe.error.message}`);
  }

  const output = `${probe.stdout ?? ""}\n${probe.stderr ?? ""}`;
  const durationMs = parseDurationMs(output);
  const size = parseVideoSize(output);
  const fps = parseFps(output);

  return {
    ffmpeg,
    durationMs,
    width: size?.width ?? null,
    height: size?.height ?? null,
    fps,
  };
}

function parseDurationMs(text) {
  const match = text.match(/Duration:\s*(\d+):(\d+):(\d+(?:\.\d+)?)/i);
  if (!match) {
    return null;
  }
  const hours = Number(match[1]);
  const minutes = Number(match[2]);
  const seconds = Number(match[3]);
  if (![hours, minutes, seconds].every(Number.isFinite)) {
    return null;
  }
  return Math.round((hours * 3600 + minutes * 60 + seconds) * 1000);
}

function parseVideoSize(text) {
  const match = text.match(/Video:.*?(\d{2,5})x(\d{2,5})/i);
  if (!match) {
    return null;
  }
  const width = Number(match[1]);
  const height = Number(match[2]);
  if (!Number.isFinite(width) || !Number.isFinite(height)) {
    return null;
  }
  return { width, height };
}

function parseFps(text) {
  const matches = [...text.matchAll(/(\d+(?:\.\d+)?)\s+fps/gi)];
  if (matches.length === 0) {
    return null;
  }
  const value = Number(matches[0][1]);
  return Number.isFinite(value) ? value : null;
}

function formatMs(ms) {
  const totalMs = Math.max(0, Math.round(ms));
  const totalSec = Math.floor(totalMs / 1000);
  const min = Math.floor(totalSec / 60)
    .toString()
    .padStart(2, "0");
  const sec = (totalSec % 60).toString().padStart(2, "0");
  const milli = (totalMs % 1000).toString().padStart(3, "0");
  return `${min}:${sec}.${milli}`;
}

function findLatestExport(projectDir) {
  const exports = fs
    .readdirSync(projectDir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && /^export-.*\.mp4$/i.test(entry.name))
    .map((entry) => path.join(projectDir, entry.name))
    .map((filePath) => ({ filePath, mtimeMs: safeStat(filePath)?.mtimeMs ?? 0 }))
    .sort((left, right) => right.mtimeMs - left.mtimeMs);

  return exports[0]?.filePath ?? null;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const projectFilePath = resolveProjectFilePath(args);
  const projectDir = path.dirname(projectFilePath);
  const project = readJson(projectFilePath);

  const issues = [];
  const warnings = [];
  const infos = [];

  const fail = (message) => issues.push(message);
  const warn = (message) => warnings.push(message);
  const info = (message) => infos.push(message);

  if (!isFiniteNumber(project.schemaVersion)) {
    fail("project.schemaVersion is missing or invalid");
  } else if (project.schemaVersion !== PROJECT_SCHEMA_VERSION) {
    fail(
      `project.schemaVersion mismatch: expected ${PROJECT_SCHEMA_VERSION}, got ${project.schemaVersion}`
    );
  }

  if (typeof project.id !== "string" || project.id.trim() === "") {
    fail("project.id is missing");
  }
  if (!isFiniteNumber(project.durationMs) || project.durationMs <= 0) {
    fail("project.durationMs is missing or invalid");
  }

  const eventsPath = path.resolve(projectDir, String(project.eventsPath ?? "events.json"));
  if (!fs.existsSync(eventsPath)) {
    fail(`events file not found: ${eventsPath}`);
  }

  const videoPath = path.resolve(projectDir, String(project.videoPath ?? "raw.mp4"));
  if (!fs.existsSync(videoPath)) {
    fail(`source video not found: ${videoPath}`);
  }

  let events = null;
  if (fs.existsSync(eventsPath)) {
    events = readJson(eventsPath);
    if (!isFiniteNumber(events.schemaVersion)) {
      fail("events.schemaVersion is missing or invalid");
    } else if (events.schemaVersion !== EVENTS_SCHEMA_VERSION) {
      fail(
        `events.schemaVersion mismatch: expected ${EVENTS_SCHEMA_VERSION}, got ${events.schemaVersion}`
      );
    }

    if (typeof events.recordingId !== "string" || events.recordingId.trim() === "") {
      fail("events.recordingId is missing");
    } else if (project.id && events.recordingId !== project.id) {
      fail(`recording id mismatch: project.id=${project.id}, events.recordingId=${events.recordingId}`);
    }

    if (!Array.isArray(events.events)) {
      fail("events.events must be an array");
    }
  }

  let videoProbe = null;
  if (fs.existsSync(videoPath)) {
    videoProbe = probeVideo(videoPath);
    info(`ffmpeg probe binary: ${videoProbe.ffmpeg}`);
    if (!isFiniteNumber(videoProbe.durationMs)) {
      fail("Failed to parse source video duration from ffmpeg output");
    } else {
      info(`source duration: ${formatMs(videoProbe.durationMs)}`);
    }

    if (isFiniteNumber(videoProbe.width) && isFiniteNumber(videoProbe.height)) {
      info(`source resolution: ${videoProbe.width}x${videoProbe.height}`);
      if (
        isFiniteNumber(project.videoWidth) &&
        isFiniteNumber(project.videoHeight) &&
        (project.videoWidth !== videoProbe.width || project.videoHeight !== videoProbe.height)
      ) {
        warn(
          `project resolution (${project.videoWidth}x${project.videoHeight}) differs from source (${videoProbe.width}x${videoProbe.height})`
        );
      }
    } else {
      warn("Failed to parse source video resolution from ffmpeg output");
    }

    if (isFiniteNumber(videoProbe.fps)) {
      info(`source fps: ${videoProbe.fps.toFixed(2)}`);
    }
  }

  if (isFiniteNumber(project.durationMs) && isFiniteNumber(videoProbe?.durationMs)) {
    const delta = Math.abs(project.durationMs - videoProbe.durationMs);
    const baseline = Math.max(project.durationMs, videoProbe.durationMs);
    const driftRatio = baseline > 0 ? delta / baseline : 0;
    if (driftRatio > 0.25) {
      fail(
        `critical duration mismatch: project=${formatMs(project.durationMs)} source=${formatMs(
          videoProbe.durationMs
        )} delta=${delta}ms (${(driftRatio * 100).toFixed(1)}%)`
      );
    } else if (driftRatio > 0.08) {
      warn(
        `duration drift detected: project=${formatMs(project.durationMs)} source=${formatMs(
          videoProbe.durationMs
        )} delta=${delta}ms (${(driftRatio * 100).toFixed(1)}%)`
      );
    } else {
      info(`duration delta: ${delta}ms (${(driftRatio * 100).toFixed(1)}%)`);
    }
  }

  if (events && Array.isArray(events.events)) {
    const timestamps = events.events
      .map((event) => Number(event.ts))
      .filter((value) => Number.isFinite(value));
    if (timestamps.length === 0) {
      warn("No telemetry events found in events.json");
    } else {
      const maxTs = Math.max(...timestamps);
      const minTs = Math.min(...timestamps);
      info(`events: ${timestamps.length}, range ${formatMs(minTs)}..${formatMs(maxTs)}`);

      if (minTs < 0) {
        fail(`events contain negative timestamps (min=${minTs})`);
      }

      if (isFiniteNumber(project.durationMs) && maxTs > project.durationMs + 1000) {
        fail(
          `event timeline exceeds project duration: lastEventTs=${maxTs}ms, project.durationMs=${project.durationMs}ms`
        );
      }

      let disorderCount = 0;
      let previous = -Infinity;
      for (const ts of timestamps) {
        if (ts < previous) {
          disorderCount += 1;
        }
        previous = ts;
      }
      if (disorderCount > 0) {
        warn(`event order is not strictly monotonic (out-of-order samples: ${disorderCount})`);
      }
    }

    const coordEvents = events.events.filter(
      (event) => isFiniteNumber(event.x) && isFiniteNumber(event.y)
    );
    if (coordEvents.length === 0) {
      warn("No pointer coordinate events found");
    } else {
      const xs = coordEvents.map((event) => Number(event.x));
      const ys = coordEvents.map((event) => Number(event.y));
      const minX = Math.min(...xs);
      const maxX = Math.max(...xs);
      const minY = Math.min(...ys);
      const maxY = Math.max(...ys);
      info(`cursor range x=${minX.toFixed(1)}..${maxX.toFixed(1)} y=${minY.toFixed(1)}..${maxY.toFixed(1)}`);

      const screenWidth = Number(events.screenWidth);
      const screenHeight = Number(events.screenHeight);
      const scaleFactor = Number(events.scaleFactor);

      if (!isFiniteNumber(screenWidth) || screenWidth <= 0 || !isFiniteNumber(screenHeight) || screenHeight <= 0) {
        fail("events.screenWidth/screenHeight are missing or invalid");
      } else {
        if (minX < -2 || minY < -2) {
          fail(`cursor coordinates contain negative values below tolerance: minX=${minX}, minY=${minY}`);
        }

        const fitsPhysical = maxX <= screenWidth * 1.05 && maxY <= screenHeight * 1.05;
        const validScale = isFiniteNumber(scaleFactor) && scaleFactor > 0 && scaleFactor <= 4;
        if (!validScale) {
          fail(`events.scaleFactor is invalid: ${String(events.scaleFactor)}`);
        }

        const fitsScaled =
          validScale &&
          scaleFactor > 1.0 &&
          maxX * scaleFactor <= screenWidth * 1.05 &&
          maxY * scaleFactor <= screenHeight * 1.05;

        if (!fitsPhysical && !fitsScaled) {
          fail(
            `cursor coordinates exceed screen bounds: maxX=${maxX}, maxY=${maxY}, screen=${screenWidth}x${screenHeight}, scaleFactor=${scaleFactor}`
          );
        } else if (!fitsPhysical && fitsScaled) {
          warn(
            `cursor coordinates look logical-pixel based; verify High-DPI transform at scale ${scaleFactor}`
          );
        }
      }
    }
  }

  const zoomSegments = project?.timeline?.zoomSegments;
  if (!Array.isArray(zoomSegments)) {
    fail("project.timeline.zoomSegments must be an array");
  } else {
    let previousEnd = -Infinity;
    for (let index = 0; index < zoomSegments.length; index += 1) {
      const segment = zoomSegments[index];
      const id = String(segment?.id ?? `segment-${index}`);
      const startTs = Number(segment?.startTs);
      const endTs = Number(segment?.endTs);

      if (!isFiniteNumber(startTs) || !isFiniteNumber(endTs)) {
        fail(`zoom segment ${id}: startTs/endTs must be numbers`);
        continue;
      }
      if (startTs < 0 || endTs <= startTs) {
        fail(`zoom segment ${id}: invalid time range ${startTs}..${endTs}`);
      }
      if (isFiniteNumber(project.durationMs) && endTs > project.durationMs + 1) {
        fail(`zoom segment ${id}: endTs exceeds project duration (${endTs} > ${project.durationMs})`);
      }
      if (startTs < previousEnd) {
        warn(`zoom segments overlap: previousEnd=${previousEnd}, currentStart=${startTs} (${id})`);
      }
      previousEnd = Math.max(previousEnd, endTs);

      const rect = segment?.targetRect;
      const x = Number(rect?.x);
      const y = Number(rect?.y);
      const width = Number(rect?.width);
      const height = Number(rect?.height);
      if (![x, y, width, height].every(Number.isFinite)) {
        fail(`zoom segment ${id}: targetRect has invalid values`);
        continue;
      }
      if (width <= 0 || height <= 0) {
        fail(`zoom segment ${id}: targetRect width/height must be > 0`);
      }
      if (x < 0 || y < 0 || x + width > 1.0001 || y + height > 1.0001) {
        fail(
          `zoom segment ${id}: targetRect out of [0..1] bounds (x=${x}, y=${y}, width=${width}, height=${height})`
        );
      }
    }
  }

  if (args.checkExport) {
    const latestExport = findLatestExport(projectDir);
    if (!latestExport) {
      warn("No export-*.mp4 files found for optional export check");
    } else {
      info(`latest export: ${latestExport}`);
      const exportProbe = probeVideo(latestExport);
      if (!isFiniteNumber(exportProbe.durationMs)) {
        warn("Failed to parse export duration from ffmpeg output");
      } else if (isFiniteNumber(project.durationMs)) {
        const delta = Math.abs(exportProbe.durationMs - project.durationMs);
        const allowed = Math.max(350, Math.round(project.durationMs * 0.08));
        if (delta > allowed) {
          warn(
            `export duration differs from project: export=${formatMs(exportProbe.durationMs)} project=${formatMs(
              project.durationMs
            )} delta=${delta}ms`
          );
        }
      }
    }
  }

  console.log(`Project: ${projectFilePath}`);
  console.log(`Video:   ${videoPath}`);
  console.log(`Events:  ${eventsPath}`);
  if (infos.length > 0) {
    console.log("");
    for (const line of infos) {
      console.log(`INFO: ${line}`);
    }
  }
  if (warnings.length > 0) {
    console.log("");
    for (const line of warnings) {
      console.log(`WARN: ${line}`);
    }
  }
  if (issues.length > 0) {
    console.log("");
    for (const line of issues) {
      console.log(`FAIL: ${line}`);
    }
    console.log("");
    console.log(`Smoke QA failed: ${issues.length} issue(s), ${warnings.length} warning(s).`);
    process.exit(1);
  }

  console.log("");
  console.log(`Smoke QA passed: 0 issues, ${warnings.length} warning(s).`);
}

try {
  main();
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error(`Smoke QA crashed: ${message}`);
  process.exit(1);
}
