# Z-DMR — minimal cross-platform download manager (Rust + Tauri + React)

Z-DMR is a fast, minimal, dark-themed desktop download manager built with:

- **Rust**: download engine + persistence + local API
- **Tauri**: desktop shell, IPC, tray
- **React**: UI (near real-time progress updates)

## Dev

### Prereqs

- Node 20+
- Rust stable

### Run (UI + Tauri)

```bash
npm install
npm run dev:tauri
```

## Where state is stored

- **SQLite DB**: app data dir `zdmr.sqlite3`
- **Logs**: app data dir `logs/zdmr.jsonl` (JSON lines, rotated daily)
- **Downloads**: written to your configured **Default download folder**

Use **Settings → “Open logs folder”** to open the log directory.

## Download behavior (high-level)

- **Smart naming**:
  - Prefer `Content-Disposition` filename
  - Else derive from URL path
  - Else fallback to `download` (+ inferred extension from `Content-Type` when possible)
  - Collisions become `file (1).ext`, `file (2).ext`, ...
  - The chosen final filename is persisted to SQLite
- **Resume**:
  - Partial bytes and segment progress are persisted in SQLite (`download_segments`)
  - On resume, if `ETag` or `Last-Modified` changed, Z-DMR stops with `REMOTE_CHANGED` and requires explicit retry
- **Multipart (segmented)**:
  - Uses HTTP Range when supported (detected via `HEAD`)
  - Falls back to single-stream if ranged requests fail (safe downgrade)
- **Global bandwidth limit**:
  - One limiter shared across all downloads (Settings → Bandwidth limit)

## Proxy rules

Z-DMR uses an **allowlist** model:

- Set a **Global proxy URL** (and enable it)
- Add **Proxy rules** by domain:
  - Exact: `example.com`
  - Wildcard subdomains: `*.example.com`
- Only matching domains use the proxy.

From a download’s context menu you can: **“Add domain to proxy list and retry”**.

## Header rules

Header rules apply deterministically by hostname match (exact / `*.domain`).

`headers_json` supports two shapes:

- Map form:

```json
{
  "headers": {
    "User-Agent": { "value": "Z-DMR", "mode": "override" },
    "Referer": "https://example.com"
  }
}
```

- Flat form:

```json
{
  "Authorization": { "value": "Bearer ...", "mode": "add_if_missing" }
}
```

Modes:

- `override` (default)
- `add_if_missing` (or `add`)

## Mirror rules (auto-mirror retry)

Mirror rules let Z-DMR retry failed downloads against alternate bases:

```json
["https://mirror1.example.com", "https://mirror2.example.com"]
```

Z-DMR preserves the original path + query and tries mirrors in order on **retryable** failures.

## Local HTTP API (for future browser extension)

Z-DMR runs a loopback API bound to `127.0.0.1` on the configured port (Settings → Local API port).

### Auth token

Authentication uses a locally generated token stored in SQLite under the `settings` key `local_api_token`.

Send it as:

- `Authorization: Bearer <token>` **or**
- `X-ZDMR-Token: <token>`

### Endpoints

- **POST `/downloads`**: add one or more URLs
- **POST `/batches`**: add a batch (urls + destination)
- **POST `/downloads/{id}/pause`**
- **POST `/downloads/{id}/resume`**
- **POST `/downloads/{id}/retry`**
- **DELETE `/downloads/{id}`**
- **GET `/events`**: SSE stream of backend events

### Payload shapes

Add downloads:

```json
{
  "urls": ["https://example.com/file.zip"],
  "dest_dir": "C:\\\\Downloads"
}
```

Add batch:

```json
{
  "name": null,
  "dest_dir": "C:\\\\Downloads",
  "raw_url_list": "https://a\\nhttps://b",
  "urls": ["https://a", "https://b"]
}
```

Events are SSE messages where the data is JSON like:

```json
{ "type": "ProgressBatch", "data": [/* DownloadProgressUpdate[] */] }
```

## Packaging notes

- **Windows**: `npm run build:tauri`
- **macOS/Linux**: same command; install platform prerequisites (Xcode tools / GTK + webkit deps as required by Tauri/Wry)


