use crate::{
  engine::{bandwidth::BandwidthLimiter, file_writer::write_at_all, naming},
  error::ErrorCode,
  model::{DownloadRecord, DownloadStatus},
  persistence::{Db, SegmentRow, SegmentRowWithId, SettingsStore},
  transport::Transport,
};
use anyhow::Context;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, RANGE};
use std::{
  fs::OpenOptions,
  path::{Path, PathBuf},
  sync::atomic::{AtomicI64, Ordering},
  sync::Arc,
  time::Duration,
};
use tokio::sync::watch;
use tokio::time::Instant;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobControl {
  Run,
  Pause,
  Cancel,
}

#[derive(Clone)]
pub struct RuntimeStats {
  pub id: String,
  pub status: Arc<parking_lot::Mutex<DownloadStatus>>,
  pub bytes: Arc<AtomicI64>,
  // -1 means unknown
  pub total: Arc<AtomicI64>,
  pub speed_ewma: Arc<parking_lot::Mutex<f64>>,
  pub last_bytes: Arc<AtomicI64>,
  pub error_code: Arc<parking_lot::Mutex<Option<ErrorCode>>>,
  pub error_message: Arc<parking_lot::Mutex<Option<String>>>,
}

impl RuntimeStats {
  pub fn new(id: String) -> Self {
    Self {
      id,
      status: Arc::new(parking_lot::Mutex::new(DownloadStatus::Queued)),
      bytes: Arc::new(AtomicI64::new(0)),
      total: Arc::new(AtomicI64::new(-1)),
      speed_ewma: Arc::new(parking_lot::Mutex::new(0.0)),
      last_bytes: Arc::new(AtomicI64::new(0)),
      error_code: Arc::new(parking_lot::Mutex::new(None)),
      error_message: Arc::new(parking_lot::Mutex::new(None)),
    }
  }
}

pub async fn run_download_job(
  db: Db,
  settings: SettingsStore,
  transport: Transport,
  limiter: BandwidthLimiter,
  rules: crate::model::RulesSnapshot,
  events: crate::events::EventHub,
  download_id: String,
  control_rx: watch::Receiver<JobControl>,
  stats: RuntimeStats,
) -> anyhow::Result<()> {
  let Some(mut rec) = db.get_download(&download_id)? else {
    return Ok(());
  };

  *stats.status.lock() = DownloadStatus::Downloading;
  db.update_download_status(&download_id, DownloadStatus::Downloading, None, None)?;

  let urls = build_attempt_urls(&rules, &rec)?;
  let dest_dir = PathBuf::from(&rec.dest_dir);
  naming::ensure_dir(&dest_dir)?;

  // Fresh name/temp decisions are made on the first successful probe.
  for (attempt_idx, url) in urls.into_iter().enumerate() {
    if matches!(*control_rx.borrow(), JobControl::Pause | JobControl::Cancel) {
      *stats.status.lock() = DownloadStatus::Paused;
      db.update_download_status(&download_id, DownloadStatus::Paused, None, None)?;
      return Ok(());
    }

    let attempt_url = url.to_string();
    match attempt_download_once(
      &db,
      &settings,
      &transport,
      &limiter,
      &rules,
      &events,
      &download_id,
      &attempt_url,
      attempt_idx,
      &mut rec,
      control_rx.clone(),
      stats.clone(),
    )
    .await
    {
      Ok(()) => return Ok(()),
      Err(e) => {
        tracing::warn!(download_id = %download_id, url = %attempt_url, error = %e, "attempt failed");
        if let Some(code) = stats.error_code.lock().clone() {
          if !code.is_retryable() {
            break;
          }
        }
        // retryable failures fall through to next mirror candidate; final failure sets ERROR below.
        continue;
      }
    }
  }

  // If we got here, all attempts failed.
  let code = stats.error_code.lock().clone().unwrap_or(ErrorCode::Unknown);
  let msg = stats
    .error_message
    .lock()
    .clone()
    .unwrap_or_else(|| "download failed".to_string());
  *stats.status.lock() = DownloadStatus::Error;
  db.update_download_status(&download_id, DownloadStatus::Error, Some(&format_code(code)), Some(&msg))?;
  Ok(())
}

