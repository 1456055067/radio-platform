import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Box from '@cloudscape-design/components/box';
import Spinner from '@cloudscape-design/components/spinner';
import { ApiClient, describeError } from '../api/client';
import type { Me, Permission } from '../api/types';
import { loadUiConfig } from './config';
import { DEV_ROLE_KEY, DevSession, SignInRequired, type DevRole, type Session } from './session';
import { DevSignIn } from './DevSignIn';

export interface AuthValue {
  session: Session;
  api: ApiClient;
  me: Me;
  can: (p: Permission) => boolean;
}

const Ctx = createContext<AuthValue | null>(null);

export function useAuth(): AuthValue {
  const v = useContext(Ctx);
  if (!v) throw new Error('useAuth outside AuthProvider');
  return v;
}

/** Test helper: provide a ready-made auth value. */
export function AuthValueProvider({ value, children }: { value: AuthValue; children: ReactNode }) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

function readDevRole(): DevRole | null {
  try {
    const r = sessionStorage.getItem(DEV_ROLE_KEY);
    return r === 'admin' || r === 'operator' || r === 'viewer' ? r : null;
  } catch {
    return null;
  }
}

type Phase =
  | { k: 'loading' }
  | { k: 'dev-choose' }
  | { k: 'redirecting' }
  | { k: 'error'; message: string }
  | { k: 'ready'; value: AuthValue };

export function AuthProvider({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<Phase>({ k: 'loading' });
  const [devRole, setDevRole] = useState<DevRole | null>(readDevRole);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const cfg = await loadUiConfig();
      let session: Session;
      if (cfg.auth.mode === 'dev') {
        if (!devRole) {
          if (!cancelled) setPhase({ k: 'dev-choose' });
          return;
        }
        session = new DevSession(devRole);
      } else {
        const { OidcSession } = await import('./oidc');
        const s = new OidcSession(cfg.auth);
        const path = await s.init();
        if (path !== window.location.pathname + window.location.search) window.history.replaceState(null, '', path);
        session = s;
      }
      const api = new ApiClient(() => session.token());
      const me = await api.get<Me>('/me');
      const perms = new Set(me.permissions);
      if (!cancelled) setPhase({ k: 'ready', value: { session, api, me, can: (p) => perms.has(p) } });
    })().catch((e: unknown) => {
      if (cancelled) return;
      if (e instanceof SignInRequired) setPhase({ k: 'redirecting' });
      else setPhase({ k: 'error', message: describeError(e) });
    });
    return () => {
      cancelled = true;
    };
  }, [devRole]);

  const content = useMemo(() => {
    switch (phase.k) {
      case 'loading':
      case 'redirecting':
        return (
          <Box textAlign="center" padding="xxl">
            <Spinner size="large" />
            <Box variant="p" color="text-body-secondary">
              {phase.k === 'redirecting' ? 'Redirecting to sign in…' : 'Loading…'}
            </Box>
          </Box>
        );
      case 'dev-choose':
        return (
          <DevSignIn
            onChoose={(r) => {
              try {
                sessionStorage.setItem(DEV_ROLE_KEY, r);
              } catch {
                /* ignore */
              }
              setDevRole(r);
              setPhase({ k: 'loading' });
            }}
          />
        );
      case 'error':
        return (
          <Box padding="xxl">
            <Alert type="error" header="Cannot start the console">
              {phase.message}
            </Alert>
          </Box>
        );
      case 'ready':
        return <Ctx.Provider value={phase.value}>{children}</Ctx.Provider>;
    }
  }, [phase, children]);

  return content;
}
