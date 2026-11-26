import { ApiClient, ApiError, describeError, withQuery } from './client';

function respond(status: number, body: unknown, type = 'application/json') {
  return new Response(body === undefined ? '' : JSON.stringify(body), { status, headers: { 'content-type': type } });
}

describe('ApiClient', () => {
  it('sends the bearer token and lease header, and never retries', async () => {
    const fetchImpl = vi.fn<typeof fetch>(async () => respond(504, { status: 504, title: 'Gateway Timeout', detail: 'timed out; scanner state may have changed' }));
    const api = new ApiClient(async () => 'tok', '/api/v1', fetchImpl);
    await expect(api.request('/scanners/x/volume', { method: 'PUT', body: { level: 3 }, lease: 'L1' })).rejects.toMatchObject({ status: 504 });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    const [url, init] = fetchImpl.mock.calls[0] ?? [];
    expect(url).toBe('/api/v1/scanners/x/volume');
    const h = init?.headers as Record<string, string>;
    expect(h.Authorization).toBe('Bearer tok');
    expect(h['X-Lease-Id']).toBe('L1');
    expect(init?.credentials).toBe('omit');
    expect(init?.body).toBe('{"level":3}');
  });

  it('parses problem details and explains them', async () => {
    const api = new ApiClient(async () => 't', '/api/v1', async () => respond(428, { status: 428, title: 'Precondition Required' }, 'application/problem+json'));
    const e = await api.get('/x').catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect(describeError(e)).toBe('Take control of the scanner first.');
  });

  it('maps transport failures to status 0', async () => {
    const api = new ApiClient(async () => 't', '/api/v1', async () => {
      throw new TypeError('Failed to fetch');
    });
    await expect(api.get('/x')).rejects.toMatchObject({ status: 0 });
  });

  it('builds query strings without empty values', () => {
    expect(withQuery('/t', { a: 1, b: '', c: undefined, d: null, e: 'x y' })).toBe('/t?a=1&e=x+y');
    expect(withQuery('/t', {})).toBe('/t');
  });
});
