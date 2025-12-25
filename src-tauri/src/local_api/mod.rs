use crate::{
  engine::{DownloadEngineHandle, EngineCommand},
  events::EventHub,
  model::{AddDownloadsRequest, NewBatchRequest},
  persistence::{Db, SettingsStore},
};
use axum::{
  extract::{Path, State},
  http::{HeaderMap, StatusCode},
  response::{sse::Event, IntoResponse, Response, Sse},
  routing::{delete, get, post},
  Json, Router,
};
use futures_util::{Stream, StreamExt};
use std::{convert::Infallible, net::SocketAddr, time::Duration};
use tokio_stream::wrappers::BroadcastStream;

#[derive(Clone)]
struct ApiState {
  db: Db,
  settings: SettingsStore,
  engine: DownloadEngineHandle,
  events: EventHub,
  token: String,
}

pub fn spawn_local_api(
  _app: tauri::AppHandle,
  db: Db,
  settings: SettingsStore,
  engine: DownloadEngineHandle,
  events: EventHub,
) -> anyhow::Result<()> {
  let token = db.get_local_api_token()?;
  let port = settings.get_snapshot()?.local_api_port as u16;

  let state = ApiState {
    db,
    settings,
    engine,
    events,
    token,
  };

  let app = Router::new()
    .route("/downloads", post(post_download))
    .route("/batches", post(post_batch))
    .route("/downloads/:id/pause", post(post_pause))
    .route("/downloads/:id/resume", post(post_resume))
    .route("/downloads/:id/retry", post(post_retry))
    .route("/downloads/:id", delete(delete_download))
    .route("/events", get(get_events))
    .with_state(state);

  let addr = SocketAddr::from(([127, 0, 0, 1], port));
  tracing::info!(%addr, "starting local api");

  tauri::async_runtime::spawn(async move {
    if let Err(e) = axum::serve(tokio::net::TcpListener::bind(addr).await.unwrap(), app).await {
      tracing::error!(error = %e, "local api server stopped");
    }
  });

  Ok(())
}

fn check_auth(headers: &HeaderMap, token: &str) -> bool {
  if let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
    if let Some(rest) = v.strip_prefix("Bearer ") {
      return rest.trim() == token;
    }
  }
  if let Some(v) = headers.get("x-zdmr-token").and_then(|v| v.to_str().ok()) {
    return v.trim() == token;
  }
  false
}

async fn post_download(
  State(st): State<ApiState>,
  headers: HeaderMap,
  Json(req): Json<AddDownloadsRequest>,
) -> impl IntoResponse {
  if !check_auth(&headers, &st.token) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let dest_dir = match req.dest_dir {
    Some(d) => d,
    None => st.settings.get_snapshot().ok().map(|s| s.default_download_dir).unwrap_or_default(),
  };
  let _ = st
    .engine
    .send(EngineCommand::AddDownloads {
      urls: req.urls,
      dest_dir,
      batch_id: None,
    })
    .await;
  StatusCode::ACCEPTED.into_response()
}

async fn post_batch(
  State(st): State<ApiState>,
  headers: HeaderMap,
  Json(req): Json<NewBatchRequest>,
) -> impl IntoResponse {
  if !check_auth(&headers, &st.token) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let batch_id = match st
    .db
    .insert_batch(&req.dest_dir, req.name.as_deref(), req.raw_url_list.as_deref())
  {
    Ok(id) => id,
    Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
  };
  let _ = st
    .engine
    .send(EngineCommand::AddDownloads {
      urls: req.urls,
      dest_dir: req.dest_dir,
      batch_id: Some(batch_id),
    })
    .await;
  StatusCode::ACCEPTED.into_response()
}

async fn post_pause(State(st): State<ApiState>, headers: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
  if !check_auth(&headers, &st.token) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let _ = st.engine.send(EngineCommand::Pause { id }).await;
  StatusCode::ACCEPTED.into_response()
}

async fn post_resume(State(st): State<ApiState>, headers: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
  if !check_auth(&headers, &st.token) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let _ = st.engine.send(EngineCommand::Resume { id }).await;
  StatusCode::ACCEPTED.into_response()
}

async fn post_retry(State(st): State<ApiState>, headers: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
  if !check_auth(&headers, &st.token) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let _ = st.engine.send(EngineCommand::Retry { id }).await;
  StatusCode::ACCEPTED.into_response()
}

async fn delete_download(State(st): State<ApiState>, headers: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
  if !check_auth(&headers, &st.token) {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let _ = st.engine.send(EngineCommand::Delete { id }).await;
  StatusCode::NO_CONTENT.into_response()
}

async fn get_events(State(st): State<ApiState>, headers: HeaderMap) -> Response {
  if !check_auth(&headers, &st.token) {
    return StatusCode::UNAUTHORIZED.into_response();
  }

  // Each client gets a broadcast receiver; events are serialized as JSON.
  let rx = st.events.subscribe();
  let stream = BroadcastStream::new(rx).filter_map(|msg| async move {
    match msg {
      Ok(evt) => {
        let json =
          serde_json::to_string(&evt).unwrap_or_else(|_| "{\"type\":\"error\"}".to_string());
        Some(Ok::<Event, Infallible>(Event::default().data(json)))
      }
      Err(_) => None,
    }
  });

  sse(stream).into_response()
}

fn sse<S>(stream: S) -> Sse<S>
where
  S: Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
  Sse::new(stream).keep_alive(
    axum::response::sse::KeepAlive::new()
      .interval(Duration::from_secs(15))
      .text("keep-alive"),
  )
}


