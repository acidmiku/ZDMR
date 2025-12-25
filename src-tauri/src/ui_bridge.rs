use crate::{
  app_state::AppState,
  engine::EngineCommand,
  model::{AddDownloadsRequest, NewBatchRequest, RulesSnapshot, SettingsSnapshot},
  transport::Transport,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;

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
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_add_batch(state: tauri::State<'_, AppState>, req: NewBatchRequest) -> Result<String, String> {
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
    })
    .await
    .map_err(|e| e.to_string())?;
  Ok(batch_id)
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


