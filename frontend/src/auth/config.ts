export type UiAuth =
  | { mode: 'oidc'; authority: string; client_id: string; scope: string }
  | { mode: 'dev' };

export interface UiConfig {
  auth: UiAuth;
}

/** Loads `/ui-config.json` (public sign-in settings served by the gateway). */
export async function loadUiConfig(fetchImpl: typeof fetch = fetch): Promise<UiConfig> {
  const r = await fetchImpl('/ui-config.json', { cache: 'no-cache', credentials: 'omit' });
  if (!r.ok) throw new Error(`Could not load /ui-config.json (HTTP ${r.status}).`);
  const c = (await r.json()) as Partial<UiConfig>;
  const a = c.auth as Partial<UiAuth> | undefined;
  if (a?.mode === 'dev') return { auth: { mode: 'dev' } };
  if (a?.mode === 'oidc' && typeof a.authority === 'string' && typeof a.client_id === 'string') {
    return { auth: { mode: 'oidc', authority: a.authority, client_id: a.client_id, scope: a.scope ?? 'openid profile' } };
  }
  throw new Error('ui-config.json has no usable auth settings.');
}
