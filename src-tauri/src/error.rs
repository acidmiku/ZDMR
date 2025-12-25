use crate::app_state::AppPaths;
use std::sync::OnceLock;
use tracing_appender::non_blocking::WorkerGuard;

static LOG_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
  DnsFail,
  ConnectFail,
  TlsFail,
  Http4xx,
  Http5xx,
  Timeout,
  RangeUnsupported,
  DiskFull,
  RemoteChanged,
  PermissionDenied,
  Cancelled,
  InvalidUrl,
  Unknown,
}

impl ErrorCode {
  pub fn is_retryable(&self) -> bool {
    matches!(
      self,
      ErrorCode::DnsFail
        | ErrorCode::ConnectFail
        | ErrorCode::TlsFail
        | ErrorCode::Http5xx
        | ErrorCode::Timeout
        | ErrorCode::RangeUnsupported
    )
  }
}

pub fn init_tracing(paths: &AppPaths) -> anyhow::Result<()> {
  // Rotate daily; keep logs in app data dir so “Open logs folder” is deterministic.
  let file_appender = tracing_appender::rolling::daily(&paths.logs_dir, "zdmr.jsonl");
  let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
  let _ = LOG_GUARD.set(guard);

  let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,reqwest=warn,hyper=warn"));

  tracing_subscriber::fmt()
    .with_env_filter(env_filter)
    .with_writer(non_blocking)
    .json()
    .with_current_span(true)
    .with_span_list(true)
    .init();

  Ok(())
}


