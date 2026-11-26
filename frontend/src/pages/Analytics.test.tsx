import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { authValue, fakeFetch, renderWith } from '../test/fixtures';
import { Analytics, bucketLabel } from './Analytics';

describe('Analytics', () => {
  it('summarises activity and ranks by the chosen field', async () => {
    const user = userEvent.setup();
    const f = fakeFetch({
      'GET /api/v1/analytics/activity': {
        bucket: 'hour',
        points: [
          { start: '2026-11-25T08:00:00Z', transmissions: 3, airtime_ms: 9000 },
          { start: '2026-11-25T09:00:00Z', transmissions: 1, airtime_ms: 1000 },
        ],
      },
      'GET /api/v1/analytics/top': (_: unknown) => ({ by: 'talkgroup', entries: [{ key: 'sc-1', transmissions: 3, airtime_ms: 9000 }] }),
    });
    render(renderWith(<Analytics />, { auth: authValue(['event_view'], f) }));
    expect(await screen.findByText('10.0 s')).toBeInTheDocument();
    expect(screen.getByText('4')).toBeInTheDocument();
    expect(screen.getByText('75%')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Scanners' }));
    // Scanner ids are shown as names.
    expect(await screen.findByRole('link', { name: 'NR Primary' })).toBeInTheDocument();
    await waitFor(() => {
      const urls = f.mock.calls.map((c) => new URL(String(c[0]), 'http://x'));
      expect(urls.some((u) => u.pathname.endsWith('/top') && u.searchParams.get('by') === 'scanner')).toBe(true);
      expect(urls.every((u) => u.searchParams.get('bucket') === 'hour')).toBe(true);
    });
  });
});

describe('bucket labels', () => {
  it('are unique across a 25-bucket day', () => {
    const start = Date.parse('2026-11-24T21:00:00Z');
    const labels = Array.from({ length: 25 }, (_, i) => bucketLabel(new Date(start + i * 3600e3).toISOString(), 'hour'));
    expect(new Set(labels).size).toBe(25);
    const days = Array.from({ length: 90 }, (_, i) => bucketLabel(new Date(start + i * 86400e3).toISOString(), 'day'));
    expect(new Set(days).size).toBe(90);
  });
});
