use crate::error::ErrorCode;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DownloadStatus {
  Queued,
  Downloading,
  Paused,
  Completed,
  Error,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadRecord {
  pub id: String,
  pub created_at: String,
  pub updated_at: String,
  pub started_at: Option<String>,
  pub completed_at: Option<String>,
  pub forced_proxy: bool,
  pub forced_proxy_url: Option<String>,
  pub original_url: String,
  pub resolved_url: Option<String>,
  pub dest_dir: String,
  pub final_filename: Option<String>,
  pub temp_path: Option<String>,
  pub status: DownloadStatus,
  pub error_code: Option<ErrorCode>,
  pub error_message: Option<String>,
  pub content_length: Option<i64>,
  pub etag: Option<String>,
  pub last_modified: Option<String>,
  pub bytes_downloaded: i64,
  pub supports_ranges: Option<bool>,
  pub mirror_used: Option<String>,
  pub batch_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadProgressUpdate {
  pub id: String,
  pub status: DownloadStatus,
  pub bytes_downloaded: i64,
  pub content_length: Option<i64>,
  pub speed_bps: f64,
  pub eta_seconds: Option<f64>,
  #[serde(default)]
  pub status_detail: Option<String>,
  pub error_code: Option<ErrorCode>,
  pub error_message: Option<String>,
  pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SettingsSnapshot {
  pub default_download_dir: String,
  pub bandwidth_limit_bps: Option<i64>,
  pub minimize_to_tray: bool,
  pub theme: String,
  pub skin: String,
  pub global_hotkey: String,
  pub global_proxy_enabled: bool,
  pub global_proxy_url: Option<String>,
  pub local_api_port: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProxyRule {
  pub id: i64,
  pub pattern: String,
  pub enabled: bool,
  pub use_proxy: bool,
  pub proxy_url_override: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HeaderRule {
  pub id: i64,
  pub pattern: String,
  pub enabled: bool,
  pub headers_json: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MirrorRule {
  pub id: i64,
  pub pattern: String,
  pub enabled: bool,
  pub candidates_json: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RulesSnapshot {
  pub proxy_rules: Vec<ProxyRule>,
  pub header_rules: Vec<HeaderRule>,
  pub mirror_rules: Vec<MirrorRule>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NewBatchRequest {
  pub name: Option<String>,
  pub dest_dir: String,
  pub raw_url_list: Option<String>,
  pub urls: Vec<String>,
  pub download_through_proxy: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AddDownloadsRequest {
  pub urls: Vec<String>,
  pub dest_dir: Option<String>,
}


