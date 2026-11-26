import type { ApiClient } from './client';
import type { Page } from './types';

/** Follows `next_cursor` until exhausted or `max` items are collected. */
export async function fetchAll<T>(
  api: ApiClient,
  path: string,
  query: Record<string, string | number | undefined | null> = {},
  max = 2000,
  signal?: AbortSignal,
): Promise<{ items: T[]; truncated: boolean }> {
  const items: T[] = [];
  let cursor: string | null = null;
  do {
    const page: Page<T> = await api.get<Page<T>>(path, { ...query, limit: 200, cursor }, signal);
    items.push(...page.items);
    cursor = page.next_cursor;
  } while (cursor && items.length < max);
  return { items: items.slice(0, max), truncated: cursor !== null || items.length > max };
}
