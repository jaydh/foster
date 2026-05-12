import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir   : './tests',
  timeout   : 10_000,
  use: {
    baseURL        : 'http://localhost:3000',
    // Keeps the browser open long enough to see what happened on failure.
    actionTimeout  : 5_000,
  },
  // Run against a locally started server.  Adjust command/port as needed.
  webServer: {
    command : 'cargo run -p counter',
    url     : 'http://localhost:3000',
    reuseExistingServer: !process.env.CI,
  },
});
