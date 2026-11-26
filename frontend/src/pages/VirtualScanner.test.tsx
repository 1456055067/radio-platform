import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { emptyLive } from '../hooks/LiveContext';
import { authValue, fakeFetch, renderWith, scanner, snapshot } from '../test/fixtures';
import { VirtualScanner } from './VirtualScanner';

const ROUTE = { path: '/scanners/sc-1', route: '/scanners/:id' };

function routes(over: Record<string, unknown> = {}) {
  return {
    'GET /api/v1/scanners/sc-1': scanner(),
    'GET /api/v1/scanners/sc-1/telemetry': { scanner_id: 'sc-1', snapshot: snapshot(), agent_connected: true },
    'GET /api/v1/transmissions': { items: [], next_cursor: null },
    'GET /api/v1/scanners/sc-1/audio': { audio_enabled: true, recording_enabled: false, live_available: true, state: { state: 'idle' }, listeners: 0, receiving: false },
    ...over,
  };
}

describe('VirtualScanner', () => {
  it('shows live telemetry and keeps viewers read-only', async () => {
    const f = fakeFetch(routes());
    render(renderWith(<VirtualScanner />, { auth: authValue(['scanner_view', 'event_view'], f), ...ROUTE }));
    expect(await screen.findByRole('heading', { name: /NR Primary/ })).toBeInTheDocument();
    expect(screen.getByLabelText('Scanner display')).toHaveTextContent('FD Main');
    expect(screen.getAllByText('851.0125 MHz').length).toBeGreaterThan(0);
    expect(screen.getByText(/Your role is read-only/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Take control' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Menu key' })).toBeDisabled();
    expect(screen.getByText(/Your role cannot listen/)).toBeInTheDocument();
  });

  it('takes control, then sends commands with the lease', async () => {
    const user = userEvent.setup();
    const lease = { lease_id: 'L-1', holder: 'alice', expires_at: new Date(Date.now() + 300_000).toISOString() };
    let held = false;
    const f = fakeFetch(
      routes({
        'GET /api/v1/scanners/sc-1': () => scanner({ control: held ? lease : null }),
        'POST /api/v1/scanners/sc-1/leases': () => {
          held = true;
          return lease;
        },
        'POST /api/v1/scanners/sc-1/commands': {},
        'POST /api/v1/scanners/sc-1/hold': {},
        'DELETE /api/v1/scanners/sc-1/leases/L-1': () => {
          held = false;
          return null;
        },
      }),
    );
    render(renderWith(<VirtualScanner />, { auth: authValue(['scanner_view', 'scanner_control'], f), ...ROUTE }));
    const take = await screen.findByRole('button', { name: 'Take control' });
    expect(screen.getByRole('button', { name: 'Menu key' })).toBeDisabled();
    await user.click(take);
    await screen.findByText(/You have control/);

    await user.click(screen.getByRole('button', { name: 'Menu key' }));
    await user.click(screen.getByRole('button', { name: 'Hold system' }));
    await waitFor(() => expect(f.mock.calls.some(([u]) => String(u).endsWith('/hold'))).toBe(true));

    const sent = f.mock.calls.filter(([, i]) => i?.method === 'POST' && !String(i?.body).includes('ttl_secs'));
    expect(sent.map(([u, i]) => [String(u), (i?.headers as Record<string, string>)['X-Lease-Id'], String(i?.body)])).toEqual([
      ['/api/v1/scanners/sc-1/commands', 'L-1', '{"command":"key","code":"M","action":"press"}'],
      ['/api/v1/scanners/sc-1/hold', 'L-1', '{"kind":"system","index":3}'],
    ]);

    await user.click(screen.getByRole('button', { name: 'Release control' }));
    await waitFor(() => expect(f.mock.calls.some(([u, i]) => i?.method === 'DELETE' && String(u).endsWith('/leases/L-1'))).toBe(true));
    expect(await screen.findByRole('button', { name: 'Take control' })).toBeInTheDocument();
  });

  it('shows who else holds control and lets admins revoke', async () => {
    const control = { lease_id: 'L-9', holder: 'bob', expires_at: new Date(Date.now() + 120_000).toISOString() };
    const f = fakeFetch(routes({ 'GET /api/v1/scanners/sc-1': scanner({ control }) }));
    render(renderWith(<VirtualScanner />, { auth: authValue(['scanner_view', 'scanner_control', 'scanner_admin'], f), ...ROUTE }));
    expect(await screen.findByText(/Controlled by bob/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Take control' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Revoke (admin)' })).toBeEnabled();
  });

  it('updates the display from the live stream without reloading', async () => {
    const f = fakeFetch(routes());
    const auth = authValue(['scanner_view'], f);
    const { rerender } = render(renderWith(<VirtualScanner />, { auth, ...ROUTE }));
    await screen.findByText('FD Main');
    const live = { ...emptyLive, telemetry: new Map([['sc-1', snapshot({ channel: 'PD North', tgid: '2101' })]]) };
    act(() => rerender(renderWith(<VirtualScanner />, { auth, live, ...ROUTE })));
    expect(screen.getByLabelText('Scanner display')).toHaveTextContent('PD North');
    expect(f.mock.calls.filter(([u]) => String(u).endsWith('/telemetry'))).toHaveLength(1);
  });
});