fn build_attempt_urls(rules: &crate::model::RulesSnapshot, rec: &DownloadRecord) -> anyhow::Result<Vec<Url>> {
  let original = Url::parse(&rec.original_url).context("invalid url")?;
  let mut out = vec![original.clone()];
  for m in Transport::mirror_candidates(rules, &original) {
    out.push(m);
  }
  Ok(out)
}

async fn attempt_download_once(
  db: &Db,
  settings: &SettingsStore,
  transport: &Transport,
  limiter: &BandwidthLimiter,
  rules: &crate::model::RulesSnapshot,
  events: &crate::events::EventHub,
  download_id: &str,
  url: &str,
  attempt_idx: usize,
  rec: &mut DownloadRecord,
  mut control_rx: watch::Receiver<JobControl>,
  stats: RuntimeStats,
) -> anyhow::Result<()> {
  let url_parsed = Url::parse(url).context("invalid url")?;

  let proxy_url = Transport::effective_proxy_url(&settings.get_snapshot()?, rules, &url_parsed);
  let forced = rec.forced_proxy;
  let forced_url = rec
    .forced_proxy_url
    .clone()
    .or_else(|| settings.get_snapshot().ok().and_then(|s| s.global_proxy_url));
  let proxy_url = if forced {
    forced_url.filter(|v| !v.trim().is_empty())
  } else {
    proxy_url
  };
  let client = transport.client_for(proxy_url.as_deref())?;

  // Record which source URL (and which mirror, if any) we are currently attempting.
  let mirror_used = if attempt_idx == 0 {
    None
  } else {
    Some(url_parsed.origin().ascii_serialization())
  };
  rec.resolved_url = Some(url.to_string());
  rec.mirror_used = mirror_used.clone();
  db.update_resolved_and_mirror(download_id, rec.resolved_url.as_deref(), mirror_used.as_deref())?;

  // Probe via HEAD
  let mut headers = HeaderMap::new();
  Transport::apply_header_rules(rules, &mut headers, &url_parsed);

  let head = client
    .head(url_parsed.clone())
    .headers(headers.clone())
    .send()
    .await;

  let (supports_ranges, content_length, etag, last_modified, content_disposition, content_type) =
    match head {
      Ok(resp) => {
        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
          set_http_error(&stats, status.as_u16(), resp.text().await.ok());
          anyhow::bail!("http {}", status.as_u16());
        }
        let headers = resp.headers();
        let supports = headers
          .get("accept-ranges")
          .and_then(|v| v.to_str().ok())
          .map(|s| s.to_ascii_lowercase().contains("bytes"));
        let len = headers
          .get("content-length")
          .and_then(|v| v.to_str().ok())
          .and_then(|s| s.parse::<i64>().ok());
        let etag = headers.get("etag").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let lm = headers
          .get("last-modified")
          .and_then(|v| v.to_str().ok())
          .map(|s| s.to_string());
        let cd = headers
          .get("content-disposition")
          .and_then(|v| v.to_str().ok())
          .map(|s| s.to_string());
        let ct = headers
          .get("content-type")
          .and_then(|v| v.to_str().ok())
          .map(|s| s.to_string());
        (supports, len, etag, lm, cd, ct)
      }
      Err(e) => {
        set_reqwest_error(&stats, &e);
        anyhow::bail!("probe failed: {e}");
      }
    };

  stats
    .total
    .store(content_length.unwrap_or(-1), Ordering::Relaxed);

  // If resuming, revalidate ETag/Last-Modified when present.
  if let (Some(prev), Some(cur)) = (rec.etag.as_deref(), etag.as_deref()) {
    if prev != cur {
      *stats.error_code.lock() = Some(ErrorCode::RemoteChanged);
      *stats.error_message.lock() = Some("Remote changed (ETag mismatch)".to_string());
      db.update_download_status(
        download_id,
        DownloadStatus::Error,
        Some("REMOTE_CHANGED"),
        Some("Remote changed (ETag mismatch)"),
      )?;
      *stats.status.lock() = DownloadStatus::Error;
      anyhow::bail!("remote changed");
    }
  }
  if let (Some(prev), Some(cur)) = (rec.last_modified.as_deref(), last_modified.as_deref()) {
    if prev != cur {
      *stats.error_code.lock() = Some(ErrorCode::RemoteChanged);
      *stats.error_message.lock() = Some("Remote changed (Last-Modified mismatch)".to_string());
      db.update_download_status(
        download_id,
        DownloadStatus::Error,
        Some("REMOTE_CHANGED"),
        Some("Remote changed (Last-Modified mismatch)"),
      )?;
      *stats.status.lock() = DownloadStatus::Error;
      anyhow::bail!("remote changed");
    }
  }

  // Decide filenames once per download (first attempt).
  if rec.final_filename.is_none() || rec.temp_path.is_none() {
    let desired = naming::filename_from_headers_and_url(
      &url_parsed,
      content_disposition.as_deref(),
      content_type.as_deref(),
    );
    let chosen = naming::choose_non_colliding_filename(Path::new(&rec.dest_dir), &desired)?;
    let temp_path = Path::new(&rec.dest_dir).join(format!(".zdmr-{download_id}.part"));

    rec.final_filename = Some(chosen.clone());
    rec.temp_path = Some(temp_path.display().to_string());
    rec.resolved_url = Some(url.to_string());
    rec.supports_ranges = supports_ranges;
    rec.content_length = content_length;
    rec.etag = etag.clone();
    rec.last_modified = last_modified.clone();
    rec.mirror_used = mirror_used;

    db.set_download_finalization(
      download_id,
      rec.resolved_url.as_deref(),
      rec.temp_path.as_deref(),
      rec.final_filename.as_deref(),
      rec.content_length,
      rec.etag.as_deref(),
      rec.last_modified.as_deref(),
      rec.supports_ranges,
      rec.mirror_used.as_deref(),
    )?;
    // Notify UI immediately so "(resolving...)" flips to the real name without waiting for another event.
    events.emit_downloads_changed();
  }

  let temp_path = PathBuf::from(rec.temp_path.clone().unwrap());
  let total = content_length.or(rec.content_length);
  stats.bytes.store(rec.bytes_downloaded, Ordering::Relaxed);
  stats.last_bytes.store(rec.bytes_downloaded, Ordering::Relaxed);

  // Create/prepare temp file.
  {
    let mut opts = OpenOptions::new();
    opts.create(true).write(true).read(true);
    let f = opts.open(&temp_path).context("failed to open temp file")?;
    if let Some(len) = total {
      if len > 0 {
        f.set_len(len as u64).ok();
      }
    }
  }

  // Decide multipart vs single.
  let do_multipart = total
    .filter(|l| *l >= 32 * 1024 * 1024) // 32MiB+
    .is_some()
    && supports_ranges.unwrap_or(false);

  if do_multipart {
    // Lightweight warmup probe to adapt initial segment concurrency based on observed throughput.
    let warmup_bps = warmup_probe_bps(&client, rules, &url_parsed).await.unwrap_or(0.0);
    if let Err(e) = download_multipart(
      db,
      client,
      rules,
      &url_parsed,
      &temp_path,
      download_id,
      total.unwrap(),
      warmup_bps,
      limiter,
      control_rx.clone(),
      stats.clone(),
    )
    .await
    {
      // If ranged requests failed, downgrade to single stream and restart safely.
      if matches!(stats.error_code.lock().clone(), Some(ErrorCode::RangeUnsupported)) {
        tracing::info!(download_id=%download_id, "downgrading to single-stream (range unsupported)");
        db.reset_download_for_retry(download_id)?;
        // Remove temp file to restart cleanly.
        let _ = std::fs::remove_file(&temp_path);
        // Re-run as single stream without ranges/resume.
        download_single(
          db,
          transport.client_for(proxy_url.as_deref())?,
          rules,
          &url_parsed,
          &temp_path,
          download_id,
          total,
          false,
          limiter,
          control_rx.clone(),
          stats.clone(),
        )
        .await?;
      } else {
        return Err(e);
      }
    }
  } else {
    download_single(
      db,
      client,
      rules,
      &url_parsed,
      &temp_path,
      download_id,
      total,
      supports_ranges.unwrap_or(false),
      limiter,
      control_rx.clone(),
      stats.clone(),
    )
    .await?;
  }

  // Finalize: rename temp to final.
  if matches!(*control_rx.borrow(), JobControl::Pause | JobControl::Cancel) {
    *stats.status.lock() = DownloadStatus::Paused;
    db.update_download_status(download_id, DownloadStatus::Paused, None, None)?;
    return Ok(());
  }

  let final_name = rec.final_filename.clone().unwrap();
  let final_path = Path::new(&rec.dest_dir).join(final_name);
  std::fs::rename(&temp_path, &final_path).context("failed to move temp file to final path")?;

  // Basic integrity: size matches expected when known.
  if let Some(len) = total {
    let meta = std::fs::metadata(&final_path).context("failed to stat final file")?;
    if meta.len() as i64 != len {
      *stats.error_code.lock() = Some(ErrorCode::Unknown);
      *stats.error_message.lock() = Some("Downloaded size mismatch".to_string());
      db.update_download_status(
        download_id,
        DownloadStatus::Error,
        Some("UNKNOWN"),
        Some("Downloaded size mismatch"),
      )?;
      anyhow::bail!("size mismatch");
    }
  }

  db.update_download_status(download_id, DownloadStatus::Completed, None, None)?;
  *stats.status.lock() = DownloadStatus::Completed;
  Ok(())
}

