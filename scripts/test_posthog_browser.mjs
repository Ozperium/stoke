#!/usr/bin/env node

// Offline real-browser coverage. The browser keeps a production URL so the
// production gate is exercised; every request is fulfilled locally and only
// /events and the waitlist fixture are observed.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';

const root = path.resolve(fileURLToPath(new URL('..', import.meta.url)));
const moduleName = process.env.STOKE_PLAYWRIGHT_MODULE;
if (!moduleName) {
  throw new Error('Set STOKE_PLAYWRIGHT_MODULE to an installed Playwright module; browser verification has not run.');
}
const playwright = await import(moduleName.includes('/') ? pathToFileURL(moduleName).href : moduleName);
const validId = '22222222-2222-4222-8222-222222222222';
const pageErrors = [];

function fileFor(url) {
  const pathname = new URL(url).pathname;
  if (pathname === '/') return path.join(root, 'landing/index.html');
  if (pathname.endsWith('/')) return path.join(root, 'landing', pathname.slice(1), 'index.html');
  return path.join(root, 'landing', pathname.slice(1));
}

function contentType(file) {
  if (file.endsWith('.html')) return 'text/html';
  if (file.endsWith('.js') || file.endsWith('.mjs')) return 'text/javascript';
  if (file.endsWith('.css')) return 'text/css';
  if (file.endsWith('.svg')) return 'image/svg+xml';
  return 'application/octet-stream';
}

async function installOfflineRoutes(context, events, submissions) {
  context.on('page', (page) => page.on('pageerror', (error) => pageErrors.push(error.message)));
  // WebKit can expose native beacon requests without postData. Observe the
  // actual Blob/string passed to native transport, without replacing transport
  // results; all network responses below remain explicitly offline fixtures.
  await context.exposeBinding('__stokeFixturePayload', (_source, body) => events.push(JSON.parse(body)));
  await context.addInitScript(() => {
    const isAnalytics = (input) => {
      const url = new URL(typeof input === 'string' ? input : input.url, location.href);
      return url.origin === location.origin && url.pathname === '/events';
    };
    const observe = (body) => {
      if (body instanceof Blob) body.text().then((text) => window.__stokeFixturePayload(text));
      else window.__stokeFixturePayload(String(body));
    };
    const nativeBeacon = navigator.sendBeacon?.bind(navigator);
    if (nativeBeacon) {
      navigator.sendBeacon = (url, body) => {
        const queued = nativeBeacon(url, body);
        if (queued && isAnalytics(url)) observe(body);
        return queued;
      };
    }
    const nativeFetch = window.fetch.bind(window);
    window.fetch = (input, options) => {
      const response = nativeFetch(input, options);
      if (isAnalytics(input) && options?.body) observe(options.body);
      return response;
    };
  });
  await context.route('**/*', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.pathname === '/events' && request.method() === 'POST') {
      await route.fulfill({ status: 204, headers: { 'cache-control': 'no-store' }, body: '' });
      return;
    }
    if (url.pathname === '/api/waitlist') {
      if (request.method() === 'GET') {
        await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ open: true, notice_version: 'waitlist-v2' }) });
      } else {
        submissions.push(new URLSearchParams(request.postData() || ''));
        await route.fulfill({ status: 202, contentType: 'application/json', body: '{}' });
      }
      return;
    }
    if (request.method() !== 'GET' && request.method() !== 'HEAD') {
      await route.fulfill({ status: 204, body: '' });
      return;
    }
    try {
      const file = fileFor(request.url());
      const body = await readFile(file);
      await route.fulfill({ status: 200, contentType: contentType(file), body });
    } catch {
      await route.fulfill({ status: 404, body: '' });
    }
  });
}

