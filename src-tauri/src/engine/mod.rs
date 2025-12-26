pub mod bandwidth;
pub mod file_writer;
pub mod naming;
mod job;

use crate::{
  events::{EventHub, ServerEvent, EVENT_DOWNLOADS_CHANGED, EVENT_PROGRESS_BATCH},
  model::{DownloadProgressUpdate, DownloadStatus},
  persistence::{Db, SettingsStore},
  transport::Transport,
};
use anyhow::Context;
use dashmap::DashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Mutex, watch};
use uuid::Uuid;

#[derive(Clone)]
pub struct DownloadEngineHandle {
  tx: mpsc::Sender<EngineCommand>,
}

impl DownloadEngineHandle {
  pub async fn send(&self, cmd: EngineCommand) -> anyhow::Result<()> {
    self.tx.send(cmd).await.context("engine channel closed")
  }
}

pub struct DownloadEngine {
  inner: Arc<EngineInner>,
  tx: mpsc::Sender<EngineCommand>,
  rx: Mutex<Option<mpsc::Receiver<EngineCommand>>>,
}

#[derive(Debug)]
pub enum EngineCommand {
  AddDownloads { urls: Vec<String>, dest_dir: String, batch_id: Option<String>, forced_proxy: bool, forced_proxy_url: Option<String> },
  Pause { id: String },
  Resume { id: String },
  Retry { id: String },
  Delete { id: String },
  PauseAll,
  ResumeAll,
  UpdateSettings { bandwidth_limit_bps: Option<i64> },
}

impl DownloadEngine {
  pub fn new(db: Db, settings: SettingsStore, events: EventHub) -> Self {
    let (tx, rx) = mpsc::channel(1024);
    let limiter = bandwidth::BandwidthLimiter::new(
      settings.get_snapshot().ok().and_then(|s| s.bandwidth_limit_bps).unwrap_or(0),
    );
    let transport = Transport::new().expect("transport init");
    let inner = Arc::new(EngineInner {
      db,
      settings,
      events,
      limiter,
      transport,
      jobs: Arc::new(DashMap::new()),
      stats: Arc::new(DashMap::new()),
    });
    Self {
      inner,
      tx,
      rx: Mutex::new(Some(rx)),
    }
  }

  pub fn handle(&self) -> DownloadEngineHandle {
    DownloadEngineHandle { tx: self.tx.clone() }
  }

  pub fn start_background_tasks(&self, app: AppHandle, _paths: crate::app_state::AppPaths) {
    // The engine loop runs on Tokio (Tauri v2 runtime is Tokio).
    let mut guard = self.rx.try_lock().expect("engine started twice");
    let mut rx = guard.take().expect("engine started twice");
    let inner = self.inner.clone();

    // Recover any “DOWNLOADING” records after crash.
    inner.db.recover_incomplete_downloads().ok();

    // Forward internal events to the UI (Tauri event stream).
    spawn_tauri_event_forwarder(app.clone(), inner.events.clone());

    // Throttled progress batch producer (30Hz).
    spawn_progress_flusher(inner.clone());

    tauri::async_runtime::spawn(async move {
      while let Some(cmd) = rx.recv().await {
        if let Err(e) = handle_cmd(inner.clone(), cmd).await {
          tracing::error!(error = %e, "engine command failed");
        }
      }
    });
  }
}

struct EngineInner {
  db: Db,
  settings: SettingsStore,
  events: EventHub,
  limiter: bandwidth::BandwidthLimiter,
  transport: Transport,
  jobs: Arc<DashMap<String, JobEntry>>,
  stats: Arc<DashMap<String, job::RuntimeStats>>,
}

struct JobEntry {
  control_tx: watch::Sender<job::JobControl>,
}

async fn handle_cmd(inner: Arc<EngineInner>, cmd: EngineCommand) -> anyhow::Result<()> {
  match cmd {
    EngineCommand::AddDownloads { urls, dest_dir, batch_id, forced_proxy, forced_proxy_url } => {
      for url in urls {
        let id = Uuid::new_v4().to_string();
        inner.db.insert_download_skeleton(&id, &url, &dest_dir, forced_proxy, forced_proxy_url.as_deref())?;
        if let Some(batch_id) = batch_id.as_deref() {
          inner.db.attach_download_to_batch(&id, batch_id)?;
        }
        inner.db.update_download_status(&id, DownloadStatus::Queued, None, None)?;
        start_or_resume(inner.clone(), id).await?;
      }
      inner.events.emit_downloads_changed();
      Ok(())
    }
    EngineCommand::Pause { id } => {
      if let Some(job) = inner.jobs.get(&id) {
        let _ = job.control_tx.send(job::JobControl::Pause);
      }
      inner.db.update_download_status(&id, DownloadStatus::Paused, None, None)?;
      inner.events.emit_downloads_changed();
      Ok(())
    }
    EngineCommand::Resume { id } => {
      inner.db.update_download_status(&id, DownloadStatus::Queued, None, None)?;
      start_or_resume(inner.clone(), id).await?;
      inner.events.emit_downloads_changed();
      Ok(())
    }
    EngineCommand::Retry { id } => {
      // Reset state + restart.
      if let Some(job) = inner.jobs.get(&id) {
        let _ = job.control_tx.send(job::JobControl::Cancel);
      }
      if let Some(r) = inner.db.get_download(&id)? {
        if let Some(p) = r.temp_path {
          let _ = std::fs::remove_file(p);
        }
      }
      inner.db.reset_download_for_retry(&id)?;
      start_or_resume(inner.clone(), id).await?;
      inner.events.emit_downloads_changed();
      Ok(())
    }
    EngineCommand::Delete { id } => {
      if let Some(job) = inner.jobs.get(&id) {
        let _ = job.control_tx.send(job::JobControl::Cancel);
      }
      if let Some(r) = inner.db.get_download(&id)? {
        if let Some(p) = r.temp_path {
          let _ = std::fs::remove_file(p);
        }
        if let Some(name) = r.final_filename {
          let fp = std::path::Path::new(&r.dest_dir).join(name);
          let _ = std::fs::remove_file(fp);
        }
      }
      inner.db.delete_download(&id)?;
      inner.events.emit_downloads_changed();
      Ok(())
    }
    EngineCommand::PauseAll => {
      for j in inner.jobs.iter() {
        let _ = j.control_tx.send(job::JobControl::Pause);
      }
      Ok(())
    }
    EngineCommand::ResumeAll => {
      // Resume all paused/queued downloads by scanning DB.
      for d in inner.db.list_downloads()? {
        if matches!(d.status, DownloadStatus::Paused | DownloadStatus::Queued) {
          start_or_resume(inner.clone(), d.id).await.ok();
        }
      }
      Ok(())
    }
    EngineCommand::UpdateSettings { bandwidth_limit_bps } => {
      let bps = bandwidth_limit_bps.unwrap_or(0);
      inner.limiter.set_limit_bps(bps);
      Ok(())
    }
  }
}

