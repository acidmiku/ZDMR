import { useEffect, useMemo, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import './App.css'
import nyanCatUrl from './assets/nyan_cat.png'
import type { AddDownloadsRequest, DownloadProgressUpdate, DownloadRecord, NewBatchRequest, RulesSnapshot, SettingsSnapshot, UpdateCheckResult } from './types'

const EVENT_PROGRESS_BATCH = 'zdmr://progress_batch'
const EVENT_DOWNLOADS_CHANGED = 'zdmr://downloads_changed'

function parseUrlsFromText(text: string): string[] {
  const parts = text
    .split(/\s+/)
    .map((s) => s.trim())
    .filter(Boolean)
  const out: string[] = []
  for (const p of parts) {
    try {
      const u = new URL(p)
      if (u.protocol === 'http:' || u.protocol === 'https:') out.push(u.toString())
    } catch {
      // ignore
    }
  }
  return Array.from(new Set(out))
}

function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

function formatSpeed(bps: number): string {
  if (!Number.isFinite(bps) || bps <= 0) return '0 B/s'
  return `${formatBytes(bps)}/s`
}

function formatStatusbarSpeed(bps: number): string {
  if (!Number.isFinite(bps) || bps <= 0) return '0 B/s'
  return formatSpeed(bps)
}

function formatEta(seconds?: number | null): string {
  if (!seconds || !Number.isFinite(seconds) || seconds <= 0) return '—'
  const s = Math.floor(seconds)
  const m = Math.floor(s / 60)
  const h = Math.floor(m / 60)
  const ss = s % 60
  const mm = m % 60
  if (h > 0) return `${h}h ${mm}m`
  if (m > 0) return `${m}m ${ss}s`
  return `${ss}s`
}

function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds))
  const m = Math.floor(s / 60)
  const h = Math.floor(m / 60)
  const ss = s % 60
  const mm = m % 60
  if (h > 0) return `${h}h ${mm}m`
  if (m > 0) return `${m}m ${ss}s`
  return `${ss}s`
}

function tookText(d: DownloadRecord): string {
  if (!d.started_at || !d.completed_at) return 'took —'
  const a = Date.parse(d.started_at)
  const b = Date.parse(d.completed_at)
  if (!Number.isFinite(a) || !Number.isFinite(b) || b <= a) return 'took —'
  return `took ${formatDuration((b - a) / 1000)}`
}

function isActive(status: string) {
  return status === 'DOWNLOADING' || status === 'QUEUED'
}