async function checkBrowser(browserType) {
  pageErrors.length = 0;
  const browser = await browserType.launch({ headless: true });
  try {
    const context = await browser.newContext();
    const events = [];
    const submissions = [];
    await installOfflineRoutes(context, events, submissions);
    const cloud = await context.newPage();
    await cloud.goto('https://stokegate.com/cloud/');
    await cloud.waitForLoadState('networkidle');
    assert.equal(events.filter((event) => event.event === '$pageview').length, 1, JSON.stringify({ engine: browserType.name(), observed: events, pageErrors, runtime: await cloud.evaluate(() => ({ analyticsLoaded: !!window.stokeAnalytics, idAvailable: !!window.stokeAnalytics?.getAnonymousId(), dnt: navigator.doNotTrack, gpc: navigator.globalPrivacyControl, protocol: location.protocol, hostname: location.hostname, secure: isSecureContext })) }));
    assert.equal(events.filter((event) => event.event === 'cloud_viewed').length, 1);
    await cloud.locator('a[href="/join/"]').first().click();
    await cloud.waitForURL('https://stokegate.com/join/');
    await cloud.waitForLoadState('networkidle');
    assert.equal(new URL(cloud.url()).hash, '');
    assert.equal(events.filter((event) => event.event === '$pageview').length, 2);
    assert.equal(events.filter((event) => event.event === 'pilot_form_opened').length, 1);
    const correlatedIds = events.filter((event) => event.event === '$pageview' || event.event === 'pilot_form_opened').map((event) => event.analytics_id);
    assert.equal(correlatedIds.length, 3);
    assert.ok(correlatedIds.every((id) => id === correlatedIds[0] && /^[0-9a-f-]{36}$/.test(id)));
    await cloud.locator('a.nav-cta[href="/join/"]').click();
    await cloud.waitForLoadState('networkidle');
    assert.equal(new URL(cloud.url()).hash, '');
    assert.equal(events.filter((event) => event.event === '$pageview').length, 3);
    assert.equal(events.filter((event) => event.event === 'pilot_form_opened').length, 2);
    const samePageIds = events.filter((event) => event.event === '$pageview' || event.event === 'pilot_form_opened').map((event) => event.analytics_id).slice(3);
    assert.equal(samePageIds.length, 2);
    assert.ok(samePageIds.every((id) => id === samePageIds[0] && /^[0-9a-f-]{36}$/.test(id)));
    await cloud.locator('input[type=email]').fill('enabled@example.com');
    await cloud.locator('input[name=persona][value=solo]').check();
    await cloud.locator('input[name=consent]').check();
    await cloud.locator('[data-waitlist-submit]').click();
    await cloud.waitForFunction((text) => document.querySelector('[data-waitlist-status]')?.textContent === text, "Request received. We'll review it.");
    await cloud.waitForLoadState('networkidle');
    assert.equal(submissions[0].get('analytics_id'), samePageIds[0]);
    assert.equal(await cloud.locator('[data-waitlist-status]').textContent(), "Request received. We'll review it.");
    assert.ok(events.every((event) => Object.keys(event).sort().join(',') === 'analytics_id,event,path,source,target'));
    assert.ok(events.every((event) => !JSON.stringify(event).match(/email|referrer|query|hash|href|private/)));
    assert.deepEqual(await context.cookies(), []);
    assert.equal(await cloud.evaluate((id) => JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } }).includes(id), samePageIds[0]), false);

    const legal = await context.newPage();
    await legal.goto('https://stokegate.com/legal/');
    await legal.locator('[data-analytics-opt-out]').click();
    await legal.reload();
    await legal.goto('https://stokegate.com/join/');
    await legal.locator('input[type=email]').fill('offline@example.com');
    await legal.locator('input[name=persona][value=solo]').check();
    await legal.locator('input[name=consent]').check();
    await legal.locator('[data-waitlist-submit]').click();
    await legal.waitForFunction((text) => document.querySelector('[data-waitlist-status]')?.textContent === text, "Request received. We'll review it.");
    await legal.waitForLoadState('networkidle');
    assert.equal(submissions.at(-1).has('analytics_id'), false);
    assert.equal(events.filter((event) => event.event === 'pilot_request_submitted').length, 0);
    await context.close();

    for (const signal of ['DNT', 'GPC']) {
      const privacyContext = await browser.newContext({ extraHTTPHeaders: signal === 'DNT' ? { DNT: '1' } : { 'Sec-GPC': '1' } });
      // HTTP header overrides do not set navigator preferences in Playwright.
      await privacyContext.addInitScript((kind) => {
        if (kind === 'DNT') Object.defineProperty(navigator, 'doNotTrack', { get: () => '1' });
        else Object.defineProperty(navigator, 'globalPrivacyControl', { get: () => true });
      }, signal);
      const privacyEvents = [];
      await installOfflineRoutes(privacyContext, privacyEvents, []);
      const privacyPage = await privacyContext.newPage();
      await privacyPage.goto('https://stokegate.com/join/');
      await privacyPage.waitForLoadState('networkidle');
      assert.equal(privacyEvents.length, 0, `${signal} must independently disable telemetry`);
      await privacyContext.close();
    }

    const previewContext = await browser.newContext();
    const previewEvents = [];
    await installOfflineRoutes(previewContext, previewEvents, []);
    const preview = await previewContext.newPage();
    await preview.goto('https://preview.pages.dev/join/');
    await preview.waitForLoadState('networkidle');
    assert.equal(previewEvents.length, 0);
    await previewContext.close();

    const blockedStorageContext = await browser.newContext();
    await blockedStorageContext.addInitScript(() => {
      Object.defineProperty(window, 'localStorage', { get() { throw new Error('blocked'); } });
    });
    const blockedEvents = [];
    await installOfflineRoutes(blockedStorageContext, blockedEvents, []);
    const blocked = await blockedStorageContext.newPage();
    await blocked.goto('https://stokegate.com/join/');
    await blocked.waitForLoadState('networkidle');
    assert.equal(blockedEvents.length, 0);
    await blockedStorageContext.close();

    const writeFailureContext = await browser.newContext();
    const writeFailureEvents = [];
    const writeFailureSubmissions = [];
    await writeFailureContext.addInitScript(() => {
      if (window.name === 'stoke-write-failure') {
        Storage.prototype.setItem = () => { throw new DOMException('Fixture quota exceeded', 'QuotaExceededError'); };
      }
    });
    await installOfflineRoutes(writeFailureContext, writeFailureEvents, writeFailureSubmissions);
    const writeFailurePage = await writeFailureContext.newPage();
    await writeFailurePage.goto('https://stokegate.com/legal/');
    await writeFailurePage.waitForLoadState('networkidle');
    const beforeOptOut = writeFailureEvents.length;
    await writeFailurePage.evaluate(() => {
      window.name = 'stoke-write-failure';
      Storage.prototype.setItem = () => { throw new DOMException('Fixture quota exceeded', 'QuotaExceededError'); };
    });
    await writeFailurePage.locator('[data-analytics-opt-out]').click();
    assert.equal(await writeFailurePage.evaluate(() => window.stokeAnalytics.getAnonymousId()), '');
    await writeFailurePage.evaluate(() => window.stokeAnalytics.track('github_clicked', 'github'));
    await writeFailurePage.waitForLoadState('networkidle');
    assert.equal(writeFailureEvents.length, beforeOptOut);
    await writeFailurePage.reload();
    await writeFailurePage.waitForLoadState('networkidle');
    await writeFailurePage.goto('https://stokegate.com/join/');
    await writeFailurePage.waitForLoadState('networkidle');
    await writeFailurePage.locator('input[type=email]').fill('quota@example.com');
    await writeFailurePage.locator('input[name=persona][value=solo]').check();
    await writeFailurePage.locator('input[name=consent]').check();
    await writeFailurePage.locator('[data-waitlist-submit]').click();
    await writeFailurePage.waitForFunction((text) => document.querySelector('[data-waitlist-status]')?.textContent === text, "Request received. We'll review it.");
    await writeFailurePage.waitForLoadState('networkidle');
    assert.equal(writeFailureSubmissions.at(-1).has('analytics_id'), false);
    assert.equal(await writeFailurePage.locator('[data-waitlist-status]').textContent(), "Request received. We'll review it.");
    assert.equal(writeFailureEvents.length, beforeOptOut);
    await writeFailureContext.close();
    assert.deepEqual(pageErrors, [], 'Actual site scripts must not raise page errors');
  } finally {
    await browser.close();
  }
}

const results = [];
for (const name of ['chromium', 'webkit']) {
  assert.equal(typeof playwright[name]?.launch, 'function', `${name} is required for this gate`);
  await checkBrowser(playwright[name]);
  results.push(name);
}
assert.deepEqual(results, ['chromium', 'webkit']);
console.log(`real browser PostHog checks passed: ${results.join(', ')}`);
