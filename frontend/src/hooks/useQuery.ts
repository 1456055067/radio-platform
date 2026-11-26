import { useCallback, useEffect, useRef, useState } from 'react';
import { describeError } from '../api/client';

export interface Query<T> {
  data: T | undefined;
  error: string | null;
  loading: boolean;
  reload: () => void;
}

/**
 * Runs `fn` on mount, whenever `deps` change, on `reload()`, and every
 * `intervalMs` if given. Stale responses are discarded.
 */
export function useQuery<T>(
  fn: (signal: AbortSignal) => Promise<T>,
  deps: unknown[],
  intervalMs?: number,
): Query<T> {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const fnRef = useRef(fn);
  fnRef.current = fn;

  useEffect(() => {
    const ctl = new AbortController();
    setLoading(true);
    fnRef
      .current(ctl.signal)
      .then((d) => {
        if (ctl.signal.aborted) return;
        setData(d);
        setError(null);
      })
      .catch((e: unknown) => {
        if (ctl.signal.aborted) return;
        setError(describeError(e));
      })
      .finally(() => {
        if (!ctl.signal.aborted) setLoading(false);
      });
    return () => ctl.abort();
  }, [...deps, tick]);

  useEffect(() => {
    if (!intervalMs) return;
    const id = setInterval(() => setTick((t) => t + 1), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);

  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { data, error, loading, reload };
}
