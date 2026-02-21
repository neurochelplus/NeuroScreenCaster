//! project_core — загрузка/сохранение project.json.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::models::events::{EventsFile, SCHEMA_VERSION as EVENTS_SCHEMA_VERSION};
use crate::models::project::{Project, SCHEMA_VERSION};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListItem {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub duration_ms: u64,
    pub video_width: u32,
    pub video_height: u32,
    pub project_path: String,
    pub folder_path: String,
    pub modified_time_ms: u64,
}

/// Загружает проект из файла `project.json`.
///
/// Поддерживает как путь к файлу, так и путь к директории проекта.
#[tauri::command]
pub async fn get_project(project_path: String) -> Result<Project, String> {
    let path = resolve_project_file(&project_path)?;
    log::info!("get_project: path={}", path.display());

    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read project file {}: {e}", path.display()))?;

    let project: Project = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse project.json {}: {e}", path.display()))?;

    if project.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "Unsupported project schemaVersion: expected {}, got {}",
            SCHEMA_VERSION, project.schema_version
        ));
    }

    Ok(project)
}

/// Загружает events.json для указанного проекта.
///
/// Поддерживает путь к `project.json` или путь к директории проекта.
#[tauri::command]
pub async fn get_events(project_path: String) -> Result<EventsFile, String> {
    let project_file = resolve_project_file(&project_path)?;
    let project_raw = std::fs::read_to_string(&project_file).map_err(|e| {
        format!(
            "Failed to read project file {}: {e}",
            project_file.display()
        )
    })?;
    let project: Project = serde_json::from_str(&project_raw).map_err(|e| {
        format!(
            "Failed to parse project file {}: {e}",
            project_file.display()
        )
    })?;

    if project.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "Unsupported project schemaVersion: expected {}, got {}",
            SCHEMA_VERSION, project.schema_version
        ));
    }

    let project_dir = project_file.parent().ok_or_else(|| {
        format!(
            "Project file has no parent directory: {}",
            project_file.display()
        )
    })?;
    let events_file = project_dir.join(Path::new(project.events_path.trim()));

    let events_raw = std::fs::read_to_string(&events_file)
        .map_err(|e| format!("Failed to read events file {}: {e}", events_file.display()))?;
    let events: EventsFile = serde_json::from_str(&events_raw)
        .map_err(|e| format!("Failed to parse events file {}: {e}", events_file.display()))?;

    if events.schema_version != EVENTS_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported events schemaVersion: expected {}, got {}",
            EVENTS_SCHEMA_VERSION, events.schema_version
        ));
    }

    Ok(events)
}

/// Сохраняет проект в `project.json`.
///
/// Если `project_path` не передан — используется стандартный путь:
/// `{Videos}/NeuroScreenCaster/{project.id}/project.json`.
#[tauri::command]
pub async fn save_project(
    project: Project,
    project_path: Option<String>,
) -> Result<String, String> {
    if project.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "Refusing to save unsupported schemaVersion: {}",
            project.schema_version
        ));
    }

    let path = match project_path {
        Some(path) if !path.trim().is_empty() => resolve_project_file(&path)?,
        _ => default_project_file(&project.id)?,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create project directory {}: {e}",
                parent.display()
            )
        })?;
    }

    let json = serde_json::to_string_pretty(&project)
        .map_err(|e| format!("Failed to serialize project {}: {e}", project.id))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write project file {}: {e}", path.display()))?;

    log::info!("save_project: id={} path={}", project.id, path.display());
    Ok(path.to_string_lossy().to_string())
}

/// Возвращает список проектов из стандартной папки `{Videos}/NeuroScreenCaster`.
#[tauri::command]
pub async fn list_projects() -> Result<Vec<ProjectListItem>, String> {
    let root = projects_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut projects = Vec::<ProjectListItem>::new();
    let entries = std::fs::read_dir(&root)
        .map_err(|e| format!("Failed to read projects directory {}: {e}", root.display()))?;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                log::warn!("list_projects: failed to read dir entry: {e}");
                continue;
            }
        };
        let folder_path = entry.path();
        if !folder_path.is_dir() {
            continue;
        }

        let project_path = folder_path.join("project.json");
        if !project_path.exists() {
            continue;
        }

        let raw = match std::fs::read_to_string(&project_path) {
            Ok(raw) => raw,
            Err(e) => {
                log::warn!(
                    "list_projects: failed to read {}: {e}",
                    project_path.display()
                );
                continue;
            }
        };

        let project: Project = match serde_json::from_str(&raw) {
            Ok(project) => project,
            Err(e) => {
                log::warn!(
                    "list_projects: failed to parse {}: {e}",
                    project_path.display()
                );
                continue;
            }
        };

        if project.schema_version != SCHEMA_VERSION {
            log::warn!(
                "list_projects: skip {} due to schemaVersion={}",
                project_path.display(),
                project.schema_version
            );
            continue;
        }

        let modified_time_ms = std::fs::metadata(&project_path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(project.created_at);

        projects.push(ProjectListItem {
            id: project.id,
            name: project.name,
            created_at: project.created_at,
            duration_ms: project.duration_ms,
            video_width: project.video_width,
            video_height: project.video_height,
            project_path: project_path.to_string_lossy().to_string(),
            folder_path: folder_path.to_string_lossy().to_string(),
            modified_time_ms,
        });
    }

    projects.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then(b.modified_time_ms.cmp(&a.modified_time_ms))
    });

    Ok(projects)
}

fn resolve_project_file(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Project path is empty".to_string());
    }

    let input = PathBuf::from(trimmed);
    let resolved = if input
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        input
    } else {
        input.join("project.json")
    };

    Ok(resolved)
}

fn default_project_file(project_id: &str) -> Result<PathBuf, String> {
    Ok(projects_root()?
        .join(project_id)
        .join(Path::new("project.json")))
}

fn projects_root() -> Result<PathBuf, String> {
    let base = dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .ok_or("Failed to resolve Videos directory")?;
    Ok(base.join("NeuroScreenCaster"))
}
