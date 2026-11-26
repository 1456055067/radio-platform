import type { TelemetrySnapshot } from '../api/types';
import { formatFrequency, relativeTime } from '../lib/format';
import { SignalBars } from './SignalBars';

function Line({ label, value, big }: { label: string; value: string | null | undefined; big?: boolean }) {
  return (
    <div className={big ? 'rp-lcd-line big' : 'rp-lcd-line'}>
      <span className="rp-lcd-label">{label}</span>
      <span className="rp-lcd-value">{value || '—'}</span>
    </div>
  );
}

/**
 * A virtual front-panel display rendered from telemetry. It shows only what
 * the scanner reported; missing fields are dashes, never guesses.
 */
export function ScannerDisplay({ snapshot, stale }: { snapshot: TelemetrySnapshot | null; stale: boolean }) {
  if (!snapshot) {
    return (
      <div className="rp-lcd off" aria-label="Scanner display">
        <div className="rp-lcd-line big">
          <span className="rp-lcd-value">No telemetry yet</span>
        </div>
      </div>
    );
  }
  const receiving = snapshot.muted === false;
  const flags = [
    snapshot.holding ? 'HOLD' : null,
    receiving ? 'RX' : null,
    snapshot.recording ? 'REC' : null,
    snapshot.modulation,
  ].filter(Boolean);
  return (
    <div className={`rp-lcd${receiving ? ' rx' : ''}${stale ? ' stale' : ''}`} aria-label="Scanner display" aria-live="polite">
      <div className="rp-lcd-top">
        <span>{snapshot.mode ?? '—'}</span>
        <span className="rp-lcd-flags">
          {flags.map((f) => (
            <span key={f} className="rp-lcd-flag">
              {f}
            </span>
          ))}
          <SignalBars bars={snapshot.signal_bars} rssi={snapshot.rssi} />
        </span>
      </div>
      <Line label="SYS" value={snapshot.system} />
      <Line label="DEPT" value={snapshot.department} />
      <Line label="CHAN" value={snapshot.channel} big />
      <div className="rp-lcd-row">
        <Line label="FREQ" value={snapshot.frequency_hz ? formatFrequency(snapshot.frequency_hz) : null} />
        <Line label="TGID" value={snapshot.tgid} />
        <Line label="UID" value={snapshot.unit_id} />
      </div>
      {(snapshot.display.info_area1 || snapshot.display.info_area2) && (
        <div className="rp-lcd-info">
          {[snapshot.display.info_area1, snapshot.display.info_area2].filter(Boolean).join(' · ')}
        </div>
      )}
      {snapshot.display.popup && <div className="rp-lcd-popup">{snapshot.display.popup}</div>}
      <div className="rp-lcd-foot">
        <span>
          VOL {snapshot.volume ?? '—'} · SQL {snapshot.squelch ?? '—'}
        </span>
        <span>{stale ? 'stale · ' : ''}last report {relativeTime(snapshot.observed_at)}</span>
      </div>
    </div>
  );
}
