import { createContext, useContext, useMemo, type ReactNode } from 'react';
import { fetchAll } from '../api/paging';
import type { Agent, Scanner, Site } from '../api/types';
import { useAuth } from '../auth/AuthContext';
import { useQuery } from './useQuery';

export interface DirectoryValue {
  sites: Site[];
  scanners: Scanner[];
  agents: Agent[];
  siteById: ReadonlyMap<string, Site>;
  scannerById: ReadonlyMap<string, Scanner>;
  loading: boolean;
  error: string | null;
  reload: () => void;
}

const Ctx = createContext<DirectoryValue | null>(null);

export function useDirectory(): DirectoryValue {
  const v = useContext(Ctx);
  if (!v) throw new Error('useDirectory outside DirectoryProvider');
  return v;
}

export function DirectoryValueProvider({ value, children }: { value: DirectoryValue; children: ReactNode }) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function makeDirectory(
  sites: Site[],
  scanners: Scanner[],
  agents: Agent[],
  extra: Partial<DirectoryValue> = {},
): DirectoryValue {
  return {
    sites,
    scanners,
    agents,
    siteById: new Map(sites.map((s) => [s.id, s])),
    scannerById: new Map(scanners.map((s) => [s.id, s])),
    loading: false,
    error: null,
    reload: () => {},
    ...extra,
  };
}

const REFRESH_MS = 15_000;

/**
 * Sites, scanners and agents, refreshed every 15 s. Agent connectivity is
 * not on the live stream, so this refresh is what notices an agent that
 * dropped off.
 */
export function DirectoryProvider({ children }: { children: ReactNode }) {
  const { api } = useAuth();
  const q = useQuery(
    async (signal) => {
      const [sites, scanners, agents] = await Promise.all([
        fetchAll<Site>(api, '/sites', {}, 2000, signal),
        fetchAll<Scanner>(api, '/scanners', {}, 2000, signal),
        fetchAll<Agent>(api, '/agents', {}, 2000, signal),
      ]);
      return { sites: sites.items, scanners: scanners.items, agents: agents.items };
    },
    [api],
    REFRESH_MS,
  );
  const value = useMemo(
    () =>
      makeDirectory(q.data?.sites ?? [], q.data?.scanners ?? [], q.data?.agents ?? [], {
        loading: q.loading && !q.data,
        error: q.error,
        reload: q.reload,
      }),
    [q.data, q.loading, q.error, q.reload],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
