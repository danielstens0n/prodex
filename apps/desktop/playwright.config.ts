import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: './tests', fullyParallel: true,
  use: { baseURL: 'http://127.0.0.1:1491', viewport: { width: 680, height: 760 }, headless: true },
  webServer: { command: 'npm run dev -- --port 1491', url: 'http://127.0.0.1:1491', reuseExistingServer: false },
});
