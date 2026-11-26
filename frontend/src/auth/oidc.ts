// OIDC sign-in, in its own module so `oidc-client-ts` is loaded only when
// the deployment uses OIDC (AuthContext imports it on demand).
import { InMemoryWebStorage, UserManager, WebStorageStateStore, type User } from 'oidc-client-ts';
import type { UiAuth } from './config';
import { SignInRequired, type Session } from './session';

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
