import { chromium } from 'playwright-core';
const browser = await chromium.connect('ws://localhost:34009');
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
const page = await context.newPage();
const logs = [];
page.on('console', (msg) => logs.push(`[${msg.type()}] ${msg.text()}`));
page.on('pageerror', (err) => logs.push(`[pageerror] ${err.message}`));
await page.goto('http://127.0.0.1:19000/login', { waitUntil: 'networkidle' });
await page.getByLabel(/логин|login/i).fill('admin');
await page.getByLabel(/пароль|password/i).fill('Admin123!Secure');
await Promise.all([
  page.waitForURL((url) => !url.pathname.includes('/login'), { timeout: 15000 }),
  page.getByRole('button', { name: /войти|sign in|log in/i }).click(),
]);
for (const path of ['/', '/documents', '/admin', '/graph']) {
  await page.goto(`http://127.0.0.1:19000${path}`, { waitUntil: 'networkidle' });
  const name = path === '/' ? 'home' : path.slice(1);
  await page.screenshot({ path: `/tmp/rustrag-${name}.png`, fullPage: true });
}
console.log(JSON.stringify({ url: page.url(), logs, screenshots: ['/tmp/rustrag-home.png','/tmp/rustrag-documents.png','/tmp/rustrag-admin.png','/tmp/rustrag-graph.png'] }, null, 2));
await page.close();
await context.close();
await browser.close();
