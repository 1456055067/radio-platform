import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { authValue, fakeFetch, renderWith } from '../test/fixtures';
import { Audit } from './Audit';

const record = { id: 1, at: '2026-11-25T10:00:00Z', tenant_id: 't', actor: 'alice', action: 'scanner.update', resource_type: 'scanner', resource_id: 'abc', outcome: 'success', details: {}, source_ip: null, seq: 1, prev_hash: '00', hash: 'aa' };

describe('Audit page', () => {
  it('filters, and reports a broken chain plainly', async () => {
    const user = userEvent.setup();
    const f = fakeFetch({
      'GET /api/v1/audit': { items: [record], next_cursor: null },
      'GET /api/v1/audit/verify': { ok: false, verified: 41, unchained: 0, head_seq: 50, head_hash: 'ff', failure: { seq: 42, reason: 'content does not match its hash (altered)' } },
    });
    render(renderWith(<Audit />, { auth: authValue(['audit_view'], f) }));
    expect(await screen.findByText('scanner.update')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Export matching records' })).not.toBeInTheDocument();
    await user.type(screen.getByPlaceholderText('scanner.'), 'site.');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    await waitFor(() => expect(f.mock.calls.some((c) => new URL(String(c[0]), 'http://x').searchParams.get('action') === 'site.')).toBe(true));
    await user.click(screen.getByRole('button', { name: 'Verify chain' }));
    expect(await screen.findByText('The audit chain is broken')).toBeInTheDocument();
    expect(screen.getByText(/Record #42: content does not match its hash/)).toBeInTheDocument();
  });

  it('offers export only with the export permission', async () => {
    render(renderWith(<Audit />, { auth: authValue(['audit_view', 'export'], fakeFetch({ 'GET /api/v1/audit': { items: [], next_cursor: null } })) }));
    expect(await screen.findByRole('button', { name: 'Export matching records' })).toBeInTheDocument();
  });
});
