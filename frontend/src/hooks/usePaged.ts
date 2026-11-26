import { useCallback, useEffect, useRef, useState } from 'react';
import { describeError, type ApiClient } from '../api/client';
import type { Page } from '../api/types';

/** Cursor-paged list with "load more"; restarts when `query` changes. */
export function usePaged<T>(api: ApiClient, path: string, query: Record<string, string | undefined>) {
  const [items, setItems] = useState<T[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const key = JSON.stringify(query);
  const gen = useRef(0);

  const fetchPage = useCallback(
    async (after: string | null) => {
      const g = after ? gen.current : ++gen.current;
      setLoading(true);
      setError(null);
      try {
        const p = await api.get<Page<T>>(path, { ...(JSON.parse(key) as Record<string, string>), limit: 100, cursor: after });
        if (g !== gen.current) return;
        setItems((prev) => (after ? [...prev, ...p.items] : p.items));
        setCursor(p.next_cursor);
      } catch (e) {
        if (g === gen.current) setError(describeError(e));
      } finally {
        if (g === gen.current) setLoading(false);
      }
    },
    [api, path, key],
  );

  useEffect(() => {
    void fetchPage(null);
  }, [fetchPage]);

  return {
    items,
    loading,
    error,
    hasMore: cursor !== null,
    loadMore: () => void fetchPage(cursor),
    refresh: () => void fetchPage(null),
  };
}
