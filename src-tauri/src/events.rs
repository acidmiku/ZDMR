use crate::model::DownloadProgressUpdate;
use tokio::sync::broadcast;

pub const EVENT_PROGRESS_BATCH: &str = "zdmr://progress_batch";
pub const EVENT_DOWNLOADS_CHANGED: &str = "zdmr://downloads_changed";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerEvent {
  ProgressBatch(Vec<DownloadProgressUpdate>),
  DownloadsChanged,
}

#[derive(Clone)]
pub struct EventHub {
  tx: broadcast::Sender<ServerEvent>,
}

impl EventHub {
  pub fn new() -> Self {
    // Small buffer; consumers should be fast. Local API SSE has its own backpressure semantics.
    let (tx, _) = broadcast::channel(512);
    Self { tx }
  }

  pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
    self.tx.subscribe()
  }

  pub fn emit_progress_batch(&self, updates: Vec<DownloadProgressUpdate>) {
    let _ = self.tx.send(ServerEvent::ProgressBatch(updates));
  }

  pub fn emit_downloads_changed(&self) {
    let _ = self.tx.send(ServerEvent::DownloadsChanged);
  }
}


