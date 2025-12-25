use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// A simple global token-bucket style bandwidth limiter shared across all downloads.
/// If `limit_bps <= 0`, limiting is disabled.
#[derive(Clone)]
pub struct BandwidthLimiter {
  limit_bps: Arc<AtomicI64>,
  credits: Arc<AtomicI64>,
  notify: Arc<Notify>,
}

impl BandwidthLimiter {
  pub fn new(limit_bps: i64) -> Self {
    let limiter = Self {
      limit_bps: Arc::new(AtomicI64::new(limit_bps)),
      credits: Arc::new(AtomicI64::new(0)),
      notify: Arc::new(Notify::new()),
    };
    limiter.spawn_refill_task();
    limiter
  }

  pub fn set_limit_bps(&self, limit_bps: i64) {
    self.limit_bps.store(limit_bps, Ordering::Relaxed);
    self.notify.notify_waiters();
  }

  pub fn limit_bps(&self) -> i64 {
    self.limit_bps.load(Ordering::Relaxed)
  }

  pub async fn acquire(&self, bytes: usize) {
    let need = bytes as i64;
    if need <= 0 {
      return;
    }

    loop {
      let limit = self.limit_bps();
      if limit <= 0 {
        return;
      }

      let cur = self.credits.load(Ordering::Relaxed);
      if cur >= need {
        if self
          .credits
          .compare_exchange(cur, cur - need, Ordering::Relaxed, Ordering::Relaxed)
          .is_ok()
        {
          return;
        }
        continue;
      }

      self.notify.notified().await;
    }
  }

  fn spawn_refill_task(&self) {
    let credits = self.credits.clone();
    let limit_bps = self.limit_bps.clone();
    let notify = self.notify.clone();

    // Refill at a fine cadence so small reads don't stall too much.
    const TICK: Duration = Duration::from_millis(20);

    tauri::async_runtime::spawn(async move {
      let mut interval = tokio::time::interval(TICK);
      interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
      let mut last = Instant::now();

      loop {
        interval.tick().await;
        let limit = limit_bps.load(Ordering::Relaxed);
        if limit <= 0 {
          credits.store(0, Ordering::Relaxed);
          notify.notify_waiters();
          last = Instant::now();
          continue;
        }

        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last);
        last = now;

        let add = ((limit as f64) * (elapsed.as_secs_f64())) as i64;
        if add <= 0 {
          continue;
        }

        // Allow up to ~1s burst so the limiter feels smoother.
        let max = limit;
        loop {
          let cur = credits.load(Ordering::Relaxed);
          let next = (cur + add).min(max);
          if credits
            .compare_exchange(cur, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
          {
            break;
          }
        }

        notify.notify_waiters();
      }
    });
  }
}


