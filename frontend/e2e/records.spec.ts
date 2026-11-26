import { expect, test } from '@playwright/test';
import { scannerByName, transmissions } from './api';
import { nav, signIn } from './fixtures';

// The devstack records and (stub-)transcribes only NR Primary. Simulated
// traffic is random, so wait for a transcript through the API first.
test('a recorded transmission is transcribed, searchable and playable', async ({ page, request }) => {
  test.setTimeout(180_000);
  const s = await scannerByName(request, 'NR Primary');
  await expect
    .poll(async () => (await transmissions(request, `scanner_id=${s.id}&limit=50`)).some((t) => !!t.transcript), {
      timeout: 150_000,
      intervals: [2_000],
    })
    .toBe(true);

  await signIn(page, 'admin');
  await nav(page, 'Transmissions');
  await expect(page.getByRole('heading', { level: 1, name: 'Transmissions' })).toBeVisible();
  await page.getByLabel('Transcript contains').fill('simulated');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  const row = page.getByRole('row').filter({ hasText: 'Simulated transcript' }).first();
  await expect(row).toBeVisible();
  await expect(row).toContainText('NR Primary');

  await row.getByRole('button', { name: 'Details' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText(/Simulated transcript: \d+ hertz tone/)).toBeVisible();
  await expect(dialog.locator('audio')).toHaveCount(1);
});

test('analytics shows activity from the simulated traffic', async ({ page }) => {
  await signIn(page, 'operator');
  await nav(page, 'Analytics');
  await expect(page.getByRole('heading', { level: 1, name: 'Analytics' })).toBeVisible();
  await expect(page.getByRole('table')).toBeVisible();
  await expect(page.getByText('Busiest', { exact: true })).toBeVisible();
});

test('audit chain verifies as intact', async ({ page }) => {
  await signIn(page, 'admin');
  await nav(page, 'Audit log');
  await page.getByRole('button', { name: 'Verify chain' }).click();
  await expect(page.getByText(/Intact: \d+ records verified/)).toBeVisible();
});