async fn download_single(
  db: &Db,
  client: reqwest::Client,
  rules: &crate::model::RulesSnapshot,
  url: &Url,
  temp_path: &Path,
  download_id: &str,
  content_length: Option<i64>,
  supports_ranges: bool,
  limiter: &BandwidthLimiter,
  mut control_rx: watch::Receiver<JobControl>,
  stats: RuntimeStats,
) -> anyhow::Result<()> {
  let mut headers = HeaderMap::new();
  Transport::apply_header_rules(rules, &mut headers, url);

  let mut start = db
    .get_download(download_id)?
    .map(|r| r.bytes_downloaded)
    .unwrap_or(0);
  if start < 0 {
    start = 0;
  }

  // If we have partial bytes and server supports ranges, resume; else restart.
  if start > 0 && !supports_ranges {
    start = 0;
    db.update_download_bytes(download_id, 0)?;
  }

  if start > 0 && supports_ranges {
    headers.insert(
      RANGE,
      HeaderValue::from_str(&format!("bytes={start}-")).unwrap(),
    );
  }

  let resp = client.get(url.clone()).headers(headers).send().await;
  let resp = match resp {
    Ok(r) => r,
    Err(e) => {
      set_reqwest_error(&stats, &e);
      anyhow::bail!(e);
    }
  };
  if resp.status().is_client_error() || resp.status().is_server_error() {
    set_http_error(&stats, resp.status().as_u16(), None);
    anyhow::bail!("http {}", resp.status().as_u16());
  }

  let file = OpenOptions::new().write(true).open(temp_path)?;
  let mut offset = start as u64;
  let mut bytes_total = start;

  let mut stream = resp.bytes_stream();
  let mut persist_tick = tokio::time::interval(Duration::from_secs(1));
  persist_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

  loop {
    tokio::select! {
      _ = persist_tick.tick() => {
        db.update_download_bytes(download_id, bytes_total)?;
      }
      maybe = stream.next() => {
        let Some(chunk) = maybe else { break; };
        let chunk = chunk?;
        if matches!(*control_rx.borrow(), JobControl::Pause | JobControl::Cancel) {
          db.update_download_bytes(download_id, bytes_total)?;
          db.update_download_status(download_id, DownloadStatus::Paused, None, None)?;
          *stats.status.lock() = DownloadStatus::Paused;
          return Ok(());
        }
        limiter.acquire(chunk.len()).await;
        write_at_all(&file, offset, &chunk)?;
        offset += chunk.len() as u64;
        bytes_total += chunk.len() as i64;
        stats.bytes.store(bytes_total, Ordering::Relaxed);
      }
    }
  }

  db.update_download_bytes(download_id, bytes_total)?;
  // content_length sanity when known
  if let Some(len) = content_length {
    if bytes_total != len {
      tracing::warn!(download_id=%download_id, bytes_total, len, "single download length mismatch");
    }
  }
  Ok(())
}

