import { expect, test } from '@playwright/test';
import { nav, signIn } from './fixtures';

const scripts = (urls: string[]) => urls.filter((u) => /\/assets\/.+\.js$/.test(u)).map((u) => u.replace(/.*\/assets\//, ''));

test('pages load on demand and the OIDC client is not loaded in dev mode', async ({ page }) => {
  const loaded: string[] = [];
  page.on('request', (r) => loaded.push(r.url()));
  await signIn(page, 'admin');
  await expect(page.getByRole('heading', { level: 1, name: 'Dashboard' })).toBeVisible();
  expect(scripts(loaded).some((f) => f.startsWith('Dashboard-'))).toBe(true);
  expect(scripts(loaded).some((f) => f.startsWith('Analytics-'))).toBe(false);
  expect(scripts(loaded).some((f) => f.startsWith('oidc-'))).toBe(false);

  await nav(page, 'Analytics');
  await expect(page.getByRole('heading', { level: 1, name: 'Analytics' })).toBeVisible();
  expect(scripts(loaded).some((f) => f.startsWith('Analytics-'))).toBe(true);
});

test('a page chunk that fails to load offers a reload instead of a blank page', async ({ page }) => {
  await signIn(page, 'admin');
  await expect(page.getByRole('heading', { level: 1, name: 'Dashboard' })).toBeVisible();
  // As after a redeploy: the tab's build references a chunk the server no longer has.
  await page.route(/\/assets\/Audit-[^/]+\.js$/, (r) => r.fulfill({ status: 404, body: 'not found' }));
  await nav(page, 'Audit log');
  await expect(page.getByText('This page could not be loaded')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Reload' })).toBeVisible();

  // Navigation still works and clears the error.
  await nav(page, 'Scanners');
  await expect(page.getByRole('heading', { level: 1, name: 'Scanners' })).toBeVisible();

  await page.unroute(/\/assets\/Audit-[^/]+\.js$/);
  await page.reload();
  await nav(page, 'Audit log');
  await expect(page.getByRole('heading', { level: 1, name: 'Audit log' })).toBeVisible();
});

test('the gateway answers 404 for a chunk from an older build', async ({ request }) => {
  const r = await request.get('/assets/Audit-OLDBUILD.js');
  expect(r.status()).toBe(404);
  expect(r.headers()['content-type'] ?? '').not.toContain('text/html');
});
