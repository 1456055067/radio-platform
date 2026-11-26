import { useCallback, useEffect, useRef, useState } from 'react';
import { ApiError, describeError, type ApiClient } from '../api/client';
import type { ControlView } from '../api/types';

export const LEASE_TTL_SECS = 300;
export const RENEW_EVERY_MS = 120_000;

export interface LeaseState {
  /** Our lease, if we hold control. */
  lease: { id: string; expires_at: string } | null;
  busy: boolean;
  error: string | null;
  acquire: () => Promise<void>;
  release: () => Promise<void>;
  /** Admin: revoke someone else's lease by id. */
  revoke: (leaseId: string) => Promise<void>;
}

/**
 * Exclusive control of one scanner. Renews every two minutes while this
 * page is open and releases on leave, so an abandoned tab frees the
 * scanner within one TTL at most.
 */
export function useLease(
  api: ApiClient,
  scannerId: string,
  subject: string,
  control: ControlView | null | undefined,
  /** When `control` was fetched (ms since epoch). */
  controlAt: number,
  onLost: (why: string) => void,
): LeaseState {
  const [lease, setLease] = useState<LeaseState['lease']>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const leaseRef = useRef(lease);
  leaseRef.current = lease;
  const acquiredAt = useRef(0);
  const onLostRef = useRef(onLost);
  onLostRef.current = onLost;

  // Adopt a lease we already hold (for example after a reload), and notice
  // when a scanner view fetched after we acquired no longer shows it.
  useEffect(() => {
    if (control === undefined) return;
    const mine = leaseRef.current;
    if (!mine) {
      if (control && control.holder === subject && control.lease_id) {
        acquiredAt.current = controlAt;
        setLease({ id: control.lease_id, expires_at: control.expires_at });
      }
      return;
    }
    if (controlAt > acquiredAt.current && (!control || control.holder !== subject)) {
      setLease(null);
      onLostRef.current(control ? `Control was taken over by ${control.holder}.` : 'Control was released or revoked.');
    }
  }, [control, controlAt, subject]);

  const post = useCallback(
    () =>
      api.request<ControlView>(`/scanners/${scannerId}/leases`, {
        method: 'POST',
        body: { ttl_secs: LEASE_TTL_SECS },
      }),
    [api, scannerId],
  );

  const acquire = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const c = await post();
      acquiredAt.current = Date.now();
      if (c.lease_id) setLease({ id: c.lease_id, expires_at: c.expires_at });
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }, [post]);

  const release = useCallback(async () => {
    const l = leaseRef.current;
    if (!l) return;
    setBusy(true);
    try {
      await api.request(`/scanners/${scannerId}/leases/${l.id}`, { method: 'DELETE' });
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 404)) setError(describeError(e));
    } finally {
      setLease(null);
      setBusy(false);
    }
  }, [api, scannerId]);

  const revoke = useCallback(
    async (leaseId: string) => {
      setBusy(true);
      setError(null);
      try {
        await api.request(`/scanners/${scannerId}/leases/${leaseId}`, { method: 'DELETE' });
      } catch (e) {
        setError(describeError(e));
      } finally {
        setBusy(false);
      }
    },
    [api, scannerId],
  );

  // Renew while held.
  useEffect(() => {
    if (!lease) return;
    const id = setInterval(() => {
      post()
        .then((c) => {
          if (c.lease_id) setLease({ id: c.lease_id, expires_at: c.expires_at });
        })
        .catch((e: unknown) => {
          setLease(null);
          onLostRef.current(`Control lost: ${describeError(e)}`);
        });
    }, RENEW_EVERY_MS);
    return () => clearInterval(id);
  }, [lease?.id, post]);

  // Release when leaving the page or closing the tab.
  useEffect(() => {
    const releaseNow = () => {
      const l = leaseRef.current;
      if (l) {
        void api
          .request(`/scanners/${scannerId}/leases/${l.id}`, { method: 'DELETE', keepalive: true })
          .catch(() => undefined);
      }
    };
    window.addEventListener('pagehide', releaseNow);
    return () => {
      window.removeEventListener('pagehide', releaseNow);
      releaseNow();
    };
  }, [api, scannerId]);

  return { lease, busy, error, acquire, release, revoke };
}
