use crate::{
  app_state::AppPaths,
  model::{
    DownloadRecord, DownloadStatus, HeaderRule, MirrorRule, ProxyRule, RulesSnapshot,
    SettingsSnapshot,
  },
};
use anyhow::Context;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

#[derive(Clone)]
pub struct Db {
  path: Arc<PathBuf>,
  // We keep a single connection behind a mutex; DB work is small and we do hot-path updates
  // via in-memory state + periodic persistence (implemented in the engine).
  conn: Arc<Mutex<Connection>>,
}

impl Db {
  pub fn open(path: PathBuf) -> anyhow::Result<Self> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).context("failed to create db parent dir")?;
    }
    let conn = Connection::open(&path).context("failed to open sqlite db")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(Self {
      path: Arc::new(path),
      conn: Arc::new(Mutex::new(conn)),
    })
  }

  pub fn init_schema(&self) -> anyhow::Result<()> {
    let sql = r#"
      CREATE TABLE IF NOT EXISTS downloads (
        id TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        started_at TEXT,
        completed_at TEXT,
        forced_proxy INTEGER NOT NULL DEFAULT 0,
        forced_proxy_url TEXT,
        original_url TEXT NOT NULL,
        resolved_url TEXT,
        dest_dir TEXT NOT NULL,
        final_filename TEXT,
        temp_path TEXT,
        status TEXT NOT NULL,
        error_code TEXT,
        error_message TEXT,
        content_length INTEGER,
        etag TEXT,
        last_modified TEXT,
        bytes_downloaded INTEGER NOT NULL DEFAULT 0,
        supports_ranges INTEGER,
        use_proxy_mode TEXT,
        mirror_used TEXT,
        batch_id TEXT,
        FOREIGN KEY(batch_id) REFERENCES batches(id)
      );

      CREATE TABLE IF NOT EXISTS download_segments (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        download_id TEXT NOT NULL,
        range_start INTEGER NOT NULL,
        range_end INTEGER NOT NULL,
        bytes_done INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL,
        last_error TEXT,
        FOREIGN KEY(download_id) REFERENCES downloads(id) ON DELETE CASCADE
      );

      CREATE TABLE IF NOT EXISTS batches (
        id TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        name TEXT,
        dest_dir TEXT NOT NULL,
        raw_url_list TEXT,
        status TEXT
      );

      CREATE TABLE IF NOT EXISTS settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      );

      CREATE TABLE IF NOT EXISTS proxy_rules (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pattern TEXT NOT NULL,
        enabled INTEGER NOT NULL,
        use_proxy INTEGER NOT NULL,
        proxy_url_override TEXT
      );

      CREATE TABLE IF NOT EXISTS header_rules (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pattern TEXT NOT NULL,
        enabled INTEGER NOT NULL,
        headers_json TEXT NOT NULL
      );

      CREATE TABLE IF NOT EXISTS mirror_rules (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pattern TEXT NOT NULL,
        enabled INTEGER NOT NULL,
        candidates_json TEXT NOT NULL
      );

      CREATE INDEX IF NOT EXISTS idx_downloads_status_updated ON downloads(status, updated_at);
      CREATE INDEX IF NOT EXISTS idx_segments_by_download ON download_segments(download_id);
    "#;

    let conn = self.conn.lock();
    conn.execute_batch(sql).context("failed to initialize schema")?;
    // Lightweight migration for existing DBs.
    let _ = conn.execute(r#"ALTER TABLE downloads ADD COLUMN started_at TEXT"#, []);
    let _ = conn.execute(r#"ALTER TABLE downloads ADD COLUMN completed_at TEXT"#, []);
    let _ = conn.execute(r#"ALTER TABLE downloads ADD COLUMN forced_proxy INTEGER NOT NULL DEFAULT 0"#, []);
    let _ = conn.execute(r#"ALTER TABLE downloads ADD COLUMN forced_proxy_url TEXT"#, []);
    Ok(())
  }

  fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
      .format(&time::format_description::well_known::Rfc3339)
      .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
  }

  pub fn list_downloads(&self) -> anyhow::Result<Vec<DownloadRecord>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
        SELECT
          id, created_at, updated_at, started_at, completed_at, forced_proxy, forced_proxy_url,
          original_url, resolved_url, dest_dir, final_filename,
          temp_path, status, error_code, error_message, content_length, etag, last_modified,
          bytes_downloaded, supports_ranges, mirror_used, batch_id
        FROM downloads
        ORDER BY created_at DESC
      "#,
    )?;

    let rows = stmt.query_map([], |row| {
      let status_str: String = row.get(12)?;
      let status = parse_status(&status_str);
      let error_code: Option<String> = row.get(13)?;
      Ok(DownloadRecord {
        id: row.get(0)?,
        created_at: row.get(1)?,
        updated_at: row.get(2)?,
        started_at: row.get(3)?,
        completed_at: row.get(4)?,
        forced_proxy: row.get::<_, i64>(5)? != 0,
        forced_proxy_url: row.get(6)?,
        original_url: row.get(7)?,
        resolved_url: row.get(8)?,
        dest_dir: row.get(9)?,
        final_filename: row.get(10)?,
        temp_path: row.get(11)?,
        status,
        error_code: error_code.and_then(parse_error_code),
        error_message: row.get(14)?,
        content_length: row.get(15)?,
        etag: row.get(16)?,
        last_modified: row.get(17)?,
        bytes_downloaded: row.get(18)?,
        supports_ranges: row
          .get::<_, Option<i64>>(19)?
          .map(|v| v != 0),
        mirror_used: row.get(20)?,
        batch_id: row.get(21)?,
      })
    })?;

    let mut out = Vec::new();
    for r in rows {
      out.push(r?);
    }
    Ok(out)
  }

  pub fn get_download(&self, id: &str) -> anyhow::Result<Option<DownloadRecord>> {
    let conn = self.conn.lock();
    conn
      .query_row(
        r#"
          SELECT
            id, created_at, updated_at, started_at, completed_at, forced_proxy, forced_proxy_url,
            original_url, resolved_url, dest_dir, final_filename,
            temp_path, status, error_code, error_message, content_length, etag, last_modified,
            bytes_downloaded, supports_ranges, mirror_used, batch_id
          FROM downloads
          WHERE id=?1
        "#,
        params![id],
        |row| {
          let status_str: String = row.get(12)?;
          let status = parse_status(&status_str);
          let error_code: Option<String> = row.get(13)?;
          Ok(DownloadRecord {
            id: row.get(0)?,
            created_at: row.get(1)?,
            updated_at: row.get(2)?,
            started_at: row.get(3)?,
            completed_at: row.get(4)?,
            forced_proxy: row.get::<_, i64>(5)? != 0,
            forced_proxy_url: row.get(6)?,
            original_url: row.get(7)?,
            resolved_url: row.get(8)?,
            dest_dir: row.get(9)?,
            final_filename: row.get(10)?,
            temp_path: row.get(11)?,
            status,
            error_code: error_code.and_then(parse_error_code),
            error_message: row.get(14)?,
            content_length: row.get(15)?,
            etag: row.get(16)?,
            last_modified: row.get(17)?,
            bytes_downloaded: row.get(18)?,
            supports_ranges: row
              .get::<_, Option<i64>>(19)?
              .map(|v| v != 0),
            mirror_used: row.get(20)?,
            batch_id: row.get(21)?,
          })
        },
      )
      .optional()
      .context("failed to load download")
  }

  pub fn recover_incomplete_downloads(&self) -> anyhow::Result<()> {
    // Anything that was DOWNLOADING when the app died gets put back to PAUSED.
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"UPDATE downloads SET updated_at=?1, status='PAUSED' WHERE status='DOWNLOADING'"#,
      params![now],
    )?;
    Ok(())
  }

  pub fn insert_download_skeleton(
    &self,
    id: &str,
    original_url: &str,
    dest_dir: &str,
    forced_proxy: bool,
    forced_proxy_url: Option<&str>,
  ) -> anyhow::Result<()> {
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"
        INSERT INTO downloads (
          id, created_at, updated_at, forced_proxy, forced_proxy_url, original_url, dest_dir, status, bytes_downloaded
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)
      "#,
      params![
        id,
        now,
        now,
        if forced_proxy { 1 } else { 0 },
        forced_proxy_url,
        original_url,
        dest_dir,
        "QUEUED"
      ],
    )?;
    Ok(())
  }

  pub fn delete_completed_downloads(&self) -> anyhow::Result<usize> {
    let conn = self.conn.lock();
    let n = conn.execute(r#"DELETE FROM downloads WHERE status='COMPLETED'"#, params![])?;
    Ok(n)
  }

  pub fn update_download_status(
    &self,
    id: &str,
    status: DownloadStatus,
    error_code: Option<&str>,
    error_message: Option<&str>,
  ) -> anyhow::Result<()> {
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    match status {
      DownloadStatus::Downloading => {
        conn.execute(
          r#"
            UPDATE downloads
            SET updated_at=?2,
                status=?3,
                error_code=?4,
                error_message=?5,
                started_at=COALESCE(started_at, ?2),
                completed_at=NULL
            WHERE id=?1
          "#,
          params![id, now, status_to_str(status), error_code, error_message],
        )?;
      }
      DownloadStatus::Completed => {
        conn.execute(
          r#"
            UPDATE downloads
            SET updated_at=?2,
                status=?3,
                error_code=?4,
                error_message=?5,
                completed_at=?2
            WHERE id=?1
          "#,
          params![id, now, status_to_str(status), error_code, error_message],
        )?;
      }
      _ => {
        conn.execute(
          r#"
            UPDATE downloads
            SET updated_at=?2, status=?3, error_code=?4, error_message=?5
            WHERE id=?1
          "#,
          params![id, now, status_to_str(status), error_code, error_message],
        )?;
      }
    }
    Ok(())
  }

  pub fn set_download_finalization(
    &self,
    id: &str,
    resolved_url: Option<&str>,
    temp_path: Option<&str>,
    final_filename: Option<&str>,
    content_length: Option<i64>,
    etag: Option<&str>,
    last_modified: Option<&str>,
    supports_ranges: Option<bool>,
    mirror_used: Option<&str>,
  ) -> anyhow::Result<()> {
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"
        UPDATE downloads
        SET updated_at=?2,
            resolved_url=?3,
            temp_path=?4,
            final_filename=?5,
            content_length=?6,
            etag=?7,
            last_modified=?8,
            supports_ranges=?9,
            mirror_used=?10
        WHERE id=?1
      "#,
      params![
        id,
        now,
        resolved_url,
        temp_path,
        final_filename,
        content_length,
        etag,
        last_modified,
        supports_ranges.map(|b| if b { 1 } else { 0 }),
        mirror_used
      ],
    )?;
    Ok(())
  }

  pub fn update_download_bytes(&self, id: &str, bytes_downloaded: i64) -> anyhow::Result<()> {
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"UPDATE downloads SET updated_at=?2, bytes_downloaded=?3 WHERE id=?1"#,
      params![id, now, bytes_downloaded],
    )?;
    Ok(())
  }

  pub fn update_resolved_and_mirror(&self, id: &str, resolved_url: Option<&str>, mirror_used: Option<&str>) -> anyhow::Result<()> {
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"UPDATE downloads SET updated_at=?2, resolved_url=?3, mirror_used=?4 WHERE id=?1"#,
      params![id, now, resolved_url, mirror_used],
    )?;
    Ok(())
  }

  pub fn reset_download_for_retry(&self, id: &str) -> anyhow::Result<()> {
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"
        UPDATE downloads
        SET updated_at=?2,
            status='QUEUED',
            error_code=NULL,
            error_message=NULL,
            bytes_downloaded=0,
            supports_ranges=NULL,
            mirror_used=NULL
        WHERE id=?1
      "#,
      params![id, now],
    )?;
    conn.execute(r#"DELETE FROM download_segments WHERE download_id=?1"#, params![id])?;
    Ok(())
  }

  pub fn replace_segments(&self, download_id: &str, segments: Vec<SegmentRow>) -> anyhow::Result<()> {
    let conn = self.conn.lock();
    conn.execute(r#"DELETE FROM download_segments WHERE download_id=?1"#, params![download_id])?;
    let mut stmt = conn.prepare(
      r#"
        INSERT INTO download_segments (download_id, range_start, range_end, bytes_done, status, last_error)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
      "#,
    )?;
    for s in segments {
      stmt.execute(params![
        download_id,
        s.range_start,
        s.range_end,
        s.bytes_done,
        s.status,
        s.last_error
      ])?;
    }
    Ok(())
  }

  pub fn list_segments(&self, download_id: &str) -> anyhow::Result<Vec<SegmentRowWithId>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
        SELECT id, range_start, range_end, bytes_done, status, last_error
        FROM download_segments
        WHERE download_id=?1
        ORDER BY range_start ASC
      "#,
    )?;
    let rows = stmt.query_map(params![download_id], |r| {
      Ok(SegmentRowWithId {
        id: r.get(0)?,
        range_start: r.get(1)?,
        range_end: r.get(2)?,
        bytes_done: r.get(3)?,
        status: r.get(4)?,
        last_error: r.get(5)?,
      })
    })?;
    let mut out = Vec::new();
    for r in rows {
      out.push(r?);
    }
    Ok(out)
  }

  pub fn update_segment_bytes(&self, segment_id: i64, bytes_done: i64, status: &str, last_error: Option<&str>) -> anyhow::Result<()> {
    let conn = self.conn.lock();
    conn.execute(
      r#"UPDATE download_segments SET bytes_done=?2, status=?3, last_error=?4 WHERE id=?1"#,
      params![segment_id, bytes_done, status, last_error],
    )?;
    Ok(())
  }

  pub fn delete_download(&self, id: &str) -> anyhow::Result<()> {
    let conn = self.conn.lock();
    conn.execute(r#"DELETE FROM downloads WHERE id=?1"#, params![id])?;
    Ok(())
  }

  pub fn insert_batch(&self, dest_dir: &str, name: Option<&str>, raw_url_list: Option<&str>) -> anyhow::Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"
        INSERT INTO batches (id, created_at, name, dest_dir, raw_url_list, status)
        VALUES (?1, ?2, ?3, ?4, ?5, 'CREATED')
      "#,
      params![id, now, name, dest_dir, raw_url_list],
    )?;
    Ok(id)
  }

  pub fn attach_download_to_batch(&self, download_id: &str, batch_id: &str) -> anyhow::Result<()> {
    let now = Self::now_rfc3339();
    let conn = self.conn.lock();
    conn.execute(
      r#"UPDATE downloads SET updated_at=?2, batch_id=?3 WHERE id=?1"#,
      params![download_id, now, batch_id],
    )?;
    Ok(())
  }

  fn get_setting_raw(&self, key: &str) -> anyhow::Result<Option<String>> {
    let conn = self.conn.lock();
    let v: Option<String> = conn
      .query_row(r#"SELECT value FROM settings WHERE key=?1"#, params![key], |r| r.get(0))
      .optional()?;
    Ok(v)
  }

  fn set_setting_raw(&self, key: &str, value: &str) -> anyhow::Result<()> {
    let conn = self.conn.lock();
    conn.execute(
      r#"INSERT INTO settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value"#,
      params![key, value],
    )?;
    Ok(())
  }

  pub fn get_settings_snapshot(&self) -> anyhow::Result<SettingsSnapshot> {
    Ok(SettingsSnapshot {
      default_download_dir: self
        .get_setting_raw("default_download_dir")?
        .unwrap_or_else(|| "".to_string()),
      bandwidth_limit_bps: self
        .get_setting_raw("bandwidth_limit_bps")?
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| *v > 0),
      minimize_to_tray: self
        .get_setting_raw("minimize_to_tray")?
        .map(|s| s == "1")
        .unwrap_or(true),
      theme: self
        .get_setting_raw("theme")?
        .unwrap_or_else(|| "dark".to_string()),
      global_proxy_enabled: self
        .get_setting_raw("global_proxy_enabled")?
        .map(|s| s == "1")
        .unwrap_or(false),
      global_proxy_url: self
        .get_setting_raw("global_proxy_url")?
        .and_then(|s| if s.trim().is_empty() { None } else { Some(s) }),
      local_api_port: self
        .get_setting_raw("local_api_port")?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(17777),
    })
  }

  pub fn set_settings_snapshot(&self, s: &SettingsSnapshot) -> anyhow::Result<()> {
    self.set_setting_raw("default_download_dir", &s.default_download_dir)?;
    self.set_setting_raw(
      "bandwidth_limit_bps",
      &s.bandwidth_limit_bps.unwrap_or(0).to_string(),
    )?;
    self.set_setting_raw("minimize_to_tray", if s.minimize_to_tray { "1" } else { "0" })?;
    self.set_setting_raw("theme", &s.theme)?;
    self.set_setting_raw("global_proxy_enabled", if s.global_proxy_enabled { "1" } else { "0" })?;
    self.set_setting_raw("global_proxy_url", s.global_proxy_url.as_deref().unwrap_or(""))?;
    self.set_setting_raw("local_api_port", &s.local_api_port.to_string())?;
    Ok(())
  }

  pub fn get_local_api_token(&self) -> anyhow::Result<String> {
    if let Some(v) = self.get_setting_raw("local_api_token")? {
      if !v.is_empty() {
        return Ok(v);
      }
    }
    let token = Uuid::new_v4().to_string();
    self.set_setting_raw("local_api_token", &token)?;
    Ok(token)
  }

  pub fn list_rules(&self) -> anyhow::Result<RulesSnapshot> {
    let conn = self.conn.lock();

    let mut proxy_stmt = conn.prepare(
      r#"SELECT id, pattern, enabled, use_proxy, proxy_url_override FROM proxy_rules ORDER BY id DESC"#,
    )?;
    let proxy_rows = proxy_stmt.query_map([], |r| {
      Ok(ProxyRule {
        id: r.get(0)?,
        pattern: r.get(1)?,
        enabled: r.get::<_, i64>(2)? != 0,
        use_proxy: r.get::<_, i64>(3)? != 0,
        proxy_url_override: r.get(4)?,
      })
    })?;
    let mut proxy_rules = Vec::new();
    for r in proxy_rows {
      proxy_rules.push(r?);
    }

    let mut header_stmt =
      conn.prepare(r#"SELECT id, pattern, enabled, headers_json FROM header_rules ORDER BY id DESC"#)?;
    let header_rows = header_stmt.query_map([], |r| {
      let raw: String = r.get(3)?;
      Ok(HeaderRule {
        id: r.get(0)?,
        pattern: r.get(1)?,
        enabled: r.get::<_, i64>(2)? != 0,
        headers_json: serde_json::from_str(&raw).unwrap_or(serde_json::json!({})),
      })
    })?;
    let mut header_rules = Vec::new();
    for r in header_rows {
      header_rules.push(r?);
    }

    let mut mirror_stmt =
      conn.prepare(r#"SELECT id, pattern, enabled, candidates_json FROM mirror_rules ORDER BY id DESC"#)?;
    let mirror_rows = mirror_stmt.query_map([], |r| {
      let raw: String = r.get(3)?;
      Ok(MirrorRule {
        id: r.get(0)?,
        pattern: r.get(1)?,
        enabled: r.get::<_, i64>(2)? != 0,
        candidates_json: serde_json::from_str(&raw).unwrap_or(serde_json::json!([])),
      })
    })?;
    let mut mirror_rules = Vec::new();
    for r in mirror_rows {
      mirror_rules.push(r?);
    }

    Ok(RulesSnapshot {
      proxy_rules,
      header_rules,
      mirror_rules,
    })
  }

  pub fn upsert_proxy_rule(
    &self,
    id: Option<i64>,
    pattern: &str,
    enabled: bool,
    use_proxy: bool,
    proxy_url_override: Option<&str>,
  ) -> anyhow::Result<i64> {
    let conn = self.conn.lock();
    let enabled_i = if enabled { 1 } else { 0 };
    let use_proxy_i = if use_proxy { 1 } else { 0 };
    if let Some(id) = id {
      conn.execute(
        r#"UPDATE proxy_rules SET pattern=?2, enabled=?3, use_proxy=?4, proxy_url_override=?5 WHERE id=?1"#,
        params![id, pattern, enabled_i, use_proxy_i, proxy_url_override],
      )?;
      Ok(id)
    } else {
      conn.execute(
        r#"INSERT INTO proxy_rules(pattern, enabled, use_proxy, proxy_url_override) VALUES(?1, ?2, ?3, ?4)"#,
        params![pattern, enabled_i, use_proxy_i, proxy_url_override],
      )?;
      Ok(conn.last_insert_rowid())
    }
  }

  pub fn delete_proxy_rule(&self, id: i64) -> anyhow::Result<()> {
    let conn = self.conn.lock();
    conn.execute(r#"DELETE FROM proxy_rules WHERE id=?1"#, params![id])?;
    Ok(())
  }

  pub fn upsert_header_rule(
    &self,
    id: Option<i64>,
    pattern: &str,
    enabled: bool,
    headers_json: &serde_json::Value,
  ) -> anyhow::Result<i64> {
    let conn = self.conn.lock();
    let enabled_i = if enabled { 1 } else { 0 };
    let raw = serde_json::to_string(headers_json)?;
    if let Some(id) = id {
      conn.execute(
        r#"UPDATE header_rules SET pattern=?2, enabled=?3, headers_json=?4 WHERE id=?1"#,
        params![id, pattern, enabled_i, raw],
      )?;
      Ok(id)
    } else {
      conn.execute(
        r#"INSERT INTO header_rules(pattern, enabled, headers_json) VALUES(?1, ?2, ?3)"#,
        params![pattern, enabled_i, raw],
      )?;
      Ok(conn.last_insert_rowid())
    }
  }

  pub fn delete_header_rule(&self, id: i64) -> anyhow::Result<()> {
    let conn = self.conn.lock();
    conn.execute(r#"DELETE FROM header_rules WHERE id=?1"#, params![id])?;
    Ok(())
  }

  pub fn upsert_mirror_rule(
    &self,
    id: Option<i64>,
    pattern: &str,
    enabled: bool,
    candidates_json: &serde_json::Value,
  ) -> anyhow::Result<i64> {
    let conn = self.conn.lock();
    let enabled_i = if enabled { 1 } else { 0 };
    let raw = serde_json::to_string(candidates_json)?;
    if let Some(id) = id {
      conn.execute(
        r#"UPDATE mirror_rules SET pattern=?2, enabled=?3, candidates_json=?4 WHERE id=?1"#,
        params![id, pattern, enabled_i, raw],
      )?;
      Ok(id)
    } else {
      conn.execute(
        r#"INSERT INTO mirror_rules(pattern, enabled, candidates_json) VALUES(?1, ?2, ?3)"#,
        params![pattern, enabled_i, raw],
      )?;
      Ok(conn.last_insert_rowid())
    }
  }

  pub fn delete_mirror_rule(&self, id: i64) -> anyhow::Result<()> {
    let conn = self.conn.lock();
    conn.execute(r#"DELETE FROM mirror_rules WHERE id=?1"#, params![id])?;
    Ok(())
  }
}

fn parse_status(s: &str) -> DownloadStatus {
  match s {
    "QUEUED" => DownloadStatus::Queued,
    "DOWNLOADING" => DownloadStatus::Downloading,
    "PAUSED" => DownloadStatus::Paused,
    "COMPLETED" => DownloadStatus::Completed,
    "ERROR" => DownloadStatus::Error,
    _ => DownloadStatus::Error,
  }
}

fn status_to_str(s: DownloadStatus) -> &'static str {
  match s {
    DownloadStatus::Queued => "QUEUED",
    DownloadStatus::Downloading => "DOWNLOADING",
    DownloadStatus::Paused => "PAUSED",
    DownloadStatus::Completed => "COMPLETED",
    DownloadStatus::Error => "ERROR",
  }
}

fn parse_error_code(s: String) -> Option<crate::error::ErrorCode> {
  use crate::error::ErrorCode::*;
  let v = match s.as_str() {
    "DNS_FAIL" => DnsFail,
    "CONNECT_FAIL" => ConnectFail,
    "TLS_FAIL" => TlsFail,
    "HTTP_4XX" => Http4xx,
    "HTTP_5XX" => Http5xx,
    "TIMEOUT" => Timeout,
    "RANGE_UNSUPPORTED" => RangeUnsupported,
    "DISK_FULL" => DiskFull,
    "REMOTE_CHANGED" => RemoteChanged,
    "PERMISSION_DENIED" => PermissionDenied,
    "CANCELLED" => Cancelled,
    "INVALID_URL" => InvalidUrl,
    _ => Unknown,
  };
  Some(v)
}

#[derive(Clone)]
pub struct SettingsStore {
  db: Db,
}

#[derive(Debug, Clone)]
pub struct SegmentRow {
  pub range_start: i64,
  pub range_end: i64,
  pub bytes_done: i64,
  pub status: String,
  pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SegmentRowWithId {
  pub id: i64,
  pub range_start: i64,
  pub range_end: i64,
  pub bytes_done: i64,
  pub status: String,
  pub last_error: Option<String>,
}

impl SettingsStore {
  pub fn new(db: Db) -> Self {
    Self { db }
  }

  pub fn get_snapshot(&self) -> anyhow::Result<SettingsSnapshot> {
    self.db.get_settings_snapshot()
  }

  pub fn set_snapshot(&self, s: &SettingsSnapshot) -> anyhow::Result<()> {
    self.db.set_settings_snapshot(s)
  }

  pub fn ensure_bootstrap_defaults(&self, paths: &AppPaths, default_download_dir: PathBuf) -> anyhow::Result<()> {
    // Default download dir: use OS downloads dir if we can resolve it, else a folder in app data.
    let fallback = paths.app_data_dir.join("downloads");
    let dd = if default_download_dir.as_os_str().is_empty() {
      fallback
    } else {
      default_download_dir
    };
    std::fs::create_dir_all(&dd).ok();

    let mut snap = self.db.get_settings_snapshot()?;
    if snap.default_download_dir.is_empty() {
      snap.default_download_dir = dd.display().to_string();
    }
    if snap.theme.trim().is_empty() {
      snap.theme = "dark".to_string();
    }
    if snap.local_api_port <= 0 {
      snap.local_api_port = 17777;
    }
    self.db.set_settings_snapshot(&snap)?;
    let _token = self.db.get_local_api_token()?;
    Ok(())
  }
}


