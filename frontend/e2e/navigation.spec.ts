import { expect, test } from '@playwright/test';
import { nav, signIn } from './fixtures';

test('dashboard shows the simulated fleet', async ({ page }) => {
  await signIn(page, 'operator');
  await expect(page.getByRole('heading', { level: 1, name: 'Dashboard' })).toBeVisible();
  await expect(page.getByText('Development stack')).toHaveCount(0);
  for (const name of ['NR Primary', 'NR Secondary', 'Harbor Main', 'Harbor Spare (offline)']) {
    await expect(page.getByText(name, { exact: true }).first()).toBeVisible();
  }
});

test('scanners page lists every scanner and opens the virtual scanner', async ({ page }) => {
  await signIn(page, 'operator');
  await nav(page, 'Scanners');
  await expect(page.getByRole('heading', { level: 1, name: 'Scanners' })).toBeVisible();
  const table = page.getByRole('table');
  for (const name of ['NR Primary', 'NR Secondary', 'Harbor Main', 'Harbor Spare (offline)']) {
    await expect(table.getByRole('link', { name, exact: true })).toBeVisible();
  }
  await table.getByRole('link', { name: 'Harbor Main', exact: true }).click();
  await expect(page).toHaveURL(/\/scanners\/[0-9a-f-]+$/);
  await expect(page.getByRole('heading', { name: 'Control' })).toBeVisible();
});

test('the role survives a reload and sign-out returns to the role picker', async ({ page }) => {
  await signIn(page, 'viewer');
  await page.reload();
  await expect(page.getByRole('heading', { level: 1, name: 'Dashboard' })).toBeVisible();
  await page.getByRole('button', { name: 'Dev viewer' }).click();
  await page.getByRole('menuitem', { name: 'Sign out' }).click();
  await expect(page.getByRole('heading', { name: 'Sign in as…' })).toBeVisible();
});

test('unknown routes show the not-found page', async ({ page }) => {
  await signIn(page, 'viewer', '/no-such-page');
  await expect(page.getByRole('heading', { level: 1, name: 'Page not found' })).toBeVisible();
});