async fn download_multipart(
  db: &Db,
  client: reqwest::Client,
  rules: &crate::model::RulesSnapshot,
  url: &Url,
  temp_path: &Path,
  download_id: &str,
  content_length: i64,
  warmup_bps: f64,
  limiter: &BandwidthLimiter,
  control_rx: watch::Receiver<JobControl>,
  stats: RuntimeStats,
) -> anyhow::Result<()> {
  // Create or load segments.
  let existing = db.list_segments(download_id)?;
  let segments = if existing.is_empty() {
    let planned = plan_segments(content_length, warmup_bps);
    db.replace_segments(download_id, planned)?;
    db.list_segments(download_id)?
  } else {
    existing
  };

  let initial = db
    .get_download(download_id)?
    .map(|r| r.bytes_downloaded)
    .unwrap_or(0);
  stats.bytes.store(initial, Ordering::Relaxed);
  let total_bytes = stats.bytes.clone();

  let mut join_handles = Vec::new();
  for seg in segments.clone() {
    let seg_client = client.clone();
    let seg_url = url.clone();
    let seg_rules = rules.clone();
    let seg_db = db.clone();
    let seg_temp = temp_path.to_path_buf();
    let seg_download_id = download_id.to_string();
    let seg_limiter = limiter.clone();
    let seg_control = control_rx.clone();
    let total_bytes = total_bytes.clone();
    let stats2 = stats.clone();

    join_handles.push(tauri::async_runtime::spawn(async move {
      if let Err(e) = download_segment(
        &seg_db,
        seg_client,
        &seg_rules,
        &seg_url,
        &seg_temp,
        &seg_download_id,
        seg,
        &seg_limiter,
        seg_control,
        total_bytes,
        stats2,
      )
      .await
      {
        tracing::warn!(download_id=%seg_download_id, error=%e, "segment failed");
      }
    }));
  }

  // Persist progress periodically until done.
  let mut persist_tick = tokio::time::interval(Duration::from_secs(1));
  persist_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
  loop {
    tokio::select! {
      _ = persist_tick.tick() => {
        let cur = total_bytes.load(Ordering::Relaxed);
        db.update_download_bytes(download_id, cur)?;
        // segment bytes are updated inside segment loop every tick as well.
      }
      _ = tokio::time::sleep(Duration::from_millis(200)) => {
        if matches!(*control_rx.borrow(), JobControl::Pause | JobControl::Cancel) {
          let cur = total_bytes.load(Ordering::Relaxed);
          db.update_download_bytes(download_id, cur)?;
          db.update_download_status(download_id, DownloadStatus::Paused, None, None)?;
          *stats.status.lock() = DownloadStatus::Paused;
          return Ok(());
        }
        let segs = db.list_segments(download_id)?;
        if segs.iter().any(|s| s.status == "ERROR") {
          // A segment errored: treat as range failure and let caller downgrade.
          *stats.error_code.lock() = Some(ErrorCode::RangeUnsupported);
          *stats.error_message.lock() = Some("Segmented download failed (range unsupported)".to_string());
          anyhow::bail!("segment error");
        }
        let all_done = segs.iter().all(|s| s.status == "COMPLETED");
        if all_done {
          let cur = total_bytes.load(Ordering::Relaxed);
          db.update_download_bytes(download_id, cur)?;
          break;
        }
      }
    }
  }

  for h in join_handles {
    let _ = h.await;
  }

  Ok(())
}

