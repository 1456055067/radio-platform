/// <reference types="vitest/config" />
import { defineConfig } from 'vite';
import { configDefaults } from 'vitest/config';
import react from '@vitejs/plugin-react';

// The dev server proxies the API, the live stream and the runtime config to
// a local gateway or `radio-devstack` (default 127.0.0.1:8080).
const backend = process.env.RADIO_BACKEND ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [react()],
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': { target: backend, ws: true, changeOrigin: false },
      '/dev': { target: backend },
      '/ui-config.json': { target: backend },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: false,
    chunkSizeWarningLimit: 2500,
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
    css: false,
    // Playwright browser tests live in e2e/ and run with `npm run e2e`.
    exclude: [...configDefaults.exclude, 'e2e/**'],
  },
});
