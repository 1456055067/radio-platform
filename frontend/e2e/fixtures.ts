import { expect, type Page } from '@playwright/test';
import type { Role } from './api';

const LABEL: Record<Role, string> = { admin: 'Administrator', operator: 'Operator', viewer: 'Viewer' };

/** Signs in through the devstack role picker (the role lasts for the tab's session). */
export async function signIn(page: Page, role: Role, path = '/') {
  await page.goto(path);
  await expect(page.getByRole('heading', { name: 'Sign in as…' })).toBeVisible();
  await page.getByRole('button', { name: LABEL[role], exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Sign in as…' })).toBeHidden();
}

/** Clicks an entry in the side navigation. */
export async function nav(page: Page, text: string) {
  await page.getByRole('navigation').getByRole('link', { name: text, exact: true }).click();
}