fn plan_segments(content_length: i64, warmup_bps: f64) -> Vec<SegmentRow> {
  // Default segment size 16MiB; cap concurrency 16. Adjust concurrency based on observed warmup throughput.
  let seg_size: i64 = 16 * 1024 * 1024;
  let base = ((content_length + seg_size - 1) / seg_size).clamp(2, 16) as i64;

  let desired = if warmup_bps <= 0.0 {
    base
  } else if warmup_bps > 20.0 * 1024.0 * 1024.0 {
    base.max(8).min(16)
  } else if warmup_bps > 8.0 * 1024.0 * 1024.0 {
    base.max(6).min(12)
  } else if warmup_bps > 3.0 * 1024.0 * 1024.0 {
    base.max(4).min(8)
  } else {
    base.min(4)
  };
  let count = desired as i64;

  let mut segs = Vec::new();
  for i in 0..count {
    let start = i * seg_size;
    let mut end = ((i + 1) * seg_size) - 1;
    if i == count - 1 {
      end = content_length - 1;
    }
    segs.push(SegmentRow {
      range_start: start,
      range_end: end,
      bytes_done: 0,
      status: "ACTIVE".to_string(),
      last_error: None,
    });
  }
  segs
}

async fn warmup_probe_bps(
  client: &reqwest::Client,
  rules: &crate::model::RulesSnapshot,
  url: &Url,
) -> Option<f64> {
  let mut headers = HeaderMap::new();
  Transport::apply_header_rules(rules, &mut headers, url);
  headers.insert(RANGE, HeaderValue::from_static("bytes=0-1048575"));
  let start = Instant::now();
  let resp = client.get(url.clone()).headers(headers).send().await.ok()?;
  if resp.status().as_u16() != 206 {
    return None;
  }
  let mut bytes: usize = 0;
  let mut stream = resp.bytes_stream();
  while let Some(chunk) = stream.next().await {
    let chunk = chunk.ok()?;
    bytes += chunk.len();
    if bytes >= 1024 * 1024 {
      break;
    }
  }
  let elapsed = start.elapsed().as_secs_f64();
  if elapsed <= 0.0 {
    return None;
  }
  Some((bytes as f64) / elapsed)
}

