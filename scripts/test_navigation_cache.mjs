import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {extname, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {after, test} from 'node:test';
import {chromium, webkit} from 'playwright';

const root = fileURLToPath(new URL('../landing/', import.meta.url));
const baseline = '449866bea080baa374f5167178cd18d8e0f90dc8';
const oldHtml = execFileSync('git', ['show', `${baseline}:landing/index.html`]);
const currentHtml = await readFile(resolve(root, 'index.html'));
const oldCss = execFileSync('git', ['show', `${baseline}:landing/presentation.css`]);
const oldJs = execFileSync('git', ['show', `${baseline}:landing/presentation.js`]);
const currentCss = await readFile(resolve(root, 'presentation.css'));
const currentJs = await readFile(resolve(root, 'presentation.js'));
const cssHash = createHash('sha256').update(currentCss).digest('hex').slice(0, 12);
const jsHash = createHash('sha256').update(currentJs).digest('hex').slice(0, 12);
const types = {'.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.webp': 'image/webp', '.woff2': 'font/woff2'};

const requests = [];
const server = createServer(async (request, response) => {
  const url = new URL(request.url, 'http://127.0.0.1');
  requests.push({method: request.method, pathname: url.pathname, search: url.search});
  if (url.pathname === '/events') {
    response.writeHead(204).end();
    return;
  }
  if (request.method !== 'GET') {
    response.writeHead(405).end();
    return;
  }
  if (url.pathname === '/old/' || url.pathname === '/') {
    const body = url.pathname === '/old/' ? oldHtml : currentHtml;
    response.writeHead(200, {
      'content-type': 'text/html',
      'cache-control': 'no-store',
      'content-security-policy': "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; font-src 'self' data:; media-src 'self'"
    }).end(body);
    return;
  }
  if (url.pathname === '/presentation.css') {
    const body = url.searchParams.has('v') ? currentCss : oldCss;
    response.writeHead(200, {'content-type': 'text/css', 'cache-control': 'public, max-age=14400, must-revalidate'}).end(body);
    return;
  }
  if (url.pathname === '/presentation.js') {
    const body = url.searchParams.has('v') ? currentJs : oldJs;
    response.writeHead(200, {'content-type': 'text/javascript', 'cache-control': 'public, max-age=14400, must-revalidate'}).end(body);
    return;
  }
  const file = resolve(root, `.${url.pathname.endsWith('/') ? `${url.pathname}index.html` : url.pathname}`);
  if (!file.startsWith(root)) {
    response.writeHead(403).end();
    return;
  }
  try {
    response.writeHead(200, {'content-type': types[extname(file)] || 'application/octet-stream'}).end(await readFile(file));
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const base = `http://127.0.0.1:${server.address().port}`;

function countAsset(log, pathname, search = null) {
  return log.filter(request => request.pathname === pathname && (search === null || request.search === search)).length;
}

async function run(engine, name) {
  const start = requests.length;
  const browser = await engine.launch({headless: true});
  try {
    const context = await browser.newContext({viewport: {width: 1280, height: 950}, reducedMotion: 'reduce'});
    const page = await context.newPage();
    await page.goto(`${base}/old/`, {waitUntil: 'networkidle'});
    const log = () => requests.slice(start);
    const firstOldCss = countAsset(log(), '/presentation.css', '');
    assert.equal(firstOldCss, 1, `${name}: first old-page load must request historic CSS`);
    await page.goto(`${base}/old/`, {waitUntil: 'networkidle'});
    assert.equal(countAsset(log(), '/presentation.css', ''), firstOldCss, `${name}: cached historic CSS must avoid the second request`);

    await page.goto(`${base}/`, {waitUntil: 'networkidle'});
    assert.equal(countAsset(log(), '/presentation.css', `?v=${cssHash}`), 1, `${name}: current versioned CSS must be fetched`);
    assert.equal(countAsset(log(), '/presentation.js', `?v=${jsHash}`), 1, `${name}: current versioned JS must be fetched`);
    assert.equal(await page.locator('link[href="/presentation.css?v=' + cssHash + '"]').count(), 1);
    assert.equal(await page.locator('script[src="/presentation.js?v=' + jsHash + '"]').count(), 1);

    for (const width of [390, 1280]) {
      await page.setViewportSize({width, height: width === 390 ? 667 : 950});
      await page.goto(`${base}/`, {waitUntil: 'networkidle'});
      if (width <= 800) await page.locator('[data-nav-toggle]').click();
      await page.evaluate(() => document.fonts.ready);
      const metrics = await page.evaluate(() => {
        const summary = document.querySelector('[data-use-cases] summary').getBoundingClientRect();
        return {
          width: document.documentElement.clientWidth,
          scrollWidth: document.documentElement.scrollWidth,
          summaryHeight: summary.height,
        };
      });
      assert.ok(metrics.summaryHeight >= 44, `${name} ${width}px: nav summary is not 44px tall: ${JSON.stringify(metrics)}`);
      assert.ok(metrics.scrollWidth <= metrics.width, `${name} ${width}px: horizontal overflow: ${JSON.stringify(metrics)}`);
    }
  } finally {
    await browser.close();
  }
}

test('Chromium upgrades a returning visitor from historic to versioned presentation assets', {timeout: 90000}, () => run(chromium, 'Chromium'));
test('WebKit upgrades a returning visitor from historic to versioned presentation assets', {timeout: 90000}, () => run(webkit, 'WebKit'));

after(() => new Promise(done => server.close(done)));
