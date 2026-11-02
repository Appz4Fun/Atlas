// node shot.cjs BASE KEY OUT [PATH] [--dark] [--width N]
// Signs in to the Atlas web UI with KEY, opens PATH (default /), waits for
// htmx to settle and saves OUT (".png" added if missing). Prints the page's
// <title> and the first result names so an agent can check without looking.
// CommonJS on purpose: ESM `import` ignores NODE_PATH, where the container's playwright lives
// the container's browsers live in /opt/pw-browsers; a shell without
// PLAYWRIGHT_BROWSERS_PATH (env -i, cron) looks in ~/.cache and fails
const fs = require('fs');
if (!process.env.PLAYWRIGHT_BROWSERS_PATH && fs.existsSync('/opt/pw-browsers')) {
  process.env.PLAYWRIGHT_BROWSERS_PATH = '/opt/pw-browsers';
}
const { chromium } = require('playwright');

(async () => {

const [base, key, outArg, ...rest] = process.argv.slice(2);
// PATH is optional, so `shot OUT --dark` must not take --dark for it
const path = rest[0] && !rest[0].startsWith('--') ? rest.shift() : '/';
if (!outArg) { console.error('usage: shot.cjs BASE KEY OUT [PATH] [--dark] [--width N]'); process.exit(1); }
const out = outArg.endsWith('.png') ? outArg : `${outArg}.png`;
const dark = rest.includes('--dark');
const wi = rest.indexOf('--width');
const width = wi >= 0 ? Number(rest[wi + 1]) : 1400;

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width, height: 900 }, colorScheme: dark ? 'dark' : 'light' });
await page.goto(base + '/');
if (await page.locator('#key').count()) {
  await page.fill('#key', key);
  await Promise.all([page.waitForNavigation(), page.press('#key', 'Enter')]);
}
await page.goto(base + path);
await page.waitForLoadState('networkidle');
await page.screenshot({ path: out, fullPage: true });
const names = await page.locator('td.name > a').allTextContents();
console.log(JSON.stringify({ out, title: await page.title(), url: page.url(), results: names.slice(0, 10) }));
await browser.close();
})().catch((e) => { console.error(e); process.exit(1); });