async fn download_segment(
  db: &Db,
  client: reqwest::Client,
  rules: &crate::model::RulesSnapshot,
  url: &Url,
  temp_path: &Path,
  _download_id: &str,
  seg: SegmentRowWithId,
  limiter: &BandwidthLimiter,
  mut control_rx: watch::Receiver<JobControl>,
  total_bytes: Arc<AtomicI64>,
  stats: RuntimeStats,
) -> anyhow::Result<()> {
  if seg.status == "COMPLETED" {
    return Ok(());
  }

  let mut headers = HeaderMap::new();
  Transport::apply_header_rules(rules, &mut headers, url);

  let start = seg.range_start + seg.bytes_done;
  if start > seg.range_end {
    db.update_segment_bytes(seg.id, seg.bytes_done, "COMPLETED", None)?;
    return Ok(());
  }
  headers.insert(
    RANGE,
    HeaderValue::from_str(&format!("bytes={start}-{}", seg.range_end)).unwrap(),
  );

  let resp = client.get(url.clone()).headers(headers).send().await;
  let resp = match resp {
    Ok(r) => r,
    Err(e) => {
      set_reqwest_error(&stats, &e);
      db.update_segment_bytes(seg.id, seg.bytes_done, "ERROR", Some(&e.to_string()))?;
      anyhow::bail!(e);
    }
  };

  if resp.status().as_u16() != 206 {
    // Range not supported or server downgraded. We treat as retryable: engine can fall back to single on retry.
    *stats.error_code.lock() = Some(ErrorCode::RangeUnsupported);
    *stats.error_message.lock() = Some("Server does not support ranged requests".to_string());
    db.update_segment_bytes(seg.id, seg.bytes_done, "ERROR", Some("range unsupported"))?;
    anyhow::bail!("range unsupported");
  }

  let file = OpenOptions::new().write(true).open(temp_path)?;
  let mut offset = start as u64;
  let mut bytes_done = seg.bytes_done;

  let mut stream = resp.bytes_stream();
  let mut persist_tick = tokio::time::interval(Duration::from_secs(1));
  persist_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

  loop {
    tokio::select! {
      _ = persist_tick.tick() => {
        db.update_segment_bytes(seg.id, bytes_done, "ACTIVE", None)?;
      }
      maybe = stream.next() => {
        let Some(chunk) = maybe else { break; };
        let chunk = chunk?;
        if matches!(*control_rx.borrow(), JobControl::Pause | JobControl::Cancel) {
          db.update_segment_bytes(seg.id, bytes_done, "ACTIVE", None)?;
          return Ok(());
        }
        limiter.acquire(chunk.len()).await;
        write_at_all(&file, offset, &chunk)?;
        offset += chunk.len() as u64;
        bytes_done += chunk.len() as i64;
        db.update_segment_bytes(seg.id, bytes_done, "ACTIVE", None).ok();
        total_bytes.fetch_add(chunk.len() as i64, Ordering::Relaxed);
      }
    }
  }

  db.update_segment_bytes(seg.id, bytes_done, "COMPLETED", None)?;
  Ok(())
}

