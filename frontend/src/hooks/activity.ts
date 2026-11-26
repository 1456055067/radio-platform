import type { Scanner, TelemetrySnapshot } from '../api/types';
import type { ActiveTransmission } from './LiveContext';

/**
 * Agents send telemetry when it changes and at least every 30 s (the
 * collector heartbeat), so silence shorter than this means "unchanged".
 */
export const STALE_MS = 45_000;

export interface Activity {
  scanner_id: string;
  site_id: string;
  started_at: string | null;
  frequency_hz: number | null;
  tgid: string | null;
  unit_id: string | null;
  system: string | null;
  department: string | null;
  channel: string | null;
  signal_bars: number | null;
}

export function isFresh(t: TelemetrySnapshot | undefined, now: number): t is TelemetrySnapshot {
  return !!t && now - new Date(t.observed_at).getTime() < STALE_MS;
}

/**
 * What each scanner is receiving now: an open transmission from the event
 * stream, or else fresh telemetry showing the squelch open (`Mute` off).
 * Both are heuristics from scanner state; see docs/protocol-compliance.md.
 */
export function currentActivity(
  scanners: readonly Scanner[],
  active: ReadonlyMap<string, ActiveTransmission>,
  telemetry: ReadonlyMap<string, TelemetrySnapshot>,
  now: number,
): Activity[] {
  const byScanner = new Map<string, ActiveTransmission>();
  for (const t of active.values()) byScanner.set(t.scanner_id, t);
  const out: Activity[] = [];
  for (const s of scanners) {
    const a = byScanner.get(s.id);
    const t = telemetry.get(s.id);
    if (a) {
      out.push({
        scanner_id: s.id,
        site_id: s.site_id,
        started_at: a.started_at,
        frequency_hz: a.frequency_hz,
        tgid: a.tgid,
        unit_id: a.unit_id,
        system: a.system,
        department: a.department,
        channel: a.channel,
        signal_bars: isFresh(t, now) ? t.signal_bars : a.signal_bars,
      });
    } else if (isFresh(t, now) && t.muted === false) {
      out.push({
        scanner_id: s.id,
        site_id: s.site_id,
        started_at: null,
        frequency_hz: t.frequency_hz,
        tgid: t.tgid,
        unit_id: t.unit_id,
        system: t.system,
        department: t.department,
        channel: t.channel,
        signal_bars: t.signal_bars,
      });
    }
  }
  return out;
}