describe('live audio', () => {
  it('negotiates WHEP and hangs up', async () => {
    const user = userEvent.setup();
    const pcs: FakePc[] = [];
    class FakePc {
      iceGatheringState = 'complete';
      connectionState = 'new';
      localDescription: { sdp: string } | null = null;
      remote: { type: string; sdp: string } | null = null;
      ontrack: ((e: unknown) => void) | null = null;
      onconnectionstatechange: (() => void) | null = null;
      transceivers: unknown[] = [];
      closed = false;
      constructor() {
        pcs.push(this);
      }
      addTransceiver(kind: string, init: unknown) {
        this.transceivers.push([kind, init]);
      }
      async createOffer() {
        return { type: 'offer', sdp: 'v=0 offer' };
      }
      async setLocalDescription(d: { sdp: string }) {
        this.localDescription = d;
      }
      async setRemoteDescription(d: { type: string; sdp: string }) {
        this.remote = d;
        this.ontrack?.({ streams: [{}], track: {} });
      }
      addEventListener() {}
      removeEventListener() {}
      close() {
        this.closed = true;
      }
    }
    vi.stubGlobal('RTCPeerConnection', FakePc);
    vi.stubGlobal('MediaStream', class {});
    HTMLMediaElement.prototype.play = vi.fn(async () => undefined);
    const f = fakeFetch(
      routes({
        'POST /api/v1/scanners/sc-1/audio/whep': () =>
          new Response('v=0 answer', { status: 201, headers: { Location: '/api/v1/audio/sessions/s-1', 'Content-Type': 'application/sdp' } }),
        'DELETE /api/v1/audio/sessions/s-1': null,
      }),
    );
    render(renderWith(<VirtualScanner />, { auth: authValue(['scanner_view', 'audio_listen'], f), ...ROUTE }));
    expect(await screen.findByText(/Only listen to communications you are authorized/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Listen live' }));
    expect(await screen.findByText('Playing')).toBeInTheDocument();
    const post = f.mock.calls.find(([u, i]) => String(u).endsWith('/audio/whep') && i?.method === 'POST');
    expect((post?.[1]?.headers as Record<string, string>)['Content-Type']).toBe('application/sdp');
    expect(post?.[1]?.body).toBe('v=0 offer');
    expect(pcs[0]?.transceivers).toEqual([['audio', { direction: 'recvonly' }]]);
    expect(pcs[0]?.remote).toEqual({ type: 'answer', sdp: 'v=0 answer' });
    await user.click(screen.getByRole('button', { name: 'Stop' }));
    await waitFor(() => expect(f.mock.calls.some(([u, i]) => String(u) === '/api/v1/audio/sessions/s-1' && i?.method === 'DELETE')).toBe(true));
    expect(pcs[0]?.closed).toBe(true);
    vi.unstubAllGlobals();
  });

  it('explains when audio is disabled for the scanner', async () => {
    const f = fakeFetch(routes({ 'GET /api/v1/scanners/sc-1': scanner({ audio_enabled: false }) }));
    render(renderWith(<VirtualScanner />, { auth: authValue(['scanner_view', 'audio_listen'], f), ...ROUTE }));
    expect(await screen.findByText(/Audio is disabled for this scanner/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Listen live' })).not.toBeInTheDocument();
  });
});
