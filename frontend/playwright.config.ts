import { defineConfig, devices } from '@playwright/test';

// Browser end-to-end tests against `radio-devstack` (simulated scanners,
// agents and gateway in one process). Set E2E_BASE_URL to reuse a devstack
// you already started; otherwise Playwright builds and starts one.
const port = Number(process.env.E2E_PORT ?? 8090);
const external = process.env.E2E_BASE_URL;
const baseURL = external ?? `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: './e2e',
  // One shared backend with leases and live state: run tests one at a time.
  workers: 1,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  globalSetup: './e2e/global-setup.ts',
  use: {
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        launchOptions: { args: ['--autoplay-policy=no-user-gesture-required'] },
      },
    },
  ],
  webServer: external
    ? undefined
    : {
        // Run from the repository root; the UI must be built first
        // (`npm run build`), and CI builds the binary beforehand.
        command: `cargo run --quiet -p radio-devstack -- --bind 127.0.0.1:${port} --ui-dir frontend/dist`,
        cwd: '..',
        url: `${baseURL}/ui-config.json`,
        timeout: 600_000,
        reuseExistingServer: false,
        stdout: 'ignore',
        stderr: 'pipe',
        gracefulShutdown: { signal: 'SIGINT', timeout: 5_000 },
      },
});
