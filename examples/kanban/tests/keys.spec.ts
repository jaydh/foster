// Hand-written (not generated): keyboard bindings (`fx-on="keydown.Enter->save
// keydown.Escape->cancel"`) and `fx-copy`. gen_tests only writes kanban.spec.ts.
import { test, expect } from '@playwright/test';

const board = (page) => page.locator('[fx-machine="kanban"]');

test.beforeEach(async ({ page }) => {
  // A fresh session per test, so tasks from earlier tests don't leak in.
  await page.goto(`/?session=keys-${Date.now()}-${Math.random().toString(16).slice(2)}`);
  await expect(board(page)).toHaveAttribute('data-fx-state', 'viewing');
});

test('Enter in the new-task input saves', async ({ page }) => {
  await page.locator('[fx-on="click->start_create"]').click();
  await expect(board(page)).toHaveAttribute('data-fx-state', 'creating');
  const input = page.locator('.modal[fx-show="creating"] input');
  await input.fill('Written with Enter');
  await input.press('Enter');
  await expect(board(page)).toHaveAttribute('data-fx-state', 'viewing');
  await expect(page.getByText('Written with Enter')).toBeVisible();
});

test('Escape cancels, other keys do nothing', async ({ page }) => {
  await page.locator('[fx-on="click->start_create"]').click();
  const input = page.locator('.modal[fx-show="creating"] input');
  await input.fill('Never saved');
  await input.press('a');
  await expect(board(page)).toHaveAttribute('data-fx-state', 'creating');
  await input.press('Escape');
  await expect(board(page)).toHaveAttribute('data-fx-state', 'viewing');
  await expect(page.getByText('Never saved')).toHaveCount(0);
});

test('a key binding on an input does not swallow clicks', async ({ page }) => {
  await page.locator('[fx-on="click->start_create"]').click();
  await page.locator('.modal[fx-show="creating"] input').fill('Saved by click');
  await page.locator('.modal[fx-show="creating"] [fx-on="click->save"]').click();
  await expect(page.getByText('Saved by click')).toBeVisible();
});

test('fx-copy copies the item field', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.locator('[fx-on="click->start_create"]').click();
  const input = page.locator('.modal[fx-show="creating"] input');
  await input.fill('Copy me');
  await input.press('Enter');
  // Kanban starts with seeded tasks; copy from the new card.
  await page.locator('[data-fx-item]').filter({ hasText: 'Copy me' }).locator('[fx-copy="item:title"]').click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('Copy me');
});
