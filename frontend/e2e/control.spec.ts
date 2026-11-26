import { expect, test } from '@playwright/test';
import { apiGet, scannerByName } from './api';
import type { TelemetryResponse } from '../src/api/types';
import { signIn } from './fixtures';

test('control lease: an operator sets the volume and a second user is locked out', async ({ page, browser, request }) => {
  const s = await scannerByName(request, 'NR Secondary');
  await signIn(page, 'operator', `/scanners/${s.id}`);
  await expect(page.getByText('Nobody has control')).toBeVisible();

  await page.getByRole('button', { name: 'Take control' }).click();
  await expect(page.getByText(/You have control/)).toBeVisible();

  const slider = page.getByRole('slider', { name: 'Volume' });
  await expect(slider).toBeEnabled();
  const before = Number(await slider.inputValue());
  const target = before >= 15 ? before - 3 : before + 3;
  await slider.focus();
  for (let i = 0; i < 3; i++) await slider.press(target > before ? 'ArrowRight' : 'ArrowLeft');
  await expect(page.getByText(`Volume: ${target}`)).toBeVisible();

  // The change reaches the simulated scanner (read back through the API).
  await expect
    .poll(async () => (await apiGet<TelemetryResponse>(request, 'admin', `/scanners/${s.id}/telemetry`)).snapshot?.volume, {
      timeout: 15_000,
    })
    .toBe(target);

  // A second user (admin, in its own browser context) sees the lease and
  // cannot take control.
  const other = await browser.newContext();
  try {
    const admin = await other.newPage();
    await signIn(admin, 'admin', `/scanners/${s.id}`);
    await expect(admin.getByText(/Controlled by dev-operator/)).toBeVisible();
    await expect(admin.getByRole('button', { name: 'Take control' })).toBeDisabled();
    await expect(admin.getByRole('button', { name: 'Revoke (admin)' })).toBeVisible();
  } finally {
    await other.close();
  }

  // Put the volume back and release.
  await slider.focus();
  for (let i = 0; i < 3; i++) await slider.press(target > before ? 'ArrowLeft' : 'ArrowRight');
  await expect(page.getByText(`Volume: ${before}`)).toBeVisible();
  await expect
    .poll(async () => (await apiGet<TelemetryResponse>(request, 'admin', `/scanners/${s.id}/telemetry`)).snapshot?.volume, {
      timeout: 15_000,
    })
    .toBe(before);
  await page.getByRole('button', { name: 'Release control' }).click();
  await expect(page.getByText('Nobody has control')).toBeVisible();
});

test('an admin can revoke another user\'s lease', async ({ page, browser, request }) => {
  const s = await scannerByName(request, 'Harbor Main');
  await signIn(page, 'operator', `/scanners/${s.id}`);
  await page.getByRole('button', { name: 'Take control' }).click();
  await expect(page.getByText(/You have control/)).toBeVisible();

  const other = await browser.newContext();
  try {
    const admin = await other.newPage();
    await signIn(admin, 'admin', `/scanners/${s.id}`);
    await admin.getByRole('button', { name: 'Revoke (admin)' }).click();
    await expect(admin.getByText('Nobody has control')).toBeVisible();
  } finally {
    await other.close();
  }
  await expect(page.getByText(/You have control/)).toBeHidden({ timeout: 30_000 });
});
