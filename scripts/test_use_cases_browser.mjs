import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile, mkdir} from 'node:fs/promises';
import {resolve, join, extname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {test, after} from 'node:test';
import {chromium, webkit} from 'playwright';

const root = fileURLToPath(new URL('../landing/', import.meta.url));
const proof = process.env.STOKE_PROOF_DIR;
if (proof) await mkdir(proof, {recursive: true});
const cases = [
  ['local-models', 'Give your idle computer a job'],
  ['private-data', 'Keep client documents off the cloud'],
  ['agent-spend', 'Stop funding an agent’s retry loop'],
  ['private-model-access', 'Connect apps without copying provider keys'],
  ['node-failover', 'A model host went offline. Now what?'],
  ['secret-redaction', 'The error log came with an API key'],
];
const editorialCases = ['local-models', 'secret-redaction'];
const types = {'.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2'};
const server = createServer(async (request, response) => {
  const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
  const file = resolve(root, '.' + (pathname.endsWith('/') ? pathname + 'index.html' : pathname));
  if (!file.startsWith(root)) return response.writeHead(403).end();
  try {
    const body = await readFile(file);
    response.writeHead(200, {'content-type': types[extname(file)] || 'application/octet-stream'}).end(body);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const base = `http://127.0.0.1:${server.address().port}`;
after(() => new Promise(done => server.close(done)));

async function openCases(page, width) {
  if (width <= 800) {
    await page.locator('[data-nav-toggle]').click();
    assert.equal(await page.locator('[data-nav-toggle]').getAttribute('aria-expanded'), 'true');
  }
  await page.locator('[data-use-cases] summary').click();
  assert.equal(await page.locator('[data-use-cases]').getAttribute('open'), '');
}

async function geometry(page) {
  await page.evaluate(() => document.fonts.ready);
  const result = await page.evaluate(() => {
    const width = document.documentElement.clientWidth;
    const nav = document.querySelector('.nav-inner').getBoundingClientRect();
    const brand = document.querySelector('.nav-inner .brand').getBoundingClientRect();
    const summary = document.querySelector('[data-use-cases] summary').getBoundingClientRect();
    const panel = document.querySelector('.use-cases-menu').getBoundingClientRect();
    return {overflow: document.documentElement.scrollWidth > width, width,
      summaryHeight: summary.height, panelLeft: panel.left, panelRight: panel.right,
      columns: getComputedStyle(document.querySelector('.use-cases-menu')).gridTemplateColumns.split(' ').length,
      brandRight: brand.right, navLeft: nav.left, summaryLeft: summary.left,
      touchSizes: [...document.querySelectorAll('.use-cases-menu a')].map(a => a.getBoundingClientRect().height)};
  });
  assert.equal(result.overflow, false, JSON.stringify(result));
  assert.ok(result.panelLeft >= 0 && result.panelRight <= result.width, JSON.stringify(result));
  assert.ok(result.summaryHeight >= 44, JSON.stringify(result));
  assert.ok(result.touchSizes.every(h => h >= 44), JSON.stringify(result));
  if (result.width > 800) {
    assert.ok(result.summaryLeft > result.brandRight, JSON.stringify(result));
    assert.equal(result.columns, 2, 'Six entries must use the compact two-column desktop panel');
  }
}

for (const [name, engine] of [['chromium', chromium], ['webkit', webkit]]) {
  for (const width of [390, 768, 801, 1024, 1280]) {
    test(`${name}: persona links and detail navigation at ${width}px`, {timeout: 90000}, async () => {
      const browser = await engine.launch({headless: true});
      try {
        const page = await browser.newPage({viewport: {width, height: width <= 800 ? 667 : 950}, reducedMotion: 'reduce'});
        const errors = [];
        page.on('pageerror', error => errors.push(error.message));
        assert.equal((await page.goto(base + '/')).status(), 200);
        for (const [index, [slug, title]] of cases.entries()) {
          await openCases(page, width);
          await geometry(page);
          const link = page.locator(`.use-cases-menu a[href="/use-cases/${slug}/"]`);
          assert.equal(await link.isVisible(), true);
          assert.ok((await link.innerText()).includes('Explore use case'));
          if (proof && name === 'chromium' && index === 0 && [390, 1280].includes(width)) {
            await page.screenshot({path: join(proof, `${width}-use-cases-menu.png`)});
          }
          await link.click();
          await page.waitForURL(base + `/use-cases/${slug}/`);
          assert.equal(await page.locator('h1').innerText(), title);
          const illustration = page.locator('[data-use-case-image]');
          await illustration.scrollIntoViewIfNeeded();
          await illustration.evaluate(img => img.decode());
          assert.deepEqual(await illustration.evaluate(img => [img.naturalWidth, img.naturalHeight]), [1152, 720]);
          assert.equal(await page.locator('.use-cases-menu [aria-current="page"]').count(), 1);
          assert.ok(await page.locator('.document-content h2').count() >= 3);
          assert.equal(await page.locator('.use-case-figure figcaption').count(), 0);
          assert.ok(!(await page.locator('body').innerText()).includes('not a product screenshot'));
          assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth), false);
          if (proof && name === 'chromium' && slug === 'private-data' && [390, 1280].includes(width)) {
            await page.screenshot({path: join(proof, `${width}-private-data.png`), fullPage: true});
          }
        }
        await openCases(page, width);
        await page.locator('.use-cases-menu__all').click();
        await page.waitForURL(base + '/use-cases/');
        assert.equal(await page.locator('.gcard').count(), 6);
        if (proof && name === 'chromium' && width === 1280) {
          await page.screenshot({path: join(proof, '1280-use-cases-index.png'), fullPage: true});
        }
        if (width > 800) {
          const summary = page.locator('[data-use-cases] summary');
          await summary.focus();
          await page.keyboard.press('Space');
          assert.equal(await page.locator('[data-use-cases]').getAttribute('open'), '');
          await page.keyboard.press('Tab');
          await page.keyboard.press('Escape');
          assert.equal(await page.locator('[data-use-cases]').getAttribute('open'), null);
          assert.equal(await summary.evaluate(el => el === document.activeElement), true);
          await summary.click();
          // Click actual blank page space outside the popup; the popup can cover the heading.
          const headerBottom = await page.locator('.site-nav').evaluate(el => el.getBoundingClientRect().bottom);
          await page.mouse.click(5, headerBottom + 10);
          assert.equal(await page.locator('[data-use-cases]').getAttribute('open'), null);
        } else {
          await openCases(page, width);
          await page.keyboard.press('Escape');
          assert.equal(await page.locator('[data-use-cases]').getAttribute('open'), null);
          assert.equal(await page.locator('[data-nav-toggle]').getAttribute('aria-expanded'), 'true');
          await page.keyboard.press('Escape');
          assert.equal(await page.locator('[data-nav-toggle]').getAttribute('aria-expanded'), 'false');
          assert.equal(await page.locator('[data-nav-toggle]').evaluate(el => el === document.activeElement), true);
        }
        assert.deepEqual(errors, []);
      } finally {
        await browser.close();
      }
    });
  }
  test(`${name}: native disclosure and detail links work without JavaScript`, {timeout: 90000}, async () => {
    const browser = await engine.launch({headless: true});
    try {
      const context = await browser.newContext({javaScriptEnabled: false});
      const page = await context.newPage();
      for (const width of [390, 1280]) {
        await page.setViewportSize({width, height: 950});
        await page.goto(base + '/');
        await page.locator('[data-use-cases] summary').click();
        assert.equal(await page.locator('.use-cases-menu__item').count(), 6);
        await page.locator('.use-cases-menu a[href="/use-cases/private-data/"]').click();
        await page.waitForURL(base + '/use-cases/private-data/');
        assert.equal(await page.locator('h1').innerText(), 'Keep client documents off the cloud');
        assert.equal(await page.locator('#nav-menu').isVisible(), true);
      }
    } finally {
      await browser.close();
    }
  });

  for (const width of [390, 768, 1024, 1280]) {
    test(`${name}: editorial pilot layout at ${width}px`, {timeout: 90000}, async () => {
      const browser = await engine.launch({headless: true});
      try {
        const page = await browser.newPage({viewport: {width, height: 950}, reducedMotion: 'reduce'});
        for (const slug of editorialCases) {
          await page.goto(base + `/use-cases/${slug}/`);
          const result = await page.evaluate(async () => {
            const image = document.querySelector('[data-use-case-image]');
            await image.decode();
            const copy = document.querySelector('.use-case-copy');
            const figure = document.querySelector('.use-case-figure');
            const heading = document.querySelector('h1');
            const grid = document.querySelector('.use-case-editorial-grid');
            const imageStyle = getComputedStyle(image);
            const figureStyle = getComputedStyle(figure);
            return {
              imageDecoded: image.complete && image.naturalWidth === 1152 && image.naturalHeight === 720,
              overflow: document.documentElement.scrollWidth > document.documentElement.clientWidth,
              border: figureStyle.borderTopWidth,
              imageBorder: imageStyle.borderTopWidth,
              copy: copy.getBoundingClientRect().toJSON(),
              figure: figure.getBoundingClientRect().toJSON(),
              grid: grid.getBoundingClientRect().toJSON(),
              heading: heading.getBoundingClientRect().toJSON(),
              main: document.querySelector('main').getBoundingClientRect().toJSON(),
            };
          });
          assert.equal(result.imageDecoded, true, slug);
          assert.equal(result.overflow, false, `${slug}: ${JSON.stringify(result)}`);
          assert.equal(result.border, '0px', `${slug}: figure frame remains`);
          assert.equal(result.imageBorder, '0px', `${slug}: image frame remains`);
          assert.ok(result.main.top < 950 && result.heading.bottom <= 950, `${slug}: body is not visible in viewport: ${JSON.stringify(result)}`);
          if (width <= 800) {
            assert.ok(result.figure.top >= result.copy.bottom - 1, `${slug}: image is not stacked below copy: ${JSON.stringify(result)}`);
            assert.equal(result.grid.width, result.copy.width, `${slug}: stacked grid width drift`);
          } else {
            assert.ok(result.figure.left > result.copy.right, `${slug}: image is not beside copy: ${JSON.stringify(result)}`);
            assert.ok(result.figure.top < result.copy.bottom && result.figure.bottom > result.copy.top, `${slug}: columns do not share the editorial row: ${JSON.stringify(result)}`);
          }
        }
      } finally {
        await browser.close();
      }
    });
  }
}
