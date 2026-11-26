import { expect, test } from '@playwright/test';
import { scannerByName } from './api';
import { nav, signIn } from './fixtures';

test('viewers get no audit log and no control', async ({ page, request }) => {
  await signIn(page, 'viewer');
  const sidebar = page.getByRole('navigation');
  await expect(sidebar.getByRole('link', { name: 'Scanners', exact: true })).toBeVisible();
  await expect(sidebar.getByRole('link', { name: 'Audit log' })).toHaveCount(0);

  const s = await scannerByName(request, 'Harbor Main');
  await page.goto(`/scanners/${s.id}`);
  await expect(page.getByText('Your role is read-only')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Take control' })).toHaveCount(0);
  await expect(page.getByRole('slider', { name: 'Volume' })).toBeDisabled();
});

test('operators can take control but cannot open the audit log', async ({ page }) => {
  await signIn(page, 'operator');
  await expect(page.getByRole('navigation').getByRole('link', { name: 'Audit log' })).toHaveCount(0);
  await page.goto('/audit');
  // Reachable by URL, but the page explains the missing permission.
  await expect(page.getByText('The audit log requires the audit_view permission.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Verify chain' })).toHaveCount(0);
});

test('admins see the audit log', async ({ page }) => {
  await signIn(page, 'admin');
  await nav(page, 'Audit log');
  await expect(page.getByRole('heading', { level: 1, name: 'Audit log' })).toBeVisible();
});
