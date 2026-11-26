import type { LiveMessage } from '../api/types';
import { snapshot } from '../test/fixtures';
import { applyLive, emptyLive } from './LiveContext';

const started = (id: string, scanner = 's1'): LiveMessage => ({
  kind: 'event',
  site_id: 'site',
  scanner_id: scanner,
  data: {
    type: 'transmission_started',
    transmission_id: id,
    scanner_id: scanner,
    site_id: null,
    started_at: '2026-11-25T00:00:00Z',
    frequency_hz: 1,
    tgid: null,
    unit_id: null,
    system: null,
    department: null,
    channel: null,
    modulation: null,
    signal_bars: null,
    rssi: null,
  },
});

describe('applyLive', () => {
  it('tracks open transmissions, one per receiver', () => {
    let v = applyLive(emptyLive, started('t1'));
    expect([...v.active.keys()]).toEqual(['t1']);
    expect(v.active.get('t1')?.site_id).toBe('site');
    v = applyLive(v, started('t2'));
    expect([...v.active.keys()]).toEqual(['t2']);
    v = applyLive(v, started('t3', 's2'));
    v = applyLive(v, { kind: 'event', site_id: 'site', scanner_id: 's1', data: { type: 'transmission_ended', transmission_id: 't2', ended_at: '2026-11-25T00:00:01Z', duration_ms: 1000 } });
    expect([...v.active.keys()]).toEqual(['t3']);
    expect(v.recent).toHaveLength(4);
  });

  it('keeps the latest telemetry and health, and counts lag', () => {
    let v = applyLive(emptyLive, { kind: 'telemetry', site_id: 'site', scanner_id: 's1', data: snapshot({ volume: 4 }) });
    v = applyLive(v, { kind: 'telemetry', site_id: 'site', scanner_id: 's1', data: snapshot({ volume: 9 }) });
    expect(v.telemetry.get('s1')?.volume).toBe(9);
    v = applyLive(v, { kind: 'event', site_id: 'site', scanner_id: 's1', data: { type: 'health_changed', health: { state: 'unreachable', consecutive_failures: 3 } } });
    expect(v.health.get('s1')?.state).toBe('unreachable');
    v = applyLive(v, { kind: 'lagged', skipped: 5 });
    expect(v.lagged).toBe(5);
  });

  it('tracks audio pipeline state', () => {
    const v = applyLive(emptyLive, { kind: 'audio', site_id: 'site', scanner_id: 's1', data: { state: 'streaming', received: 10, lost: 1 } });
    expect(v.audio.get('s1')).toEqual({ state: 'streaming', received: 10, lost: 1 });
  });

  it('bounds the recent list', () => {
    let v = emptyLive;
    for (let i = 0; i < 250; i++) v = applyLive(v, started(`t${i}`));
    expect(v.recent).toHaveLength(200);
  });
});
