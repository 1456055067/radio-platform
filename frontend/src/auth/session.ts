import type { UiAuth } from './config';

/** Supplies bearer tokens for API calls and the live stream. */
export interface Session {
  readonly mode: UiAuth['mode'];
  /** A currently valid access token. Rejects if the user must sign in. */
  token(): Promise<string>;
  signOut(): Promise<void>;
  /** Development role, if any. */
  readonly devRole?: DevRole;
}

export type DevRole = 'admin' | 'operator' | 'viewer';
export const DEV_ROLE_KEY = 'radio-platform.dev-role';

export class SignInRequired extends Error {
  constructor() {
    super('sign-in required');
    this.name = 'SignInRequired';
  }
}

// ---------------- development tokens ----------------

/**
 * Development only: tokens come from `radio-devstack`'s `/dev/token`. The
 * production gateway has no such route, so this mode cannot work there.
 */
export class DevSession implements Session {
  readonly mode = 'dev' as const;
  private cached: { token: string; expires: number } | null = null;
  private inflight: Promise<string> | null = null;

  constructor(
    readonly devRole: DevRole,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
    private readonly now: () => number = Date.now,
  ) {}

  token(): Promise<string> {
    if (this.cached && this.cached.expires - 60_000 > this.now()) return Promise.resolve(this.cached.token);
    this.inflight ??= this.fetchToken().finally(() => {
      this.inflight = null;
    });
    return this.inflight;
  }

  private async fetchToken(): Promise<string> {
    const r = await this.fetchImpl(`/dev/token?role=${encodeURIComponent(this.devRole)}`, { cache: 'no-store' });
    if (!r.ok) throw new Error(`/dev/token failed (HTTP ${r.status})`);
    const body = (await r.json()) as { access_token: string; expires_in: number };
    this.cached = { token: body.access_token, expires: this.now() + body.expires_in * 1000 };
    return body.access_token;
  }

  async signOut(): Promise<void> {
    this.cached = null;
    try {
      sessionStorage.removeItem(DEV_ROLE_KEY);
    } catch {
      /* storage may be unavailable */
    }
    window.location.assign('/');
  }
}

// ---------------- OIDC (authorization code + PKCE) ----------------
