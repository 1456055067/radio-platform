import { render, screen, within } from '@testing-library/react';
import { emptyLive } from '../hooks/LiveContext';
import { authValue, fakeFetch, renderWith, scanner, snapshot } from '../test/fixtures';
import { Dashboard } from './Dashboard';

describe('Dashboard', () => {
  it('counts scanners and shows receiving scanners and service status honestly', async () => {
    const scanners = [
      scanner(),
      scanner({ id: 'sc-2', name: 'Spare', health: { state: 'unreachable', consecutive_failures: 5 } }),
      scanner({ id: 'sc-3', name: 'Off', enabled: false }),
    ];
    const live = { ...emptyLive, state: 'open' as const, telemetry: new Map([['sc-1', snapshot({ muted: false })]]) };
    const api = fakeFetch({
      'GET /api/v1/transcription/status': { enabled: false, service: null, queue: { pending: 0, done: 0, failed: 0 }, worker: null },
      'GET /api/v1/retention': { enabled: true, recordings_days: 30, transcripts_days: null, transmissions_days: 365, events_days: null, last_sweep: null },
    });
    render(renderWith(<Dashboard />, { auth: authValue(['scanner_view'], api), scanners, live }));
    const kpi = (label: string) => screen.getAllByText(label)[0]?.nextElementSibling?.textContent;
    expect(kpi('Total scanners')).toBe('3');
    expect(kpi('Online')).toBe('1');
    expect(kpi('Offline or disabled')).toBe('2');
    expect(kpi('Receiving now')).toBe('1');
    expect(screen.getAllByText('FD Main').length).toBeGreaterThan(0);
    expect(await screen.findByText('recordings 30 d · transmissions 365 d')).toBeInTheDocument();
    expect(screen.getAllByText('Off (default)').length).toBeGreaterThan(0);
    expect(within(screen.getByLabelText('Relative site locations')).getByText('North Ridge')).toBeInTheDocument();
  });
});
