## v0.1.3 (2025-12-25)

### Fixed
- Windows in-app updater: avoid `os error 32` (“file is being used by another process”) by closing the downloaded installer before launch, using a unique temp filename, and retrying on transient sharing violations.

## v0.1.2 (2025-12-25)

### Fixed
- Correct filename parsing when servers send `Content-Disposition` with both `filename*=` and `filename=` (prevents names like `foo.gguf; filename=foo.gguf`).

## v0.1.1 (2025-12-25)

### Added
- Batch option **“Download through proxy”** (uses the proxy URL from Settings even if global proxy is disabled).
- **Proxied** label in the downloads list for forced-proxy downloads.
- Bottom **status bar** with per-status counts, total download speed, and **Clear completed**.
- **Check for updates** in Settings: detects newer GitHub release, downloads Windows installer, and launches it.

### Fixed
- Filename “resolving…” now updates immediately once headers are known.
- Completed items show **“took …”** instead of speed/ETA.
- UI polish: aligned “took …”, fixed paste hijack in batch modal, fixed checkbox spacing, fixed status bar overflow.


