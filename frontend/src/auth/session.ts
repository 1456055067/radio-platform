import { InMemoryWebStorage, UserManager, WebStorageStateStore, type User } from 'oidc-client-ts';
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

export const CALLBACK_PATH = '/auth/callback';

/**
 * OIDC public client. Tokens live in memory only (never localStorage), so
 * a reload re-runs the redirect; the identity provider's own session makes
 * that quick. The PKCE state for an in-flight redirect is kept in
 * sessionStorage, which the library clears on completion.
 */
export class OidcSession implements Session {
  readonly mode = 'oidc' as const;
  private readonly um: UserManager;
  private user: User | null = null;

  constructor(cfg: Extract<UiAuth, { mode: 'oidc' }>) {
    const origin = window.location.origin;
    this.um = new UserManager({
      authority: cfg.authority,
      client_id: cfg.client_id,
      scope: cfg.scope,
      response_type: 'code',
      redirect_uri: origin + CALLBACK_PATH,
      post_logout_redirect_uri: origin + '/',
      userStore: new WebStorageStateStore({ store: new InMemoryWebStorage() }),
      stateStore: new WebStorageStateStore({ store: window.sessionStorage }),
      automaticSilentRenew: true,
      monitorSession: false,
    });
    this.um.events.addUserLoaded((u) => {
      this.user = u;
    });
    this.um.events.addUserUnloaded(() => {
      this.user = null;
    });
  }

  /** Completes a redirect if we are on the callback; returns the path to show. */
  async init(): Promise<string> {
    if (window.location.pathname === CALLBACK_PATH) {
      const u = await this.um.signinRedirectCallback();
      this.user = u;
      const back = typeof u.state === 'string' && u.state.startsWith('/') && !u.state.startsWith('//') ? u.state : '/';
      return back;
    }
    this.user = await this.um.getUser();
    if (!this.user || this.user.expired) {
      await this.um.signinRedirect({ state: window.location.pathname + window.location.search });
      throw new SignInRequired();
    }
    return window.location.pathname + window.location.search;
  }

  async token(): Promise<string> {
    if (this.user && !this.user.expired) return this.user.access_token;
    try {
      const u = await this.um.signinSilent();
      if (u && !u.expired) {
        this.user = u;
        return u.access_token;
      }
    } catch {
      /* fall through to a full redirect */
    }
    await this.um.signinRedirect({ state: window.location.pathname + window.location.search });
    throw new SignInRequired();
  }

  async signOut(): Promise<void> {
    await this.um.removeUser();
    try {
      await this.um.signoutRedirect();
    } catch {
      window.location.assign('/');
    }
  }
}
