import { DevSession } from './session';

describe('DevSession', () => {
  it('caches tokens until a minute before expiry and dedupes concurrent fetches', async () => {
    let now = 0;
    let n = 0;
    const fetchImpl = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({ access_token: `t${++n}`, expires_in: 3600 })));
    const s = new DevSession('viewer', fetchImpl, () => now);
    const [a, b] = await Promise.all([s.token(), s.token()]);
    expect([a, b]).toEqual(['t1', 't1']);
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(String(fetchImpl.mock.calls[0]?.[0])).toBe('/dev/token?role=viewer');
    now = 3_539_000;
    expect(await s.token()).toBe('t1');
    now = 3_541_000;
    expect(await s.token()).toBe('t2');
  });
});
