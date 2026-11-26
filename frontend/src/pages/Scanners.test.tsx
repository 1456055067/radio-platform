import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { authValue, fakeFetch, renderWith, scanner } from '../test/fixtures';
import { Scanners, validateScanner } from './Scanners';
import { parseNetworks } from './Sites';

describe('scanner form validation', () => {
  const ok = { site_id: 's', agent_id: 'a', name: 'X', host: '192.168.1.5', port: '50536', enabled: true };
  it('accepts IP literals only', () => {
    expect(validateScanner(ok, true)).toEqual({});
    expect(validateScanner({ ...ok, host: 'scanner.local' }, true).host).toBeDefined();
    expect(validateScanner({ ...ok, host: '300.1.1.1' }, true).host).toBeDefined();
    expect(validateScanner({ ...ok, host: 'fd00::5' }, true)).toEqual({});
    expect(validateScanner({ ...ok, port: '70000' }, true).port).toBeDefined();
    expect(validateScanner({ ...ok, site_id: '', agent_id: '' }, false)).toEqual({});
  });
  it('parses authorized networks', () => {
    expect(parseNetworks('192.168.1.0/24, 10.0.0.0/8\nfd00::/64')).toEqual({ nets: ['192.168.1.0/24', '10.0.0.0/8', 'fd00::/64'] });
    expect(parseNetworks('0.0.0.0/0').error).toMatch(/every address/);
    expect(parseNetworks('192.168.1.1').error).toMatch(/CIDR/);
  });
});

describe('Scanners page', () => {
  it('hides management actions without scanner_manage', async () => {
    render(renderWith(<Scanners />, { auth: authValue(['scanner_view'], fakeFetch({})) }));
    expect(await screen.findByRole('link', { name: 'NR Primary' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Enroll scanner' })).not.toBeInTheDocument();
  });

  it('enrolls a scanner through the API', async () => {
    const user = userEvent.setup();
    const f = fakeFetch({ 'POST /api/v1/scanners': (b: unknown) => ({ ...scanner(), ...(b as object) }) });
    render(renderWith(<Scanners />, { auth: authValue(['scanner_view', 'scanner_manage'], f) }));
    await user.click(screen.getByRole('button', { name: 'Enroll scanner' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Enroll' }));
    expect(within(dialog).getByText(/Choose the agent/)).toBeInTheDocument();
    expect(f).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole('button', { name: /Choose an agent/ }));
    await user.click(await screen.findByRole('option', { name: /agent-a/ }));
    const [name, host] = within(dialog).getAllByRole('textbox');
    await user.type(name as HTMLElement, 'Rack 2');
    await user.type(host as HTMLElement, '192.168.50.22');
    await user.click(within(dialog).getByRole('button', { name: 'Enroll' }));
    await waitFor(() => expect(f).toHaveBeenCalledTimes(1));
    expect(JSON.parse(String(f.mock.calls[0]?.[1]?.body))).toEqual({
      name: 'Rack 2',
      host: '192.168.50.22',
      port: 50536,
      enabled: true,
      site_id: 'site-1',
      agent_id: 'agent-a',
    });
  });
});

describe('discovery', () => {
  it('probes through the API and prefills enrollment from a result', async () => {
    const user = userEvent.setup();
    const f = fakeFetch({
      'POST /api/v1/sites/site-1/discover': { candidates: [{ host: '192.168.50.77', port: 50536, model: 'SDS200' }], skipped_enrolled: 1 },
    });
    render(renderWith(<Scanners />, { auth: authValue(['scanner_view', 'scanner_manage'], f) }));
    await user.click(screen.getByRole('button', { name: 'Discover' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/never probed/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole('button', { name: 'Probe' }));
    expect(await within(dialog).findByText('192.168.50.77:50536')).toBeInTheDocument();
    expect(JSON.parse(String(f.mock.calls[0]?.[1]?.body))).toEqual({ agent_id: 'agent-a', networks: ['192.168.50.0/24'], port: 50536 });
    await user.click(within(dialog).getByRole('button', { name: 'Enroll…' }));
    const enroll = await screen.findByRole('dialog');
    expect(within(enroll).getByDisplayValue('192.168.50.77')).toBeInTheDocument();
    expect(within(enroll).getByDisplayValue('SDS200 192.168.50.77')).toBeInTheDocument();
  });
});
