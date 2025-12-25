use crate::{engine, events, persistence};
use anyhow::Context;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

#[derive(Clone)]
pub struct AppPaths {
  pub app_data_dir: PathBuf,
  pub logs_dir: PathBuf,
  pub db_path: PathBuf,
}

impl AppPaths {
  pub fn from_app(app: &AppHandle) -> anyhow::Result<Self> {
    let app_data_dir = app
      .path()
      .app_data_dir()
      .context("failed to resolve app_data_dir")?;
    std::fs::create_dir_all(&app_data_dir).context("failed to create app_data_dir")?;

    let logs_dir = app_data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir).context("failed to create logs dir")?;

    let db_path = app_data_dir.join("zdmr.sqlite3");

    Ok(Self {
      app_data_dir,
      logs_dir,
      db_path,
    })
  }
}

#[derive(Clone)]
pub struct AppState {
  pub paths: AppPaths,
  pub db: persistence::Db,
  pub settings: persistence::SettingsStore,
  pub engine: engine::DownloadEngineHandle,
  pub events: events::EventHub,
}