export default function App() {
  const [downloads, setDownloads] = useState<DownloadRecord[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [progressById, setProgressById] = useState<Record<string, DownloadProgressUpdate>>({})
  const progressRef = useRef(progressById)
  progressRef.current = progressById
  const downloadsRef = useRef(downloads)
  downloadsRef.current = downloads
  const selectedIdRef = useRef(selectedId)
  selectedIdRef.current = selectedId

  const [settingsOpen, setSettingsOpen] = useState(false)
  const [batchOpen, setBatchOpen] = useState(false)
  const modalOpenRef = useRef(false)
  modalOpenRef.current = settingsOpen || batchOpen

  const [settings, setSettings] = useState<SettingsSnapshot | null>(null)
  const [rules, setRules] = useState<RulesSnapshot | null>(null)

  async function refreshDownloads() {
    const list = await invoke<DownloadRecord[]>('cmd_list_downloads')
    setDownloads(list)
    // Avoid stale “DOWNLOADING” progress overriding DB status after completion.
    const active = new Set(list.filter((d) => d.status === 'DOWNLOADING' || d.status === 'QUEUED').map((d) => d.id))
    setProgressById((prev) => {
      const next: Record<string, DownloadProgressUpdate> = {}
      for (const [id, v] of Object.entries(prev)) {
        if (active.has(id)) next[id] = v
      }
      return next
    })
  }

  async function refreshSettingsAndRules() {
    const s = await invoke<SettingsSnapshot>('cmd_get_settings')
    const r = await invoke<RulesSnapshot>('cmd_list_rules')
    setSettings(s)
    setRules(r)
  }

  useEffect(() => {
    refreshDownloads()
    refreshSettingsAndRules()

    const unsubs: Array<() => void> = []

    listen<DownloadProgressUpdate[]>(EVENT_PROGRESS_BATCH, (e) => {
      const batch = e.payload ?? []
      if (batch.length === 0) return
      setProgressById((prev) => {
        const next = { ...prev }
        for (const u of batch) next[u.id] = u
        return next
      })
    }).then((unsub) => unsubs.push(unsub))

    listen(EVENT_DOWNLOADS_CHANGED, () => {
      refreshDownloads()
    }).then((unsub) => unsubs.push(unsub))

    // Ctrl+V / paste-to-add while focused: silently add downloads using defaults.
    const onPaste = (ev: ClipboardEvent) => {
      // Do not hijack paste in modals / input fields.
      if (modalOpenRef.current) return
      const t = ev.target as HTMLElement | null
      if (t) {
        const tag = t.tagName?.toLowerCase()
        if (tag === 'input' || tag === 'textarea' || tag === 'select') return
        if (t.isContentEditable) return
        if (t.closest?.('input,textarea,select,[contenteditable="true"]')) return
      }
      const text = ev.clipboardData?.getData('text') ?? ''
      const urls = parseUrlsFromText(text)
      if (urls.length === 0) return
      ev.preventDefault()
      const req: AddDownloadsRequest = { urls }
      invoke('cmd_add_downloads', { req })
        .then(() => refreshDownloads())
        .catch(() => {})
    }
    window.addEventListener('paste', onPaste)

    const onKeyDown = (ev: KeyboardEvent) => {
      const sid = selectedIdRef.current
      if ((ev.key === 'Delete' || ev.key === 'Backspace') && sid) {
        const d = downloadsRef.current.find((x) => x.id === sid)
        if (!d) return
        const p = progressRef.current[sid]
        const status = p?.status ?? d.status
        if (isActive(status)) {
          const ok = window.confirm('Delete this active download?')
          if (!ok) return
        }
        invoke('cmd_delete_download', { id: sid }).catch(() => {})
      }
    }
    window.addEventListener('keydown', onKeyDown)

    return () => {
      for (const u of unsubs) u()
      window.removeEventListener('paste', onPaste)
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [])

  useEffect(() => {
    // Apply theme to the root element (CSS variables in index.css).
    const theme = settings?.theme ?? 'dark'
    document.documentElement.dataset.theme = theme
  }, [settings?.theme])

  useEffect(() => {
    const skin = settings?.skin ?? 'modern'
    document.documentElement.dataset.skin = skin
  }, [settings?.skin])

  const skin = settings?.skin ?? 'modern'

  const rows = useMemo(() => {
    return downloads.map((d) => {
      const p = progressById[d.id]
      const bytes = p?.bytes_downloaded ?? d.bytes_downloaded
      const total = p?.content_length ?? d.content_length ?? undefined
      const pct = total && total > 0 ? Math.min(1, Math.max(0, bytes / total)) : 0
      return { d, p, bytes, total, pct }
    })
  }, [downloads, progressById])

  const statusSummary = useMemo(() => {
    const counts = { QUEUED: 0, DOWNLOADING: 0, PAUSED: 0, COMPLETED: 0, ERROR: 0 }
    for (const d of downloads) counts[d.status]++
    let totalBps = 0
    for (const d of downloads) {
      const p = progressById[d.id]
      if (!p) continue
      if (p.status === 'DOWNLOADING') totalBps += p.speed_bps
    }
    return { counts, totalBps }
  }, [downloads, progressById])

  return (
    <div className="app">
      <div className="topbar">
        <div className="brand">Z-DMR</div>
        <div className="spacer" />
        <button className="iconBtn" onClick={() => setBatchOpen(true)} title="New Batch Download">
          +
        </button>
        <button
          className="iconBtn"
          onClick={async () => {
            await refreshSettingsAndRules()
            setSettingsOpen(true)
          }}
          title="Settings"
        >
          ⚙
        </button>
      </div>

      <div className="list">
        {rows.length === 0 ? (
          <div className="empty">
            Paste a URL (Ctrl+V) to start downloading. Use “+” for batch downloads.
          </div>
        ) : (
          rows.map(({ d, p, bytes, total, pct }) => {
            const status = p?.status ?? d.status
            const speed = p?.speed_bps ?? 0
            const eta = p?.eta_seconds ?? null
            const err = p?.error_message ?? d.error_message
            const pct100 = Math.max(0, Math.min(100, pct * 100))
            return (
              <div
                key={d.id}
                className={`row ${selectedId === d.id ? 'selected' : ''} ${status === 'ERROR' ? 'error' : ''}`}
                tabIndex={0}
                onClick={() => setSelectedId(d.id)}
                onContextMenu={(ev) => {
                  ev.preventDefault()
                  showContextMenu(ev.clientX, ev.clientY, d, status)
                }}
              >
                <div className="rowTop">
                  <div className="filename">
                    {d.final_filename ?? '(resolving...)'}
                    {d.forced_proxy ? <span className="pill">Proxied</span> : null}
                  </div>
                  <div className="status">{status === 'ERROR' ? err ?? 'Error' : status}</div>
                </div>
                <div className="rowMid">
                  <div className="progressBar">
                    <div className="progressFill" style={{ width: `${pct100}%` }} />
                  </div>
                  {skin === 'nyan' ? (
                    <img
                      className="nyanCat"
                      src={nyanCatUrl}
                      alt=""
                      draggable={false}
                      style={{ left: `calc(${pct100}% - 14px)` }}
                    />
                  ) : null}
                </div>
                <div className="rowBot">
                  <div className="meta">
                    {formatBytes(bytes)}
                    {total ? ` / ${formatBytes(total)}` : ''}
                  </div>
                  {status === 'COMPLETED' ? (
                    <>
                      <div className="meta" />
                      <div className="meta right">{tookText(d)}</div>
                    </>
                  ) : (
                    <>
                      <div className="meta">{formatSpeed(speed)}</div>
                      <div className="meta">ETA {formatEta(eta)}</div>
                    </>
                  )}
                </div>
              </div>
            )
          })
        )}
      </div>

      <div className="statusbar">
        <div className="statusItem">
          Q {statusSummary.counts.QUEUED} · D {statusSummary.counts.DOWNLOADING} · P {statusSummary.counts.PAUSED} · C {statusSummary.counts.COMPLETED} · E {statusSummary.counts.ERROR}
        </div>
        <div className="statusItem">{formatStatusbarSpeed(statusSummary.totalBps)}</div>
        <div className="spacer" />
        <button
          className="btn"
          onClick={async () => {
            await invoke('cmd_clear_completed_downloads')
            await refreshDownloads()
          }}
        >
          🗑️
        </button>
      </div>

      {batchOpen && settings && (
        <BatchModal
          defaultDir={settings.default_download_dir}
          canUseProxy={!!settings.global_proxy_url && settings.global_proxy_url.trim().length > 0}
          onClose={() => setBatchOpen(false)}
          onStart={(urls, destDir, downloadThroughProxy) => {
            const req: NewBatchRequest = { name: null, dest_dir: destDir, raw_url_list: urls.join('\n'), urls, download_through_proxy: downloadThroughProxy }
            invoke<string>('cmd_add_batch', { req })
              .then(() => refreshDownloads())
              .catch((e) => window.alert(String(e)))
            setBatchOpen(false)
          }}
        />
      )}

      {settingsOpen && settings && rules && (
        <SettingsModal
          settings={settings}
          rules={rules}
          onClose={() => setSettingsOpen(false)}
          onSave={async (s, r) => {
            await invoke('cmd_set_settings', { s })
            // rules are saved per-action inside the modal (simple + deterministic)
            setSettings(s)
            setRules(r)
            setSettingsOpen(false)
          }}
        />
      )}

      {skin === 'festive' ? <Snowfall /> : null}
    </div>
  )
}

function showContextMenu(x: number, y: number, d: DownloadRecord, status: string) {
  const menu = document.getElementById('ctx')
  if (!menu) return
  menu.style.left = `${x}px`
  menu.style.top = `${y}px`
  menu.style.display = 'block'
  menu.dataset.id = d.id
  menu.dataset.url = d.resolved_url ?? d.original_url
  menu.dataset.dest = d.dest_dir
  menu.dataset.status = status
}

function hideContextMenu() {
  const menu = document.getElementById('ctx')
  if (!menu) return
  menu.style.display = 'none'
}

function ContextMenu() {
  useEffect(() => {
    const onClick = () => hideContextMenu()
    window.addEventListener('click', onClick)
    return () => window.removeEventListener('click', onClick)
  }, [])

  return (
    <div id="ctx" className="ctx" style={{ display: 'none' }}>
      <button
        onClick={() => {
          const el = document.getElementById('ctx')!
          const dest = el.dataset.dest!
          invoke('cmd_open_download_folder', { dest_dir: dest }).catch(() => {})
          hideContextMenu()
        }}
      >
        Open folder
      </button>
      <button
        onClick={() => {
          const el = document.getElementById('ctx')!
          const id = el.dataset.id!
          const status = el.dataset.status!
          if (status === 'PAUSED') invoke('cmd_resume_download', { id }).catch(() => {})
          else invoke('cmd_pause_download', { id }).catch(() => {})
          hideContextMenu()
        }}
      >
        Pause / Resume
      </button>
      <button
        onClick={() => {
          const el = document.getElementById('ctx')!
          const id = el.dataset.id!
          invoke('cmd_retry_download', { id }).catch(() => {})
          hideContextMenu()
        }}
      >
        Retry
      </button>
      <button
        onClick={() => {
          const el = document.getElementById('ctx')!
          const id = el.dataset.id!
          invoke('cmd_delete_download', { id }).catch(() => {})
          hideContextMenu()
        }}
      >
        Delete
      </button>
      <button
        onClick={() => {
          const el = document.getElementById('ctx')!
          const id = el.dataset.id!
          const url = el.dataset.url!
          invoke('cmd_add_domain_to_proxy_and_retry', { download_id: id, url }).catch(() => {})
          hideContextMenu()
        }}
      >
        Add domain to proxy list and retry
      </button>
    </div>
  )
}

function BatchModal(props: { defaultDir: string; canUseProxy: boolean; onClose: () => void; onStart: (urls: string[], destDir: string, downloadThroughProxy: boolean) => void }) {
  const [dest, setDest] = useState(props.defaultDir)
  const [text, setText] = useState('')
  const [useProxy, setUseProxy] = useState(false)
  return (
    <div className="modalBackdrop" onMouseDown={props.onClose}>
      <div className="modal" onMouseDown={(e) => e.stopPropagation()}>
        <div className="modalTitle">New Batch Download</div>
        <label className="field">
          <div className="label">Destination folder</div>
          <input value={dest} onChange={(e) => setDest(e.target.value)} />
        </label>
        <label className="field">
          <div className="label">URLs (newline-separated)</div>
          <textarea value={text} onChange={(e) => setText(e.target.value)} rows={10} />
        </label>
        <label className="field">
          <div className="rowInline">
            <input
              type="checkbox"
              checked={useProxy}
              disabled={!props.canUseProxy}
              onChange={(e) => setUseProxy(e.target.checked)}
            />
            <span>Download through proxy</span>
          </div>
          {!props.canUseProxy && <div className="hint">Set a proxy address in Settings first.</div>}
        </label>
        <div className="modalActions">
          <button className="btn" onClick={props.onClose}>
            Cancel
          </button>
          <button
            className="btn primary"
            onClick={() => {
              const urls = parseUrlsFromText(text.replace(/\r/g, '\n')).filter((u) => u.includes('://'))
              if (urls.length === 0) return
              props.onStart(urls, dest, useProxy)
            }}
          >
            Start
          </button>
        </div>
      </div>
    </div>
  )
}

function SettingsModal(props: {
  settings: SettingsSnapshot
  rules: RulesSnapshot
  onClose: () => void
  onSave: (s: SettingsSnapshot, r: RulesSnapshot) => void
}) {
  const [s, setS] = useState<SettingsSnapshot>(props.settings)
  const [r, setR] = useState<RulesSnapshot>(props.rules)
  const [updateBusy, setUpdateBusy] = useState(false)

  return (
    <div className="modalBackdrop" onMouseDown={props.onClose}>
      <div className="modal wide" onMouseDown={(e) => e.stopPropagation()}>
        <div className="modalTitle">Settings</div>

        <div className="grid2">
          <label className="field">
            <div className="label">Default download folder</div>
            <input
              value={s.default_download_dir}
              onChange={(e) => setS({ ...s, default_download_dir: e.target.value })}
            />
          </label>

          <label className="field">
            <div className="label">Global bandwidth limit (KB/s, 0 = unlimited)</div>
            <input
              value={Math.floor((s.bandwidth_limit_bps ?? 0) / 1024)}
              onChange={(e) => {
                const kb = parseInt(e.target.value || '0', 10)
                setS({ ...s, bandwidth_limit_bps: kb > 0 ? kb * 1024 : null })
              }}
            />
          </label>

          <label className="field">
            <div className="label">Minimize to tray</div>
            <input
              type="checkbox"
              checked={s.minimize_to_tray}
              onChange={(e) => setS({ ...s, minimize_to_tray: e.target.checked })}
            />
          </label>

          <label className="field">
            <div className="label">Theme</div>
            <select
              value={s.theme}
              onChange={(e) => setS({ ...s, theme: e.target.value as SettingsSnapshot['theme'] })}
            >
              <option value="dark">Dark (default)</option>
              <option value="mirage">Mirage</option>
              <option value="light">Light</option>
            </select>
          </label>

          <label className="field">
            <div className="label">Skin</div>
            <select
              value={s.skin}
              onChange={(e) => setS({ ...s, skin: e.target.value as SettingsSnapshot['skin'] })}
            >
              <option value="modern">Modern</option>
              <option value="aero">Aero (glassy)</option>
              <option value="festive">Festive (Christmas)</option>
              <option value="nyan">Nyan</option>
            </select>
            <div className="hint">Skins change styling only (theme still controls base colors).</div>
          </label>

          <label className="field">
            <div className="label">Global hotkey (toggle show/hide)</div>
            <input
              placeholder="Ctrl+Shift+X"
              value={s.global_hotkey}
              onChange={(e) => setS({ ...s, global_hotkey: e.target.value })}
            />
            <div className="hint">Example: Ctrl+Shift+X. Leave as-is if you’re unsure.</div>
          </label>

          <div className="field">
            <div className="label">Global proxy</div>
            <div className="rowInline">
              <label className="rowInline">
                <input
                  type="checkbox"
                  checked={s.global_proxy_enabled}
                  onChange={(e) => setS({ ...s, global_proxy_enabled: e.target.checked })}
                />
                <span>Enable</span>
              </label>
              <input
                placeholder="http://127.0.0.1:7890"
                value={s.global_proxy_url ?? ''}
                onChange={(e) => setS({ ...s, global_proxy_url: e.target.value })}
              />
            </div>
            <div className="hint">Proxy rules are allowlist-based: only matching domains use the proxy.</div>
          </div>
        </div>

        <div className="sectionTitle">Proxy rules</div>
        <div className="table">
          <div className="thead">
            <div>Pattern</div>
            <div>Enabled</div>
            <div>Use proxy</div>
            <div />
          </div>
          {r.proxy_rules.map((pr) => (
            <div key={pr.id} className="trow">
              <input
                value={pr.pattern}
                onChange={(e) => {
                  const next = { ...r, proxy_rules: r.proxy_rules.map((x) => (x.id === pr.id ? { ...x, pattern: e.target.value } : x)) }
                  setR(next)
                }}
              />
              <input
                type="checkbox"
                checked={pr.enabled}
                onChange={(e) => {
                  const next = { ...r, proxy_rules: r.proxy_rules.map((x) => (x.id === pr.id ? { ...x, enabled: e.target.checked } : x)) }
                  setR(next)
                }}
              />
              <input
                type="checkbox"
                checked={pr.use_proxy}
                onChange={(e) => {
                  const next = { ...r, proxy_rules: r.proxy_rules.map((x) => (x.id === pr.id ? { ...x, use_proxy: e.target.checked } : x)) }
                  setR(next)
                }}
              />
              <button
                className="btn"
                onClick={async () => {
                  if (pr.id < 0) {
                    setR({ ...r, proxy_rules: r.proxy_rules.filter((x) => x.id !== pr.id) })
                  } else {
                    await invoke('cmd_delete_proxy_rule', { id: pr.id })
                    const rr = await invoke<RulesSnapshot>('cmd_list_rules')
                    setR(rr)
                  }
                }}
              >
                Delete
              </button>
              <button
                className="btn primary"
                onClick={async () => {
                  await invoke('cmd_upsert_proxy_rule', {
                    id: pr.id > 0 ? pr.id : null,
                    pattern: pr.pattern,
                    enabled: pr.enabled,
                    use_proxy: pr.use_proxy,
                    proxy_url_override: pr.proxy_url_override ?? null,
                  })
                  const rr = await invoke<RulesSnapshot>('cmd_list_rules')
                  setR(rr)
                }}
              >
                Save
              </button>
            </div>
          ))}
        </div>
        <button
          className="btn"
          onClick={() =>
            setR({
              ...r,
              proxy_rules: [{ id: -Date.now(), pattern: 'example.com', enabled: true, use_proxy: true, proxy_url_override: null }, ...r.proxy_rules],
            })
          }
        >
          Add proxy rule
        </button>

        <div className="sectionTitle">Header rules</div>
        <div className="table">
          <div className="thead" style={{ gridTemplateColumns: '1fr 80px 1fr 80px 80px' }}>
            <div>Pattern</div>
            <div>Enabled</div>
            <div>headers_json</div>
            <div />
            <div />
          </div>
          {r.header_rules.map((hr) => (
            <div key={hr.id} className="trow" style={{ gridTemplateColumns: '1fr 80px 1fr 80px 80px' }}>
              <input
                value={hr.pattern}
                onChange={(e) => setR({ ...r, header_rules: r.header_rules.map((x) => (x.id === hr.id ? { ...x, pattern: e.target.value } : x)) })}
              />
              <input
                type="checkbox"
                checked={hr.enabled}
                onChange={(e) => setR({ ...r, header_rules: r.header_rules.map((x) => (x.id === hr.id ? { ...x, enabled: e.target.checked } : x)) })}
              />
              <textarea
                className="json"
                value={JSON.stringify(hr.headers_json, null, 2)}
                rows={4}
                onChange={(e) => {
                  try {
                    const parsed = JSON.parse(e.target.value)
                    setR({ ...r, header_rules: r.header_rules.map((x) => (x.id === hr.id ? { ...x, headers_json: parsed } : x)) })
                  } catch {
                    // keep previous valid JSON
                  }
                }}
              />
              <button
                className="btn"
                onClick={async () => {
                  if (hr.id < 0) {
                    setR({ ...r, header_rules: r.header_rules.filter((x) => x.id !== hr.id) })
                  } else {
                    await invoke('cmd_delete_header_rule', { id: hr.id })
                    const rr = await invoke<RulesSnapshot>('cmd_list_rules')
                    setR(rr)
                  }
                }}
              >
                Delete
              </button>
              <button
                className="btn primary"
                onClick={async () => {
                  await invoke('cmd_upsert_header_rule', {
                    id: hr.id > 0 ? hr.id : null,
                    pattern: hr.pattern,
                    enabled: hr.enabled,
                    headers_json: hr.headers_json,
                  })
                  const rr = await invoke<RulesSnapshot>('cmd_list_rules')
                  setR(rr)
                }}
              >
                Save
              </button>
            </div>
          ))}
        </div>
        <button
          className="btn"
          onClick={() =>
            setR({
              ...r,
              header_rules: [
                { id: -Date.now(), pattern: 'example.com', enabled: true, headers_json: { headers: { 'User-Agent': { value: 'Z-DMR', mode: 'override' } } } },
                ...r.header_rules,
              ],
            })
          }
        >
          Add header rule
        </button>

        <div className="sectionTitle">Mirror rules</div>
        <div className="table">
          <div className="thead" style={{ gridTemplateColumns: '1fr 80px 1fr 80px 80px' }}>
            <div>Pattern</div>
            <div>Enabled</div>
            <div>candidates_json</div>
            <div />
            <div />
          </div>
          {r.mirror_rules.map((mr) => (
            <div key={mr.id} className="trow" style={{ gridTemplateColumns: '1fr 80px 1fr 80px 80px' }}>
              <input
                value={mr.pattern}
                onChange={(e) => setR({ ...r, mirror_rules: r.mirror_rules.map((x) => (x.id === mr.id ? { ...x, pattern: e.target.value } : x)) })}
              />
              <input
                type="checkbox"
                checked={mr.enabled}
                onChange={(e) => setR({ ...r, mirror_rules: r.mirror_rules.map((x) => (x.id === mr.id ? { ...x, enabled: e.target.checked } : x)) })}
              />
              <textarea
                className="json"
                value={JSON.stringify(mr.candidates_json, null, 2)}
                rows={4}
                onChange={(e) => {
                  try {
                    const parsed = JSON.parse(e.target.value)
                    setR({ ...r, mirror_rules: r.mirror_rules.map((x) => (x.id === mr.id ? { ...x, candidates_json: parsed } : x)) })
                  } catch {
                    // keep previous valid JSON
                  }
                }}
              />
              <button
                className="btn"
                onClick={async () => {
                  if (mr.id < 0) {
                    setR({ ...r, mirror_rules: r.mirror_rules.filter((x) => x.id !== mr.id) })
                  } else {
                    await invoke('cmd_delete_mirror_rule', { id: mr.id })
                    const rr = await invoke<RulesSnapshot>('cmd_list_rules')
                    setR(rr)
                  }
                }}
              >
                Delete
              </button>
              <button
                className="btn primary"
                onClick={async () => {
                  await invoke('cmd_upsert_mirror_rule', {
                    id: mr.id > 0 ? mr.id : null,
                    pattern: mr.pattern,
                    enabled: mr.enabled,
                    candidates_json: mr.candidates_json,
                  })
                  const rr = await invoke<RulesSnapshot>('cmd_list_rules')
                  setR(rr)
                }}
              >
                Save
              </button>
            </div>
          ))}
        </div>
        <button
          className="btn"
          onClick={() =>
            setR({
              ...r,
              mirror_rules: [{ id: -Date.now(), pattern: 'example.com', enabled: true, candidates_json: ['https://mirror.example.com'] }, ...r.mirror_rules],
            })
          }
        >
          Add mirror rule
        </button>

        <div className="modalActions">
          <button
            className="btn"
            onClick={() => {
              invoke('cmd_open_logs_folder').catch(() => {})
            }}
          >
            Open logs folder
          </button>
          <button
            className="btn"
            disabled={updateBusy}
            onClick={async () => {
              setUpdateBusy(true)
              try {
                const res = await invoke<UpdateCheckResult>('cmd_check_for_updates')
                if (!res.update_available || !res.installer_url) {
                  window.alert(`No updates available. Current: ${res.current_version}`)
                  return
                }
                const ok = window.confirm(
                  `New version available: ${res.latest_version}. Install now?\n\n(Current: ${res.current_version})`,
                )
                if (!ok) return
                await invoke('cmd_install_update', { installerUrl: res.installer_url })
              } catch (e) {
                window.alert(`Update check failed: ${String(e)}`)
              } finally {
                setUpdateBusy(false)
              }
            }}
          >
            Check for updates
          </button>
          <div className="spacer" />
          <button className="btn" onClick={props.onClose}>
            Close
          </button>
          <button className="btn primary" onClick={() => props.onSave(s, r)}>
            Save settings
          </button>
        </div>
      </div>
    </div>
  )
}

function Snowfall() {
  const flakes = Array.from({ length: 18 }, (_, i) => {
    const left = (i * 61) % 100
    const duration = 7 + (i % 7) * 1.1
    const delay = -((i % 9) * 0.8)
    const size = 12 + (i % 6) * 3
    const opacity = 0.25 + ((i % 5) * 0.12)
    return { i, left, duration, delay, size, opacity }
  })

  return (
    <div className="snowOverlay" aria-hidden="true">
      {flakes.map((f) => (
        <span
          key={f.i}
          className="snowflake"
          style={{
            left: `${f.left}%`,
            animationDuration: `${f.duration}s`,
            animationDelay: `${f.delay}s`,
            fontSize: `${f.size}px`,
            opacity: f.opacity,
          }}
        >
          ❄
        </span>
      ))}
    </div>
  )
}

// Mount the context menu once.
// eslint-disable-next-line react-refresh/only-export-components
export function AppWithContext() {
  return (
    <>
      <App />
      <ContextMenu />
    </>
  )
}
