use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::capture::recorder::find_ffmpeg_exe;

const CURSOR_RESOLVED_PNG_NAME: &str = "cursor-resolved.png";

#[derive(Debug, Clone)]
pub(crate) struct ResolvedCursorAsset {
    pub png_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub hotspot_x: f64,
    pub hotspot_y: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorAssetInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub hotspot_x: f64,
    pub hotspot_y: f64,
}

#[derive(Debug, Clone, Copy)]
struct CurEntryInfo {
    width: u32,
    height: u32,
    hotspot_x: u32,
    hotspot_y: u32,
    bytes_in_res: u32,
    image_offset: u32,
}

#[tauri::command]
pub async fn get_cursor_asset_info() -> Result<Option<CursorAssetInfo>, String> {
    let Some(asset) = resolve_cursor_asset_for_render()? else {
        return Ok(None);
    };

    Ok(Some(CursorAssetInfo {
        path: asset.png_path.to_string_lossy().to_string(),
        width: asset.width,
        height: asset.height,
        hotspot_x: asset.hotspot_x,
        hotspot_y: asset.hotspot_y,
    }))
}

pub(crate) fn resolve_cursor_asset_for_render() -> Result<Option<ResolvedCursorAsset>, String> {
    let root = cursor_assets_root()?;
    if !root.exists() {
        return Ok(None);
    }

    let Some(source) = pick_cursor_source_file(&root)? else {
        return Ok(None);
    };

    let source_ext = source
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();

    let cur_info = if source_ext == "cur" {
        parse_cur_entry_info(&source)
    } else {
        None
    };

    let png_path = ensure_png_cursor(&root, &source, &source_ext)?;
    let (png_width, png_height) = read_png_dimensions(&png_path)?;

    let (hotspot_x, hotspot_y) = match cur_info {
        Some(info) => {
            let src_w = info.width.max(1) as f64;
            let src_h = info.height.max(1) as f64;
            let scale_x = png_width as f64 / src_w;
            let scale_y = png_height as f64 / src_h;
            (
                (info.hotspot_x as f64 * scale_x).clamp(0.0, png_width.saturating_sub(1) as f64),
                (info.hotspot_y as f64 * scale_y).clamp(0.0, png_height.saturating_sub(1) as f64),
            )
        }
        None => (0.0, 0.0),
    };

    Ok(Some(ResolvedCursorAsset {
        png_path,
        width: png_width,
        height: png_height,
        hotspot_x,
        hotspot_y,
    }))
}

fn cursor_assets_root() -> Result<PathBuf, String> {
    let base = dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Videos")))
        .ok_or("Failed to resolve Videos directory")?;
    Ok(base.join("NeuroScreenCaster").join("cursor"))
}

fn pick_cursor_source_file(root: &Path) -> Result<Option<PathBuf>, String> {
    let entries = std::fs::read_dir(root).map_err(|e| {
        format!(
            "Failed to read cursor assets directory {}: {e}",
            root.display()
        )
    })?;

    let mut candidates: Vec<(u8, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();

        let priority = match ext.as_str() {
            "cur" => 0,
            "ico" => 1,
            "png" => 2,
            "webp" => 3,
            "bmp" => 4,
            "jpg" | "jpeg" => 5,
            _ => continue,
        };

        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        let name_bias = if stem == "cursor" { 0 } else { 1 };

        candidates.push((priority * 2 + name_bias, path));
    }

    candidates.sort_by_key(|(priority, _)| *priority);
    Ok(candidates.into_iter().next().map(|(_, path)| path))
}

fn ensure_png_cursor(root: &Path, source: &Path, source_ext: &str) -> Result<PathBuf, String> {
    if source_ext == "png" {
        return Ok(source.to_path_buf());
    }

    std::fs::create_dir_all(root).map_err(|e| {
        format!(
            "Failed to create cursor assets directory {}: {e}",
            root.display()
        )
    })?;

    let target = root.join(CURSOR_RESOLVED_PNG_NAME);
    if !should_rebuild_target(source, &target) {
        return Ok(target);
    }

    if source_ext == "cur" {
        let mut errors: Vec<String> = Vec::new();

        match convert_cur_with_powershell(source, &target) {
            Ok(()) => return Ok(target),
            Err(err) => errors.push(format!("PowerShell conversion failed: {err}")),
        }

        match try_extract_embedded_png_from_cur(source, &target) {
            Ok(true) => return Ok(target),
            Ok(false) => errors.push("Embedded PNG entry not found in .cur".to_string()),
            Err(err) => errors.push(format!("Embedded PNG extraction failed: {err}")),
        }

        if let Err(err) = convert_cursor_with_ffmpeg(source, &target) {
            errors.push(format!("FFmpeg fallback failed: {err}"));
        } else {
            return Ok(target);
        }

        return Err(format!(
            "Failed to convert .cur cursor to PNG. {}",
            errors.join(" | ")
        ));
    }

    convert_cursor_with_ffmpeg(source, &target)?;
    Ok(target)
}

fn should_rebuild_target(source: &Path, target: &Path) -> bool {
    if !target.exists() {
        return true;
    }

    let source_mtime = std::fs::metadata(source)
        .and_then(|meta| meta.modified())
        .ok();
    let target_mtime = std::fs::metadata(target)
        .and_then(|meta| meta.modified())
        .ok();

    match (source_mtime, target_mtime) {
        (Some(src), Some(dst)) => src > dst,
        _ => true,
    }
}

