import { expect, test } from '@playwright/test';
import { scannerByName } from './api';
import { signIn } from './fixtures';

test('live audio plays over WebRTC and stops', async ({ page, request }) => {
  const s = await scannerByName(request, 'Harbor Main');
  await signIn(page, 'admin', `/scanners/${s.id}`);
  await page.getByRole('button', { name: 'Listen live' }).click();
  await expect(page.getByText('Playing', { exact: true })).toBeVisible({ timeout: 30_000 });

  // Audio is actually flowing: a live remote track, and playback time
  // advances (it stalls if no RTP arrives).
  const audio = page.locator('audio[aria-label="Live scanner audio"]');
  const state = () =>
    audio.evaluate((el: HTMLAudioElement) => {
      const s = el.srcObject;
      const live = s instanceof MediaStream ? s.getAudioTracks().filter((t) => t.readyState === 'live').length : 0;
      return { live, t: el.currentTime };
    });
  const first = await state();
  expect(first.live).toBe(1);
  await expect.poll(async () => (await state()).t, { timeout: 15_000 }).toBeGreaterThan(first.t + 1);

  await page.getByRole('button', { name: 'Stop' }).click();
  await expect(page.getByRole('button', { name: 'Listen live' })).toBeVisible();
});
