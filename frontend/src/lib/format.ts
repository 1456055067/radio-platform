import type { ScannerHealth, Transmission } from '../api/types';

/** 851012500 → "851.0125 MHz" (the scanner reports 100 Hz resolution). */
export function formatFrequency(hz: number | null | undefined): string {
  if (hz === null || hz === undefined || !Number.isFinite(hz)) return '—';
  return `${(hz / 1e6).toFixed(4)} MHz`;
}

/** Parses "851.0125" (MHz) into Hz; null if not a plausible frequency. */
export function parseMhz(text: string): number | null {
  const t = text.trim();
  if (!/^\d{1,4}(\.\d{1,6})?$/.test(t)) return null;
  const hz = Math.round(Number(t) * 1e6);
  return hz >= 25_000_000 && hz <= 1_300_000_000 ? hz : null;
}

export function formatDuration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return '—';
  if (ms < 1000) return `${ms} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)} s`;
  const m = Math.floor(s / 60);
  if (m >= 60) return `${Math.floor(m / 60)} h ${m % 60} min`;
  return `${m} min ${Math.round(s - m * 60)} s`;
}

export function formatTime(iso: string | null | undefined): string {
  if (!iso) return '—';
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '—' : d.toLocaleString();
}

export function relativeTime(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return 'never';
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return '—';
  const s = Math.round((now - t) / 1000);
  if (s < 5) return 'just now';
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} d ago`;
}

export function dash<T>(v: T | null | undefined): T | string {
  return v === null || v === undefined || v === '' ? '—' : v;
}

export type HealthTone = 'success' | 'warning' | 'error' | 'stopped' | 'pending';

/** Combines agent connectivity, enablement and SDK health into one status. */
export function scannerStatus(s: {
  enabled: boolean;
  agent_connected: boolean;
  health: ScannerHealth | null;
}): { tone: HealthTone; label: string; online: boolean } {
  if (!s.enabled) return { tone: 'stopped', label: 'Disabled', online: false };
  if (!s.agent_connected) return { tone: 'error', label: 'Agent offline', online: false };
  switch (s.health?.state) {
    case 'healthy':
      return { tone: 'success', label: 'Online', online: true };
    case 'degraded':
      return { tone: 'warning', label: `Degraded (${s.health.consecutive_failures} failures)`, online: true };
    case 'unreachable':
      return { tone: 'error', label: 'Scanner unreachable', online: false };
    default:
      return { tone: 'pending', label: 'Connecting', online: false };
  }
}

// ---------------- export ----------------

const CSV_COLUMNS: (keyof Transmission)[] = [
  'id',
  'started_at',
  'ended_at',
  'duration_ms',
  'site_id',
  'scanner_id',
  'frequency_hz',
  'modulation',
  'system_name',
  'department_name',
  'channel_name',
  'tgid',
  'unit_id',
  'signal_bars',
  'rssi',
  'transcription_state',
  'transcript',
];

/** RFC 4180 field, neutralising spreadsheet formula injection. */
export function csvField(v: unknown): string {
  if (v === null || v === undefined) return '';
  let s = String(v);
  if (/^[=+\-@\t\r]/.test(s) && !/^-?\d+(\.\d+)?$/.test(s)) s = `'${s}`;
  return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function transmissionsCsv(rows: Transmission[]): string {
  const lines = [CSV_COLUMNS.join(',')];
  for (const r of rows) lines.push(CSV_COLUMNS.map((c) => csvField(r[c])).join(','));
  return lines.join('\r\n') + '\r\n';
}

export function download(filename: string, mime: string, body: string): void {
  const url = URL.createObjectURL(new Blob([body], { type: mime }));
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.rel = 'noopener';
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Buckets transmissions into `buckets` equal time slices for the timeline. */
export function timeline(rows: Transmission[], from: number, to: number, buckets: number): { x: Date; y: number }[] {
  const n = Math.max(1, buckets);
  const span = Math.max(1, to - from);
  const out = Array.from({ length: n }, (_, i) => ({ x: new Date(from + (span * i) / n), y: 0 }));
  for (const r of rows) {
    const t = new Date(r.started_at).getTime();
    if (t < from || t > to) continue;
    const i = Math.min(n - 1, Math.floor(((t - from) / span) * n));
    const b = out[i];
    if (b) b.y += 1;
  }
  return out;
}
