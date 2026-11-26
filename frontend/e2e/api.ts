import type { APIRequestContext } from '@playwright/test';
import type { Page, Scanner, Transmission } from '../src/api/types';

export type Role = 'admin' | 'operator' | 'viewer';

/** Fetches a development token from radio-devstack's `/dev/token`. */
export async function token(request: APIRequestContext, role: Role): Promise<string> {
  const r = await request.get(`/dev/token?role=${role}`);
  if (!r.ok()) throw new Error(`/dev/token: HTTP ${r.status()}`);
  const body = (await r.json()) as { access_token: string };
  return body.access_token;
}

export async function apiGet<T>(request: APIRequestContext, role: Role, path: string): Promise<T> {
  const r = await request.get(`/api/v1${path}`, { headers: { authorization: `Bearer ${await token(request, role)}` } });
  if (!r.ok()) throw new Error(`GET ${path}: HTTP ${r.status()}`);
  return (await r.json()) as T;
}

export async function scanners(request: APIRequestContext): Promise<Scanner[]> {
  return (await apiGet<Page<Scanner>>(request, 'admin', '/scanners?limit=100')).items;
}

export async function scannerByName(request: APIRequestContext, name: string): Promise<Scanner> {
  const s = (await scanners(request)).find((x) => x.name === name);
  if (!s) throw new Error(`no scanner named ${name}`);
  return s;
}

export async function transmissions(request: APIRequestContext, query: string): Promise<Transmission[]> {
  return (await apiGet<Page<Transmission>>(request, 'admin', `/transmissions?${query}`)).items;
}

/**
 * Reads the volume straight from the scanner (read-only `get_volume`
 * command; no lease needed). Telemetry snapshots are not used here: they
 * are sent only when something changes and can lag a command.
 */
export async function scannerVolume(request: APIRequestContext, id: string): Promise<number> {
  const r = await request.post(`/api/v1/scanners/${id}/commands`, {
    headers: { authorization: `Bearer ${await token(request, 'admin')}` },
    data: { command: 'get_volume' },
  });
  if (!r.ok()) throw new Error(`get_volume: HTTP ${r.status()}`);
  return ((await r.json()) as { level: number }).level;
}

/** Revokes any control lease on the scanner (test cleanup). */
export async function revokeLease(request: APIRequestContext, name: string): Promise<void> {
  const s = await scannerByName(request, name);
  const lease = s.control?.lease_id;
  if (!lease) return;
  await request.delete(`/api/v1/scanners/${s.id}/leases/${lease}`, {
    headers: { authorization: `Bearer ${await token(request, 'admin')}` },
  });
}
