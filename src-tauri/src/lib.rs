mod app_state;
mod engine;
mod error;
mod events;
mod local_api;
mod model;
mod persistence;
mod transport;
mod ui_bridge;

use app_state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  let mut builder = tauri::Builder::default();

  builder = builder.plugin(tauri_plugin_shell::init());
  builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());

  builder
    .setup(|app| -> Result<(), Box<dyn std::error::Error>> {
      // Logging + data dirs
      let paths = app_state::AppPaths::from_app(app.handle())?;
      error::init_tracing(&paths)?;

      tracing::info!(app_data_dir = %paths.app_data_dir.display(), "starting z-dmr");

      let db = persistence::Db::open(paths.db_path.clone())?;
      db.init_schema()?;

      let settings = persistence::SettingsStore::new(db.clone());
      let os_download_dir = app.handle().path().download_dir().unwrap_or_default();
      settings.ensure_bootstrap_defaults(&paths, os_download_dir)?;

      // Shared event hub + download engine
      let events = events::EventHub::new();
      let engine = engine::DownloadEngine::new(db.clone(), settings.clone(), events.clone());
      engine.start_background_tasks(app.handle().clone(), paths.clone());

      // Local loopback API (extension integration)
      local_api::spawn_local_api(app.handle().clone(), db.clone(), settings.clone(), engine.handle(), events.clone())?;

      app.manage(AppState {
        paths,
        db,
        settings,
        engine: engine.handle(),
        events,
      });

      // Tray
      ui_bridge::init_tray(app.handle())?;

      // Global hotkey (toggle show/hide). Best-effort; if parsing/register fails, app still runs.
      if let Ok(snap) = app.state::<AppState>().settings.get_snapshot() {
        if !snap.global_hotkey.trim().is_empty() {
          let gs = app.state::<tauri_plugin_global_shortcut::GlobalShortcut<tauri::Wry>>();
          let _ = gs.on_shortcut(snap.global_hotkey.as_str(), |app, _shortcut, _event| {
            let _ = crate::ui_bridge::toggle_main_window(app);
          });
        }
      }

      Ok(())
    })
    .on_window_event(|window, event| {
      if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        if let Some(st) = window.app_handle().try_state::<AppState>() {
          let minimize = st
            .settings
            .get_snapshot()
            .map(|s| s.minimize_to_tray)
            .unwrap_or(true);
          if minimize {
            api.prevent_close();
            let _ = window.hide();
          }
        }
      }
    })
    .invoke_handler(tauri::generate_handler![
      ui_bridge::cmd_list_downloads,
      ui_bridge::cmd_add_downloads,
      ui_bridge::cmd_add_batch,
      ui_bridge::cmd_pause_download,
      ui_bridge::cmd_resume_download,
      ui_bridge::cmd_retry_download,
      ui_bridge::cmd_delete_download,
      ui_bridge::cmd_pause_all,
      ui_bridge::cmd_resume_all,
      ui_bridge::cmd_get_settings,
      ui_bridge::cmd_set_settings,
      ui_bridge::cmd_list_rules,
      ui_bridge::cmd_upsert_proxy_rule,
      ui_bridge::cmd_delete_proxy_rule,
      ui_bridge::cmd_upsert_header_rule,
      ui_bridge::cmd_delete_header_rule,
      ui_bridge::cmd_upsert_mirror_rule,
      ui_bridge::cmd_delete_mirror_rule,
      ui_bridge::cmd_add_domain_to_proxy_and_retry,
      ui_bridge::cmd_clear_completed_downloads,
      ui_bridge::cmd_check_for_updates,
      ui_bridge::cmd_install_update,
      ui_bridge::cmd_open_logs_folder,
      ui_bridge::cmd_open_download_folder,
      ui_bridge::cmd_toggle_main_window,
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
