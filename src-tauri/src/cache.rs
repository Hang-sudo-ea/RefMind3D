//! Persistent on-disk cache for runtime assets.
//!
//! RefMind3D resources (image previews, thumbnails, originals, document media)
//! are served to the webview through the custom `refmind3d://` protocol. Before
//! this cache existed, every request re-read the backing store (memory or the
//! packed `.refmind3d` ZIP) and re-decoded the bytes, and the response was
//! marked `Cache-Control: no-store` so the WebView could never reuse a decoded
//! image. With many images on the canvas, viewport culling mounts/unmounts
//! nodes while panning, which re-triggered those expensive reads and caused the
//! reported lag.
//!
//! Resources are content-addressed by `(asset_id, field)`. `asset_id` is a UUID
//! generated at import time and never changes for a given asset, so a resource
//! URL is immutable. We therefore materialize every resource to a user
//! configurable cache directory on first access and serve it with an
//! `immutable` cache header, which lets both the disk cache and the WebView
//! HTTP cache avoid repeated ZIP decompression / image re-decoding.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Serialize, Deserialize, Clone)]
struct CacheConfig {
    cache_dir: String,
}

static CACHE_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn cache_dir_cell() -> &'static Mutex<Option<PathBuf>> {
    CACHE_DIR.get_or_init(|| Mutex::new(None))
}

fn windows_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// App data root used only for the tiny cache-config file.
pub fn config_root() -> PathBuf {
    windows_env("APPDATA")
        .or_else(|| windows_env("LOCALAPPDATA"))
        .unwrap_or_else(std::env::temp_dir)
        .join("RefMind3D")
}

fn config_file_path() -> PathBuf {
    config_root().join("cache-config.json")
}

/// Default cache location: a regenerable, non-roaming cache directory.
pub fn default_cache_dir() -> PathBuf {
    windows_env("LOCALAPPDATA")
        .or_else(|| windows_env("APPDATA"))
        .unwrap_or_else(std::env::temp_dir)
        .join("RefMind3D")
        .join("Cache")
}

fn load_configured_dir() -> Option<PathBuf> {
    let text = fs::read_to_string(config_file_path()).ok()?;
    let config: CacheConfig = serde_json::from_str(&text).ok()?;
    let dir = PathBuf::from(config.cache_dir.trim());
    if dir.as_os_str().is_empty() {
        None
    } else {
        Some(dir)
    }
}

#[tauri::command]
pub fn get_cache_settings() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "cacheDir": cache_dir().to_string_lossy(),
        "defaultCacheDir": default_cache_dir().to_string_lossy(),
    }))
}

#[tauri::command]
pub fn set_cache_dir(path: String) -> Result<(), String> {
    set_cache_dir_inner(path)
}

/// Resolve the current cache directory (configured value, else the default).
pub fn cache_dir() -> PathBuf {
    {
        let cell = cache_dir_cell();
        if let Some(dir) = cell.lock().unwrap().clone() {
            return dir;
        }
    }
    let dir = load_configured_dir().unwrap_or_else(default_cache_dir);
    if let Ok(mut guard) = cache_dir_cell().lock() {
        *guard = Some(dir.clone());
    }
    dir
}

/// Persist a new cache directory and switch to it immediately.
fn set_cache_dir_inner(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("缓存目录不能为空".to_string());
    }
    let dir = PathBuf::from(trimmed);
    fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录失败: {e}"))?;

    let config = CacheConfig {
        cache_dir: dir.to_string_lossy().to_string(),
    };
    let config_path = config_file_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    fs::write(&config_path, text).map_err(|e| format!("写入缓存配置失败: {e}"))?;

    if let Ok(mut guard) = cache_dir_cell().lock() {
        *guard = Some(dir);
    }
    Ok(())
}

fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// On-disk path for a given resource. The cache is content-addressed by the
/// immutable `(asset_id, field)` pair.
pub fn cache_file_path(asset_id: &str, field: &str) -> PathBuf {
    cache_dir().join(format!("{}__{}", sanitize(asset_id), sanitize(field)))
}

pub fn read_cache(asset_id: &str, field: &str) -> Option<Vec<u8>> {
    fs::read(cache_file_path(asset_id, field)).ok()
}

pub fn write_cache(asset_id: &str, field: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = cache_file_path(asset_id, field);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)
}
