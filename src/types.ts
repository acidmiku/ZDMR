export type DownloadStatus = 'QUEUED' | 'DOWNLOADING' | 'PAUSED' | 'COMPLETED' | 'ERROR'

export type ErrorCode =
  | 'DNS_FAIL'
  | 'CONNECT_FAIL'
  | 'TLS_FAIL'
  | 'HTTP_4XX'
  | 'HTTP_5XX'
  | 'TIMEOUT'
  | 'RANGE_UNSUPPORTED'
  | 'DISK_FULL'
  | 'REMOTE_CHANGED'
  | 'PERMISSION_DENIED'
  | 'CANCELLED'
  | 'INVALID_URL'
  | 'UNKNOWN'

export interface DownloadRecord {
  id: string
  created_at: string
  updated_at: string
  started_at: string | null
  completed_at: string | null
  forced_proxy: boolean
  forced_proxy_url: string | null
  original_url: string
  resolved_url: string | null
  dest_dir: string
  final_filename: string | null
  temp_path: string | null
  status: DownloadStatus
  error_code: ErrorCode | null
  error_message: string | null
  content_length: number | null
  etag: string | null
  last_modified: string | null
  bytes_downloaded: number
  supports_ranges: boolean | null
  mirror_used: string | null
  batch_id: string | null
}

export interface DownloadProgressUpdate {
  id: string
  status: DownloadStatus
  bytes_downloaded: number
  content_length: number | null
  speed_bps: number
  eta_seconds: number | null
  error_code: ErrorCode | null
  error_message: string | null
  updated_at: string
}

export interface SettingsSnapshot {
  default_download_dir: string
  bandwidth_limit_bps: number | null
  minimize_to_tray: boolean
  theme: 'dark' | 'mirage' | 'light'
  global_proxy_enabled: boolean
  global_proxy_url: string | null
  local_api_port: number
}

export interface ProxyRule {
  id: number
  pattern: string
  enabled: boolean
  use_proxy: boolean
  proxy_url_override: string | null
}

export interface HeaderRule {
  id: number
  pattern: string
  enabled: boolean
  headers_json: unknown
}

export interface MirrorRule {
  id: number
  pattern: string
  enabled: boolean
  candidates_json: unknown
}

export interface RulesSnapshot {
  proxy_rules: ProxyRule[]
  header_rules: HeaderRule[]
  mirror_rules: MirrorRule[]
}

export interface AddDownloadsRequest {
  urls: string[]
  dest_dir?: string | null
}

export interface NewBatchRequest {
  name: string | null
  dest_dir: string
  raw_url_list: string | null
  urls: string[]
  download_through_proxy?: boolean | null
}

export interface UpdateCheckResult {
  current_version: string
  latest_version: string | null
  update_available: boolean
  installer_url: string | null
}


