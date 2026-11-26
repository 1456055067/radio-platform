import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { LiveStream, type LiveState } from '../api/live';
import type { AudioState, LiveMessage, ScannerEvent, ScannerHealth, TelemetrySnapshot, TransmissionStart } from '../api/types';
import { useAuth } from '../auth/AuthContext';

export interface ActiveTransmission extends TransmissionStart {
  site_id: string | null;
}

export interface LiveEventEntry {
  at: number;
  scanner_id: string;
  site_id: string;
  event: ScannerEvent;
}

export interface LiveValue {
  state: LiveState;
  telemetry: ReadonlyMap<string, TelemetrySnapshot>;
  active: ReadonlyMap<string, ActiveTransmission>;
  health: ReadonlyMap<string, ScannerHealth>;
  audio: ReadonlyMap<string, AudioState>;
  recent: readonly LiveEventEntry[];
  lagged: number;
}

const MAX_RECENT = 200;
const Ctx = createContext<LiveValue | null>(null);

export function useLive(): LiveValue {
  const v = useContext(Ctx);
  if (!v) throw new Error('useLive outside LiveProvider');
  return v;
}

export function LiveValueProvider({ value, children }: { value: LiveValue; children: ReactNode }) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

/** Pure reducer so it can be unit-tested. */
export function applyLive(prev: LiveValue, m: LiveMessage, now = Date.now()): LiveValue {
  if (m.kind === 'lagged') return { ...prev, lagged: prev.lagged + m.skipped };
  if (m.kind === 'audio') {
    const audio = new Map(prev.audio);
    audio.set(m.scanner_id, m.data);
    return { ...prev, audio };
  }
  if (m.kind === 'telemetry') {
    const telemetry = new Map(prev.telemetry);
    telemetry.set(m.scanner_id, m.data);
    return { ...prev, telemetry };
  }
  const ev = m.data;
  const recent = [{ at: now, scanner_id: m.scanner_id, site_id: m.site_id, event: ev }, ...prev.recent].slice(0, MAX_RECENT);
  let { active, health } = prev;
  switch (ev.type) {
    case 'transmission_started': {
      const a = new Map(active);
      // A scanner is one receiver: a new transmission supersedes any open one.
      for (const [id, t] of a) if (t.scanner_id === m.scanner_id) a.delete(id);
      a.set(ev.transmission_id, { ...ev, site_id: ev.site_id ?? m.site_id });
      active = a;
      break;
    }
    case 'transmission_ended': {
      if (active.has(ev.transmission_id)) {
        const a = new Map(active);
        a.delete(ev.transmission_id);
        active = a;
      }
      break;
    }
    case 'health_changed': {
      const h = new Map(health);
      h.set(m.scanner_id, ev.health);
      health = h;
      break;
    }
  }
  return { ...prev, recent, active, health };
}

export const emptyLive: LiveValue = {
  state: 'connecting',
  telemetry: new Map(),
  active: new Map(),
  health: new Map(),
  audio: new Map(),
  recent: [],
  lagged: 0,
};

/** One shared live stream for the whole console. */
export function LiveProvider({ children }: { children: ReactNode }) {
  const { session } = useAuth();
  const [value, setValue] = useState<LiveValue>(emptyLive);
  const queue = useRef<LiveMessage[]>([]);

  useEffect(() => {
    // Batch bursts into one render per animation frame.
    let raf = 0;
    const flush = () => {
      raf = 0;
      const batch = queue.current;
      queue.current = [];
      if (batch.length) setValue((v) => batch.reduce((acc, m) => applyLive(acc, m), v));
    };
    const stream = new LiveStream({
      token: () => session.token(),
      onMessage: (m) => {
        queue.current.push(m);
        if (!raf) raf = requestAnimationFrame(flush);
      },
      onState: (state) => setValue((v) => ({ ...v, state })),
    });
    stream.start();
    return () => {
      stream.stop();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [session]);

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
