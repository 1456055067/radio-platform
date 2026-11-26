import type { ReactNode } from 'react';
import { MemoryRouter, Route, Routes } from 'react-router';
import { ApiClient } from '../api/client';
import type { Agent, Me, Permission, Scanner, Site, TelemetrySnapshot, Transmission } from '../api/types';
import { AuthValueProvider, type AuthValue } from '../auth/AuthContext';
import type { Session } from '../auth/session';
import { DirectoryValueProvider, makeDirectory } from '../hooks/Directory';
import { emptyLive, LiveValueProvider, type LiveValue } from '../hooks/LiveContext';
import { NotificationsProvider } from '../hooks/Notifications';

export const SITE: Site = {
  id: 'site-1',
  tenant_id: 't',
  name: 'North Ridge',
  description: 'Hilltop',
  latitude: 44.9,
  longitude: -93.2,
  allowed_networks: ['192.168.50.0/24'],
  created_at: '2026-11-01T00:00:00Z',
};

export const AGENT: Agent = { id: 'agent-a', tenant_id: 't', site_id: 'site-1', description: null, last_seen_at: null, connected: true };

export function scanner(over: Partial<Scanner> = {}): Scanner {
  return {
    id: 'sc-1',
    tenant_id: 't',
    site_id: 'site-1',
    agent_id: 'agent-a',
    name: 'NR Primary',
    host: '192.168.50.10',
    port: 50536,
    enabled: true,
    model: 'SDS200',
    firmware: 'Version 1.26.01',
    health: { state: 'healthy' },
    audio_enabled: true,
    recording_enabled: false,
    transcription_enabled: false,
    rtsp_port: 554,
    created_at: '2026-11-01T00:00:00Z',
    updated_at: '2026-11-01T00:00:00Z',
    agent_connected: true,
    control: null,
    ...over,
  };
}

export function transmission(over: Partial<Transmission> = {}): Transmission {
  return {
    id: 'tx-1',
    tenant_id: 't-1',
    site_id: 'site-1',
    scanner_id: 'sc-1',
    started_at: '2026-11-25T10:00:00Z',
    ended_at: '2026-11-25T10:00:04Z',
    duration_ms: 4000,
    frequency_hz: 851_012_500,
    tgid: null,
    unit_id: null,
    system_name: 'Example County',
    department_name: null,
    channel_name: 'FD Main',
    modulation: 'FM',
    signal_bars: 3,
    rssi: null,
    audio_object: null,
    transcript: null,
    transcription_state: null,
    transcription_error: null,
    transcript_language: null,
    transcript_model: null,
    transcribed_at: null,
    hold: false,
    ...over,
  };
}

export function snapshot(over: Partial<TelemetrySnapshot> = {}): TelemetrySnapshot {
  return {
    observed_at: new Date().toISOString(),
    mode: 'Scan Mode',
    screen: null,
    system: 'Example County P25',
    system_index: 3,
    system_type: 'P25',
    department: 'Fire Dispatch',
    department_index: 7,
    site: null,
    channel: 'FD Main',
    channel_index: 11,
    channel_kind: null,
    frequency_hz: 851_012_500,
    tgid: '1201',
    unit_id: '123456',
    modulation: 'P25',
    service_type: null,
    signal_bars: 3,
    rssi: -74,
    muted: false,
    volume: 10,
    squelch: 3,
    holding: false,
    recording: null,
    display: { info_area1: null, info_area2: null, overwrite: null, popup: null },
    ...over,
  };
}

export const ALL_PERMS: Permission[] = ['scanner_view', 'scanner_control', 'scanner_admin', 'scanner_manage', 'site_manage', 'event_view', 'audit_view', 'export'];

export type FetchMock = ReturnType<typeof vi.fn<typeof fetch>>;

/** A fetch double that answers by `METHOD path` and records calls. */
export function fakeFetch(routes: Record<string, unknown | ((body: unknown) => unknown)>): FetchMock {
  return vi.fn<typeof fetch>(async (input, init) => {
    const url = new URL(String(input), 'http://ui.test');
    const key = `${init?.method ?? 'GET'} ${url.pathname}`;
    const h = routes[key];
    if (h === undefined) return new Response(JSON.stringify({ status: 404, title: 'Not Found' }), { status: 404 });
    let body: unknown;
    if (init?.body) {
      try {
        body = JSON.parse(String(init.body));
      } catch {
        body = String(init.body);
      }
    }
    const v = typeof h === 'function' ? (h as (b: unknown) => unknown)(body) : h;
    if (v instanceof Response) return v;
    return v === null ? new Response(null, { status: 204 }) : new Response(JSON.stringify(v), { status: 200 });
  });
}

export function authValue(perms: Permission[], fetchImpl: typeof fetch, subject = 'alice'): AuthValue {
  const me: Me = { subject, name: null, tenant_id: 't', sites: null, permissions: perms, expires_at: '2099-01-01T00:00:00Z' };
  const session: Session = { mode: 'dev', token: async () => 'tok', signOut: async () => {} };
  const set = new Set(perms);
  return { session, api: new ApiClient(session.token, '/api/v1', fetchImpl), me, can: (p) => set.has(p) };
}

export function renderWith(
  ui: ReactNode,
  {
    auth,
    live = emptyLive,
    scanners = [scanner()],
    path = '/',
    route = '/',
  }: { auth: AuthValue; live?: LiveValue; scanners?: Scanner[]; path?: string; route?: string },
) {
  return (
    <MemoryRouter initialEntries={[path]}>
      <AuthValueProvider value={auth}>
        <NotificationsProvider>
          <LiveValueProvider value={live}>
            <DirectoryValueProvider value={makeDirectory([SITE], scanners, [AGENT])}>
              <Routes>
                <Route path={route} element={ui} />
              </Routes>
            </DirectoryValueProvider>
          </LiveValueProvider>
        </NotificationsProvider>
      </AuthValueProvider>
    </MemoryRouter>
  );
}
