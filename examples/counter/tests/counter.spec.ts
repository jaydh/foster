// Smoke test: the client loads, events round-trip, state and context render.
import { test, expect } from '@playwright/test';

test('count, break, recover', async ({ page }) => {
  await page.goto('/');
  const root = page.locator('[fx-machine="counter"]');
  await expect(root).toHaveAttribute('data-fx-state', 'idle');
  const plus = page.getByRole('button', { name: '+' });
  await plus.click();
  await plus.click();
  await expect(page.locator('.count-display')).toHaveText('2');
  await page.getByRole('button', { name: '−' }).click();
  await expect(page.locator('.count-display')).toHaveText('1');
  await page.getByRole('button', { name: 'break' }).click();
  await expect(root).toHaveAttribute('data-fx-state', 'error');
  await expect(page.locator('.error-count')).toHaveText('1');
  await page.getByRole('button', { name: 'recover' }).click();
  await expect(page.locator('.count-display')).toHaveText('1');
  // State survives a reload: it's the server's (this tab's session).
  await page.reload();
  await expect(page.locator('.count-display')).toHaveText('1');
});
