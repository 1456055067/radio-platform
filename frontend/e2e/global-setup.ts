import { request, type FullConfig } from '@playwright/test';
import { scanners } from './api';

/**
 * The devstack listens before it has enrolled everything: wait until all
 * four scanners exist and the three live ones report healthy.
 */
export default async function globalSetup(config: FullConfig) {
  const baseURL = config.projects[0]?.use.baseURL;
  const ctx = await request.newContext({ baseURL });
  const deadline = Date.now() + 120_000;
  let last = '';
  try {
    for (;;) {
      try {
        const all = await scanners(ctx);
        const healthy = all.filter((s) => s.health?.state === 'healthy' && s.agent_connected).length;
        last = `${all.length} scanners, ${healthy} healthy`;
        if (all.length === 4 && healthy === 3) return;
      } catch (e) {
        last = String(e);
      }
      if (Date.now() > deadline) throw new Error(`radio-devstack not ready: ${last}`);
      await new Promise((r) => setTimeout(r, 500));
    }
  } finally {
    await ctx.dispose();
  }
}