fn convert_cursor_with_ffmpeg(source: &Path, target: &Path) -> Result<(), String> {
    let ffmpeg = find_ffmpeg_exe();
    let output = Command::new(&ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(source)
        .arg("-frames:v")
        .arg("1")
        .arg(target)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run ffmpeg ({}) for cursor conversion: {e}",
                ffmpeg.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "status: {} | {}",
            output.status,
            stderr.lines().rev().take(6).collect::<Vec<_>>().join(" | ")
        ));
    }

    Ok(())
}

fn convert_cur_with_powershell(source: &Path, target: &Path) -> Result<(), String> {
    let source_escaped = escape_powershell_single_quote(source);
    let target_escaped = escape_powershell_single_quote(target);
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         Add-Type -AssemblyName System.Windows.Forms; \
         Add-Type -AssemblyName System.Drawing; \
         $cursor = New-Object System.Windows.Forms.Cursor('{src}'); \
         $bmp = New-Object System.Drawing.Bitmap($cursor.Size.Width, $cursor.Size.Height); \
         $g = [System.Drawing.Graphics]::FromImage($bmp); \
         $g.Clear([System.Drawing.Color]::Transparent); \
         $cursor.Draw($g, [System.Drawing.Rectangle]::new(0, 0, $bmp.Width, $bmp.Height)); \
         $bmp.Save('{dst}', [System.Drawing.Imaging.ImageFormat]::Png); \
         $g.Dispose(); \
         $bmp.Dispose(); \
         $cursor.Dispose();",
        src = source_escaped,
        dst = target_escaped
    );

    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .output()
        .map_err(|e| format!("Failed to start PowerShell for cursor conversion: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "status: {} | stderr: {} | stdout: {}",
            output.status,
            stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | "),
            stdout.lines().rev().take(2).collect::<Vec<_>>().join(" | ")
        ));
    }

    Ok(())
}

fn try_extract_embedded_png_from_cur(source: &Path, target: &Path) -> Result<bool, String> {
    let bytes = std::fs::read(source)
        .map_err(|e| format!("Failed to read .cur file {}: {e}", source.display()))?;
    let mut entries = parse_cur_entries_from_bytes(&bytes);
    if entries.is_empty() {
        return Ok(false);
    }

    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    entries.sort_by_key(|entry| entry.width.saturating_mul(entry.height));
    entries.reverse();

    for entry in entries {
        let start = entry.image_offset as usize;
        let len = entry.bytes_in_res as usize;
        let end = start.saturating_add(len);
        if start >= bytes.len() || end > bytes.len() {
            continue;
        }

        let payload = &bytes[start..end];
        if !payload.starts_with(&PNG_SIGNATURE) {
            continue;
        }

        std::fs::write(target, payload).map_err(|e| {
            format!(
                "Failed to write extracted PNG cursor {}: {e}",
                target.display()
            )
        })?;
        return Ok(true);
    }

    Ok(false)
}

fn escape_powershell_single_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn parse_cur_entry_info(path: &Path) -> Option<CurEntryInfo> {
    let bytes = std::fs::read(path).ok()?;
    parse_cur_entry_info_from_bytes(&bytes)
}

fn parse_cur_entry_info_from_bytes(bytes: &[u8]) -> Option<CurEntryInfo> {
    let mut entries = parse_cur_entries_from_bytes(bytes);
    if entries.is_empty() {
        return None;
    }
    entries.sort_by_key(|entry| entry.width.saturating_mul(entry.height));
    entries.pop()
}

fn parse_cur_entries_from_bytes(bytes: &[u8]) -> Vec<CurEntryInfo> {
    let mut entries = Vec::new();
    if bytes.len() < 6 {
        return entries;
    }

    let icon_type = u16::from_le_bytes([bytes[2], bytes[3]]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if icon_type != 2 || count == 0 {
        return entries;
    }
    if bytes.len() < 6 + count * 16 {
        return entries;
    }

    for index in 0..count {
        let offset = 6 + index * 16;
        let width = if bytes[offset] == 0 {
            256
        } else {
            bytes[offset] as u32
        };
        let height = if bytes[offset + 1] == 0 {
            256
        } else {
            bytes[offset + 1] as u32
        };
        let hotspot_x = u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]) as u32;
        let hotspot_y = u16::from_le_bytes([bytes[offset + 6], bytes[offset + 7]]) as u32;
        let bytes_in_res = u32::from_le_bytes([
            bytes[offset + 8],
            bytes[offset + 9],
            bytes[offset + 10],
            bytes[offset + 11],
        ]);
        let image_offset = u32::from_le_bytes([
            bytes[offset + 12],
            bytes[offset + 13],
            bytes[offset + 14],
            bytes[offset + 15],
        ]);

        entries.push(CurEntryInfo {
            width,
            height,
            hotspot_x,
            hotspot_y,
            bytes_in_res,
            image_offset,
        });
    }

    entries
}

fn read_png_dimensions(path: &Path) -> Result<(u32, u32), String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open PNG cursor {}: {e}", path.display()))?;

    let mut header = [0u8; 24];
    file.read_exact(&mut header)
        .map_err(|e| format!("Failed to read PNG cursor header {}: {e}", path.display()))?;

    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if header[0..8] != PNG_SIGNATURE {
        return Err(format!(
            "Cursor file is not a valid PNG: {}",
            path.display()
        ));
    }

    let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    if width == 0 || height == 0 {
        return Err(format!(
            "Invalid PNG cursor dimensions in {}",
            path.display()
        ));
    }

    Ok((width, height))
}