async fn start_or_resume(inner: Arc<EngineInner>, id: String) -> anyhow::Result<()> {
  // Already active?
  if inner.jobs.contains_key(&id) {
    return Ok(());
  }

  // Snapshot rules once per job start (deterministic).
  let rules = inner.db.list_rules()?;

  let (tx, rx) = watch::channel(job::JobControl::Run);
  inner.jobs.insert(id.clone(), JobEntry { control_tx: tx });

  let stats = job::RuntimeStats::new(id.clone());
  inner.stats.insert(id.clone(), stats.clone());

  let db = inner.db.clone();
  let settings = inner.settings.clone();
  let transport = inner.transport.clone();
  let limiter = inner.limiter.clone();
  let events = inner.events.clone();
  let jobs = inner.jobs.clone();
  let stats_map = inner.stats.clone();

  tauri::async_runtime::spawn(async move {
    let res = job::run_download_job(
      db.clone(),
      settings.clone(),
      transport.clone(),
      limiter.clone(),
      rules,
      events.clone(),
      id.clone(),
      rx,
      stats.clone(),
    )
    .await;

    if let Err(e) = res {
      tracing::error!(download_id = %id, error = %e, "download job failed");
    }

    jobs.remove(&id);
    // Keep stats for a short while so UI can receive last status; dropping is fine too.
    stats_map.remove(&id);
    events.emit_downloads_changed();
  });

  Ok(())
}

fn spawn_progress_flusher(inner: Arc<EngineInner>) {
  tauri::async_runtime::spawn(async move {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      tick.tick().await;
      if inner.stats.is_empty() {
        continue;
      }
      let now = now_rfc3339();
      let mut batch: Vec<DownloadProgressUpdate> = Vec::new();
      for item in inner.stats.iter() {
        let bytes = item.bytes.load(std::sync::atomic::Ordering::Relaxed);
        let last = item.last_bytes.swap(bytes, std::sync::atomic::Ordering::Relaxed);
        let inst = ((bytes - last) as f64) / 0.033;
        {
          let mut ewma = item.speed_ewma.lock();
          let alpha = 0.2;
          *ewma = (*ewma * (1.0 - alpha)) + (inst.max(0.0) * alpha);
        }
        let speed = *item.speed_ewma.lock();
        let total_raw = item.total.load(std::sync::atomic::Ordering::Relaxed);
        let total = if total_raw > 0 { Some(total_raw) } else { None };
        let eta = match (total, speed) {
          (Some(t), s) if s > 1.0 && bytes >= 0 && t > bytes => Some(((t - bytes) as f64) / s),
          _ => None,
        };

        let status = *item.status.lock();
        let error_code = item.error_code.lock().clone();
        let error_message = item.error_message.lock().clone();
        let detail_from_job = item.status_detail.lock().clone();
        let backoff_until_ms = item.backoff_until_ms.load(std::sync::atomic::Ordering::Relaxed);
        let now_ms = {
          use std::time::{SystemTime, UNIX_EPOCH};
          SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
        };
        let status_detail = if backoff_until_ms > now_ms {
          let secs = ((backoff_until_ms - now_ms) as f64 / 1000.0).ceil() as i64;
          Some(format!("Retrying in {secs}s"))
        } else {
          detail_from_job
        };
        batch.push(DownloadProgressUpdate {
          id: item.id.clone(),
          status,
          bytes_downloaded: bytes,
          content_length: total,
          speed_bps: speed,
          eta_seconds: eta,
          status_detail,
          error_code,
          error_message,
          updated_at: now.clone(),
        });
      }
      inner.events.emit_progress_batch(batch);
    }
  });
}

fn spawn_tauri_event_forwarder(app: AppHandle, events: EventHub) {
  tauri::async_runtime::spawn(async move {
    let mut rx = events.subscribe();
    loop {
      match rx.recv().await {
        Ok(ServerEvent::ProgressBatch(batch)) => {
          let _ = app.emit(EVENT_PROGRESS_BATCH, batch);
        }
        Ok(ServerEvent::DownloadsChanged) => {
          let _ = app.emit(EVENT_DOWNLOADS_CHANGED, ());
        }
        Err(_) => {}
      }
    }
  });
}

fn now_rfc3339() -> String {
  time::OffsetDateTime::now_utc()
    .format(&time::format_description::well_known::Rfc3339)
    .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

