import type { Problem } from './types';

/** An RFC 9457 problem returned by the gateway (or a transport failure). */
export class ApiError extends Error {
  readonly status: number;
  readonly problem: Problem;
  constructor(problem: Problem) {
    super(problem.detail ?? problem.title ?? `HTTP ${problem.status}`);
    this.name = 'ApiError';
    this.status = problem.status;
    this.problem = problem;
  }
}

export type TokenSource = () => Promise<string>;

export interface RequestOptions {
  method?: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
  body?: unknown;
  lease?: string | null;
  query?: Record<string, string | number | undefined | null>;
  signal?: AbortSignal;
  keepalive?: boolean;
}

/** Builds `path?query`, dropping empty values. */
export function withQuery(path: string, query?: RequestOptions['query']): string {
  if (!query) return path;
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) {
    if (v !== undefined && v !== null && v !== '') qs.set(k, String(v));
  }
  const s = qs.toString();
  return s ? `${path}?${s}` : path;
}

/**
 * Thin fetch wrapper for `/api/v1`. It never retries: state-changing
 * commands must not be repeated automatically, and reads are refreshed by
 * the caller.
 */
export class ApiClient {
  constructor(
    private readonly token: TokenSource,
    private readonly base = '/api/v1',
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {}

  async request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
    const headers: Record<string, string> = {
      Accept: 'application/json, application/problem+json',
      Authorization: `Bearer ${await this.token()}`,
    };
    if (opts.body !== undefined) headers['Content-Type'] = 'application/json';
    if (opts.lease) headers['X-Lease-Id'] = opts.lease;
    let resp: Response;
    try {
      resp = await this.fetchImpl(this.base + withQuery(path, opts.query), {
        method: opts.method ?? 'GET',
        headers,
        body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
        signal: opts.signal,
        keepalive: opts.keepalive,
        credentials: 'omit',
        cache: 'no-store',
      });
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') throw e;
      throw new ApiError({ status: 0, title: 'Network error', detail: 'The gateway could not be reached.' });
    }
    const text = await resp.text();
    let data: unknown = null;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = null;
      }
    }
    if (!resp.ok) {
      const p = (data && typeof data === 'object' ? data : {}) as Partial<Problem>;
      throw new ApiError({ ...p, status: resp.status, title: p.title ?? resp.statusText });
    }
    return data as T;
  }

  /**
   * A raw authenticated request (for SDP and binary bodies). Non-2xx
   * responses throw [`ApiError`].
   */
  async raw(path: string, init: { method?: string; body?: BodyInit; contentType?: string; signal?: AbortSignal; keepalive?: boolean } = {}): Promise<Response> {
    const headers: Record<string, string> = { Authorization: `Bearer ${await this.token()}` };
    if (init.contentType) headers['Content-Type'] = init.contentType;
    let resp: Response;
    try {
      resp = await this.fetchImpl(path.startsWith('/api/') ? path : this.base + path, {
        method: init.method ?? 'GET',
        headers,
        body: init.body,
        signal: init.signal,
        keepalive: init.keepalive,
        credentials: 'omit',
        cache: 'no-store',
      });
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') throw e;
      throw new ApiError({ status: 0, title: 'Network error', detail: 'The gateway could not be reached.' });
    }
    if (!resp.ok) {
      let p: Partial<Problem> = {};
      try {
        p = (await resp.json()) as Partial<Problem>;
      } catch {
        /* not JSON */
      }
      throw new ApiError({ ...p, status: resp.status, title: p.title ?? resp.statusText });
    }
    return resp;
  }

  get<T>(path: string, query?: RequestOptions['query'], signal?: AbortSignal) {
    return this.request<T>(path, { query, signal });
  }
}

/** Human explanation for common control failures. */
export function describeError(e: unknown): string {
  if (e instanceof ApiError) {
    switch (e.status) {
      case 0:
        return e.message;
      case 401:
        return 'Your session has expired. Sign in again.';
      case 403:
        return e.problem.detail ?? 'You are not allowed to do that.';
      case 409:
        return e.problem.detail ?? 'Conflict: someone else may have control.';
      case 428:
        return 'Take control of the scanner first.';
      case 429:
        return 'Too many requests. Wait a moment and try again.';
      case 501:
        return 'This scanner firmware does not support that command.';
      case 503:
        return e.problem.detail ?? 'The scanner or its site agent is offline.';
      case 504:
        return e.problem.detail ?? 'The scanner did not answer in time. Check its state before retrying.';
      default:
        return e.message;
    }
  }
  return e instanceof Error ? e.message : String(e);
}
