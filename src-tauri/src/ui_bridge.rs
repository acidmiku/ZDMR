use crate::{
  app_state::AppState,
  engine::EngineCommand,
  model::{AddDownloadsRequest, NewBatchRequest, RulesSnapshot, SettingsSnapshot},
  transport::Transport,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResult {
  pub current_version: String,
  pub latest_version: Option<String>,
  pub update_available: bool,
  pub installer_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
  tag_name: String,
  assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhAsset {
  name: String,
  browser_download_url: String,
}

async fn spawn_installer_with_retry(installer_path: &std::path::Path) -> Result<(), String> {
  // Windows can transiently lock freshly-written executables/MSIs (e.g. AV scanning),
  // returning ERROR_SHARING_VIOLATION (os error 32). Retry a few times.
  let ext = installer_path
    .extension()
    .and_then(|s| s.to_str())
    .unwrap_or("")
    .to_ascii_lowercase();

  for attempt in 0..10 {
    let spawn_res = if ext == "msi" {
      std::process::Command::new("msiexec")
        .args(["/i", installer_path.to_string_lossy().as_ref()])
        .spawn()
        .map(|_| ())
    } else {
      std::process::Command::new(installer_path).spawn().map(|_| ())
    };

    match spawn_res {
      Ok(()) => return Ok(()),
      Err(e) => {
        let sharing_violation = e.raw_os_error() == Some(32);
        if sharing_violation && attempt < 9 {
          tokio::time::sleep(std::time::Duration::from_millis(200 + attempt * 150)).await;
          continue;
        }
        return Err(e.to_string());
      }
    }
  }

  Err("failed to launch installer".to_string())
}

#[tauri::command]
pub fn cmd_list_downloads(state: tauri::State<AppState>) -> Result<Vec<crate::model::DownloadRecord>, String> {
  state.db.list_downloads().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_add_downloads(state: tauri::State<'_, AppState>, req: AddDownloadsRequest) -> Result<(), String> {
  let dest_dir = match req.dest_dir {
    Some(d) => d,
    None => state
      .settings
      .get_snapshot()
      .map(|s| s.default_download_dir)
      .map_err(|e| e.to_string())?,
  };
  state
    .engine
    .send(EngineCommand::AddDownloads {
      urls: req.urls,
      dest_dir,
      batch_id: None,
      forced_proxy: false,
      forced_proxy_url: None,
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_add_batch(state: tauri::State<'_, AppState>, req: NewBatchRequest) -> Result<String, String> {
  let force_proxy = req.download_through_proxy.unwrap_or(false);
  let forced_proxy_url = if force_proxy {
    let s = state.settings.get_snapshot().map_err(|e| e.to_string())?;
    let url = s
      .global_proxy_url
      .clone()
      .filter(|v| !v.trim().is_empty())
      .ok_or_else(|| "Proxy address is empty. Set it in Settings first.".to_string())?;
    Some(url)
  } else {
    None
  };

  let batch_id = state
    .db
    .insert_batch(&req.dest_dir, req.name.as_deref(), req.raw_url_list.as_deref())
    .map_err(|e| e.to_string())?;
  state
    .engine
    .send(EngineCommand::AddDownloads {
      urls: req.urls,
      dest_dir: req.dest_dir,
      batch_id: Some(batch_id.clone()),
      forced_proxy: force_proxy,
      forced_proxy_url,
    })
    .await
    .map_err(|e| e.to_string())?;
  Ok(batch_id)
}

#[tauri::command]
pub fn cmd_clear_completed_downloads(state: tauri::State<AppState>) -> Result<i64, String> {
  state
    .db
    .delete_completed_downloads()
    .map(|n| n as i64)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_check_for_updates(app: AppHandle) -> Result<UpdateCheckResult, String> {
  let current_version = app.package_info().version.to_string();

  let client = reqwest::Client::builder()
    .user_agent("Z-DMR")
    .build()
    .map_err(|e| e.to_string())?;

  let resp = client
    .get("https://api.github.com/repos/acidmiku/ZDMR/releases/latest")
    .send()
    .await
    .map_err(|e| e.to_string())?;

  if !resp.status().is_success() {
    return Err(format!("Update check failed: HTTP {}", resp.status().as_u16()));
  }

  let release: GhRelease = resp.json().await.map_err(|e| e.to_string())?;

  let latest_tag = release.tag_name;
  let latest_version = latest_tag.trim_start_matches('v').to_string();

  let current_v = semver::Version::parse(current_version.trim_start_matches('v'))
    .map_err(|_| format!("Cannot parse current version: {}", current_version))?;
  let latest_v = semver::Version::parse(latest_version.trim_start_matches('v'))
    .map_err(|_| format!("Cannot parse latest version: {}", latest_version))?;

  let update_available = latest_v > current_v;

  let installer_url = if update_available {
    pick_windows_installer_url(&release.assets)
  } else {
    None
  };

  Ok(UpdateCheckResult {
    current_version,
    latest_version: Some(latest_version),
    update_available,
    installer_url,
  })
}

fn pick_windows_installer_url(assets: &[GhAsset]) -> Option<String> {
  // Prefer NSIS setup exe, else MSI.
  for a in assets {
    let n = a.name.to_ascii_lowercase();
    if n.ends_with("-setup.exe") || n.ends_with("setup.exe") {
      return Some(a.browser_download_url.clone());
    }
  }
  for a in assets {
    let n = a.name.to_ascii_lowercase();
    if n.ends_with(".msi") {
      return Some(a.browser_download_url.clone());
    }
  }
  None
}

#[tauri::command]
pub async fn cmd_install_update(app: AppHandle, installer_url: String) -> Result<(), String> {
  let client = reqwest::Client::builder()
    .user_agent("Z-DMR")
    .build()
    .map_err(|e| e.to_string())?;

  let resp = client
    .get(&installer_url)
    .send()
    .await
    .map_err(|e| e.to_string())?;

  if !resp.status().is_success() {
    return Err(format!("Download failed: HTTP {}", resp.status().as_u16()));
  }

  let filename = installer_url
    .split('/')
    .last()
    .filter(|s| !s.is_empty())
    .unwrap_or("zdmr-installer.exe");

  let mut path: PathBuf = std::env::temp_dir();
  // Always use a unique temp filename to avoid collisions/locks with a previous attempt.
  let ext = std::path::Path::new(filename).extension().and_then(|s| s.to_str()).unwrap_or("exe");
  path.push(format!("zdmr-update-{}.{}", Uuid::new_v4(), ext));

  let mut file = tokio::fs::OpenOptions::new()
    .create_new(true)
    .write(true)
    .open(&path)
    .await
    .map_err(|e| e.to_string())?;
  let mut stream = resp.bytes_stream();
  while let Some(chunk) = stream.next().await {
    let chunk = chunk.map_err(|e| e.to_string())?;
    tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
      .await
      .map_err(|e| e.to_string())?;
  }
  tokio::io::AsyncWriteExt::flush(&mut file)
    .await
    .map_err(|e| e.to_string())?;
  file.sync_all().await.map_err(|e| e.to_string())?;
  drop(file); // critical on Windows: ensure the file handle is closed before launching

  // Launch installer (with retry for Windows sharing violations)
  spawn_installer_with_retry(&path).await?;

  // Exit so installer can replace files.
  app.exit(0);
  Ok(())
}

#[tauri::command]
pub async fn cmd_pause_download(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
  state
    .engine
    .send(EngineCommand::Pause { id })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_resume_download(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
  state
    .engine
    .send(EngineCommand::Resume { id })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_retry_download(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
  state
    .engine
    .send(EngineCommand::Retry { id })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_delete_download(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
  state
    .engine
    .send(EngineCommand::Delete { id })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_pause_all(state: tauri::State<'_, AppState>) -> Result<(), String> {
  state
    .engine
    .send(EngineCommand::PauseAll)
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_resume_all(state: tauri::State<'_, AppState>) -> Result<(), String> {
  state
    .engine
    .send(EngineCommand::ResumeAll)
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_get_settings(state: tauri::State<AppState>) -> Result<SettingsSnapshot, String> {
  state.settings.get_snapshot().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_set_settings(state: tauri::State<'_, AppState>, s: SettingsSnapshot) -> Result<(), String> {
  state.settings.set_snapshot(&s).map_err(|e| e.to_string())?;
  state
    .engine
    .send(crate::engine::EngineCommand::UpdateSettings {
      bandwidth_limit_bps: s.bandwidth_limit_bps,
    })
    .await
    .map_err(|e| e.to_string())?;
  Ok(())
}

#[tauri::command]
pub fn cmd_list_rules(state: tauri::State<AppState>) -> Result<RulesSnapshot, String> {
  state.db.list_rules().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_upsert_proxy_rule(
  state: tauri::State<AppState>,
  id: Option<i64>,
  pattern: String,
  enabled: bool,
  use_proxy: bool,
  proxy_url_override: Option<String>,
) -> Result<i64, String> {
  state
    .db
    .upsert_proxy_rule(id, &pattern, enabled, use_proxy, proxy_url_override.as_deref())
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_delete_proxy_rule(state: tauri::State<AppState>, id: i64) -> Result<(), String> {
  state.db.delete_proxy_rule(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_upsert_header_rule(
  state: tauri::State<AppState>,
  id: Option<i64>,
  pattern: String,
  enabled: bool,
  headers_json: serde_json::Value,
) -> Result<i64, String> {
  state
    .db
    .upsert_header_rule(id, &pattern, enabled, &headers_json)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_delete_header_rule(state: tauri::State<AppState>, id: i64) -> Result<(), String> {
  state.db.delete_header_rule(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_upsert_mirror_rule(
  state: tauri::State<AppState>,
  id: Option<i64>,
  pattern: String,
  enabled: bool,
  candidates_json: serde_json::Value,
) -> Result<i64, String> {
  state
    .db
    .upsert_mirror_rule(id, &pattern, enabled, &candidates_json)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_delete_mirror_rule(state: tauri::State<AppState>, id: i64) -> Result<(), String> {
  state.db.delete_mirror_rule(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_add_domain_to_proxy_and_retry(state: tauri::State<'_, AppState>, download_id: String, url: String) -> Result<(), String> {
  let host = Transport::url_hostname(&url).ok_or_else(|| "could not parse hostname".to_string())?;
  let _id = state
    .db
    .upsert_proxy_rule(None, &host, true, true, None)
    .map_err(|e| e.to_string())?;
  state
    .engine
    .send(EngineCommand::Retry { id: download_id })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_open_logs_folder(state: tauri::State<AppState>, app: AppHandle) -> Result<(), String> {
  let p = state.paths.logs_dir.clone();
  app
    .shell()
    .open(p.to_string_lossy().to_string(), None)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_open_download_folder(dest_dir: String, app: AppHandle) -> Result<(), String> {
  app.shell().open(dest_dir, None).map_err(|e| e.to_string())
}

pub fn init_tray(app: &AppHandle) -> anyhow::Result<()> {
  use tauri::menu::{Menu, MenuItem};
  use tauri::tray::TrayIconBuilder;

  let show = MenuItem::with_id(app, "show_hide", "Show/Hide", true, None::<&str>)?;
  let pause_all = MenuItem::with_id(app, "pause_all", "Pause all", true, None::<&str>)?;
  let resume_all = MenuItem::with_id(app, "resume_all", "Resume all", true, None::<&str>)?;
  let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

  let menu = Menu::with_items(app, &[&show, &pause_all, &resume_all, &quit])?;

  let mut builder = TrayIconBuilder::new()
    .menu(&menu)
    .on_menu_event(move |app, event| {
      let id = event.id().as_ref();
      match id {
        "show_hide" => {
          if let Some(w) = app.get_webview_window("main") {
            let _ = if w.is_visible().unwrap_or(true) { w.hide() } else { w.show() };
            let _ = w.set_focus();
          }
        }
        "pause_all" => {
          if let Some(st) = app.try_state::<AppState>() {
            let eng = st.engine.clone();
            tauri::async_runtime::spawn(async move {
              let _ = eng.send(EngineCommand::PauseAll).await;
            });
          }
        }
        "resume_all" => {
          if let Some(st) = app.try_state::<AppState>() {
            let eng = st.engine.clone();
            tauri::async_runtime::spawn(async move {
              let _ = eng.send(EngineCommand::ResumeAll).await;
            });
          }
        }
        "quit" => {
          app.exit(0);
        }
        _ => {}
      }
    })
    ;

  // Ensure tray icon matches the app/window icon (from src-tauri/icons/icon.ico on Windows).
  if let Some(icon) = app.default_window_icon().cloned() {
    builder = builder.icon(icon);
  }

  builder.build(app)?;

  Ok(())
}