fn set_http_error(stats: &RuntimeStats, status: u16, body: Option<String>) {
  let code = if (400..500).contains(&status) {
    ErrorCode::Http4xx
  } else if (500..600).contains(&status) {
    ErrorCode::Http5xx
  } else {
    ErrorCode::Unknown
  };
  *stats.error_code.lock() = Some(code);
  *stats.error_message.lock() = Some(body.unwrap_or_else(|| format!("HTTP {status}")));
}

fn set_reqwest_error(stats: &RuntimeStats, err: &reqwest::Error) {
  let code = if err.is_timeout() {
    ErrorCode::Timeout
  } else if err.is_connect() {
    ErrorCode::ConnectFail
  } else if err.is_request() {
    ErrorCode::Unknown
  } else {
    ErrorCode::Unknown
  };
  *stats.error_code.lock() = Some(code);
  *stats.error_message.lock() = Some(err.to_string());
}

fn format_code(code: ErrorCode) -> &'static str {
  use ErrorCode::*;
  match code {
    DnsFail => "DNS_FAIL",
    ConnectFail => "CONNECT_FAIL",
    TlsFail => "TLS_FAIL",
    Http4xx => "HTTP_4XX",
    Http5xx => "HTTP_5XX",
    Timeout => "TIMEOUT",
    RangeUnsupported => "RANGE_UNSUPPORTED",
    DiskFull => "DISK_FULL",
    RemoteChanged => "REMOTE_CHANGED",
    PermissionDenied => "PERMISSION_DENIED",
    Cancelled => "CANCELLED",
    InvalidUrl => "INVALID_URL",
    Unknown => "UNKNOWN",
  }
}


