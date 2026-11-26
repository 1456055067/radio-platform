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
