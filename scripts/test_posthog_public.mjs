#!/usr/bin/env node

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const ROOT = new URL('..', import.meta.url);
const read = (path) => readFile(new URL(path, ROOT), 'utf8');
const trackingSource = await read('landing/tracking.js');
const waitlistSource = await read('landing/newwaitlist.js');

const eventsSource = await read('functions/events.js');
const eventsModule = { exports: {} };
new Function('module', 'exports', `${eventsSource.replace('export async function onRequestPost', 'async function onRequestPost')}\nmodule.exports = { onRequestPost };`)(eventsModule, eventsModule.exports);
const { onRequestPost } = eventsModule.exports;

const validId = '11111111-1111-4111-8111-111111111111';

function makeD1(rows) {
  return {
    prepare(sql) {
      return {
        bind(...values) {
          return {
            async run() {
              rows.push({ sql, values });
            }
          };
        }
      };
    }
  };
}

async function callHandler(env, init, url = 'https://stokegate.com/events') {
  return onRequestPost({
    request: new Request(url, init),
    env
  });
}

async function testHttpHandler() {
  const rows = [];
  const providerBodies = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (url, options) => {
    assert.equal(url, 'https://eu.i.posthog.com/i/v0/e/');
    providerBodies.push(JSON.parse(options.body));
    return new Response(null, { status: 204 });
  };
  try {
    const accepted = await callHandler({
      POSTHOG_PROJECT_KEY: 'test-project-key',
      STOKE_EVENTS: makeD1(rows)
    }, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          event: '$pageview',
          analytics_id: validId,
          path: 'https://evil.example/?email=alice@example.com',
          source: 'newsletter&utm_campaign=secret',
          target: 'https://evil.example/form?use_case=private',
          email: 'alice@example.com',
          use_case: 'do not send this'
        })
      });
    assert.equal(accepted.status, 204);
    assert.deepEqual(rows[0].values, ['$pageview', '', '', '', rows[0].values[4]]);
    assert.deepEqual(providerBodies, [{
      api_key: 'test-project-key',
      distinct_id: validId,
      event: '$pageview',
      properties: {
        $process_person_profile: false,
        $geoip_disable: true,
        $ip: '0.0.0.0',
        path: '',
        source: '',
        target: ''
      }
    }]);

    const spoofed = await callHandler({
      POSTHOG_PROJECT_KEY: 'test-project-key',
      STOKE_EVENTS: makeD1(rows)
    }, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ event: 'pilot_request_submitted', analytics_id: validId })
      });
    assert.equal(spoofed.status, 400);
    assert.equal(providerBodies.length, 1);

    const wrongOrigin = await callHandler({ POSTHOG_PROJECT_KEY: 'test-project-key', STOKE_EVENTS: makeD1(rows) }, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ event: 'cloud_viewed', analytics_id: validId })
    }, 'https://stoke-preview.pages.dev/events');
    assert.equal(wrongOrigin.status, 403);

    const dnt = await callHandler({ POSTHOG_PROJECT_KEY: 'test-project-key', STOKE_EVENTS: makeD1(rows) }, {
      method: 'POST',
      headers: { 'content-type': 'application/json', dnt: '1' },
      body: JSON.stringify({ event: 'cloud_viewed', analytics_id: validId })
    });
    assert.equal(dnt.status, 204);
    assert.equal(providerBodies.length, 1);

    const tooLarge = await callHandler({ POSTHOG_PROJECT_KEY: 'test-project-key', STOKE_EVENTS: makeD1(rows) }, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ event: 'cloud_viewed', padding: 'x'.repeat(20000) })
    });
    assert.equal(tooLarge.status, 413);
  } finally {
    globalThis.fetch = originalFetch;
  }

  const failingRows = [];
  const originalFailingFetch = globalThis.fetch;
  globalThis.fetch = async () => { throw new Error('provider unavailable'); };
  try {
    const response = await callHandler({ POSTHOG_PROJECT_KEY: 'test-project-key', STOKE_EVENTS: makeD1(failingRows) }, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ event: 'cloud_viewed', analytics_id: validId, path: '/cloud/' })
      });
    assert.equal(response.status, 204);
    assert.equal(failingRows.length, 1);
  } finally {
    globalThis.fetch = originalFailingFetch;
  }
}

class Element {
  constructor({ id = '', dataset = {}, href = '', matches = [] } = {}) {
    this.id = id;
    this.dataset = dataset;
    this.href = href;
    this.matchesSet = new Set(matches);
    this.attributes = {};
    this.listeners = {};
    this.hidden = false;
    this.disabled = false;
    this.textContent = '';
  }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  dispatchEvent(event) { return this.listeners[event.type]?.(event); }
  closest(selector) { return this.matchesSet.has(selector) ? this : null; }
  matches(selector) { return this.matchesSet.has(selector); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  setAttribute(name, value) { this.attributes[name] = String(value); if (name === 'href') this.href = String(value); }
}

function makeBrowser({
  hostname = 'stokegate.com',
  pathname = '/',
  search = '',
  hash = '',
  storage = new Map(),
  storageThrows = false,
  storageWriteThrows = false,
  dnt = '',
  gpc = false,
  injectedConfig = undefined,
  withForm = false,
  withOptOut = false
} = {}) {
  const beacons = [];
  const fetches = [];
  const documentListeners = {};
  const form = withForm ? new Element({ id: 'waitlist-form' }) : null;
  const optOut = withOptOut ? new Element({ dataset: { analyticsOptOut: '' }, matches: ['[data-analytics-opt-out]'] }) : null;
  const optIn = withOptOut ? new Element({ dataset: { analyticsEnable: '' }, matches: ['[data-analytics-enable]'] }) : null;
  const optOutStatus = withOptOut ? new Element() : null;
  const location = {
    hostname,
    protocol: 'https:',
    origin: `https://${hostname}`,
    pathname,
    search,
    hash,
    get href() { return `${this.origin}${this.pathname}${this.search}${this.hash}`; }
  };
  const localStorage = {
    getItem(key) {
      if (storageThrows) throw new Error('storage blocked');
      return storage.has(key) ? storage.get(key) : null;
    },
    setItem(key, value) {
      if (storageThrows || storageWriteThrows) throw new Error('storage write blocked');
      storage.set(key, String(value));
    },
    removeItem(key) {
      if (storageThrows) throw new Error('storage blocked');
      storage.delete(key);
    }
  };
  const document = {
    addEventListener(name, handler) { documentListeners[name] = handler; },
    getElementById(id) { return id === 'waitlist-form' ? form : null; },
    querySelector(selector) {
      if (selector === '[data-analytics-opt-out]') return optOut;
      if (selector === '[data-analytics-enable]') return optIn;
      if (selector === '[data-analytics-status]') return optOutStatus;
      if (selector === '#waitlist-form') return form;
      return null;
    }
  };
  const history = {
    replaceState(_state, _title, url) {
      const parsed = new URL(url, location.href);
      location.pathname = parsed.pathname;
      location.search = parsed.search;
      location.hash = parsed.hash;
    }
  };
  const context = {
    window: null,
    document,
    navigator: { sendBeacon(url, blob) { beacons.push({ url, blob }); return true; }, doNotTrack: dnt, globalPrivacyControl: gpc },
    localStorage,
    location,
    history,
    URL,
    URLSearchParams,
    Blob,
    crypto: { randomUUID: () => validId },
    fetch: async (url, options) => { fetches.push({ url, options }); return new Response(null, { status: 204 }); },
    setTimeout,
    clearTimeout,
    console: { error() {} },
    STOKE_ANALYTICS_CONFIG: injectedConfig
  };
  context.window = context;
  context.globalPrivacyControl = gpc;
  vm.runInNewContext(trackingSource, context, { filename: 'landing/tracking.js' });
  return {
    context,
    location,
    form,
    optOut,
    optIn,
    optOutStatus,
    beacons,
    fetches,
    documentListeners,
    async beaconBodies() { return Promise.all(beacons.map(({ blob }) => blob.text())); }
  };
}

async function testBrowserScript() {
  const cloud = makeBrowser({ pathname: '/cloud/' });
  assert.equal(cloud.beacons.length, 2);
  const cloudBodies = await cloud.beaconBodies();
  assert.ok(cloudBodies.every((body) => !body.includes('email') && !body.includes('href')));
  const join = new Element({ href: 'https://stokegate.com/join/?email=leak#unsafe', matches: ['a[href]'] });
  cloud.documentListeners.click({ target: join });
  assert.equal(join.href, `https://stokegate.com/join/#stoke-analytics=${validId}`);

  const samePageJoin = makeBrowser({ pathname: '/join/' });
  const samePageLink = new Element({ href: '/join/', matches: ['a[href]'] });
  samePageJoin.documentListeners.click({ target: samePageLink });
  assert.equal(samePageLink.href, '/join/');

  const destination = makeBrowser({ pathname: '/join/', search: '?email=not-telemetry', hash: `#stoke-analytics=${validId}`, withForm: true });
  assert.equal(destination.location.hash, '');
  assert.equal(destination.location.search, '?email=not-telemetry');
  assert.equal(destination.context.window.stokeAnalytics.getAnonymousId(), validId);
  assert.equal(destination.beacons.length, 2);
  destination.context.window.stokeAnalytics.track('$pageview');
  assert.equal(destination.beacons.length, 2);
  const destinationEvents = (await destination.beaconBodies()).map((body) => JSON.parse(body).event);
  assert.deepEqual(destinationEvents, ['$pageview', 'pilot_form_opened']);

  assert.equal(makeBrowser({ pathname: '/join/', storageThrows: true }).beacons.length, 0);
  assert.equal(makeBrowser({ pathname: '/join/', dnt: '1' }).beacons.length, 0);
  assert.equal(makeBrowser({ pathname: '/join/', gpc: true }).beacons.length, 0);
  assert.equal(makeBrowser({ hostname: 'preview.pages.dev', pathname: '/join/' }).beacons.length, 0);
  assert.equal(makeBrowser({ hostname: 'preview.pages.dev', pathname: '/join/', injectedConfig: { enabled: true } }).beacons.length, 0);

  const storage = new Map();
  const legal = makeBrowser({ pathname: '/legal/', storage, withOptOut: true });
  await legal.optOut.dispatchEvent({ type: 'click' });
  assert.equal(storage.get('stoke_analytics_opt_out'), '1');
  assert.equal(storage.has('stoke_analytics_storage_probe'), false);
  assert.equal(makeBrowser({ pathname: '/join/', storage }).beacons.length, 0);

  const writeFailureStorage = new Map();
  const writeFailure = makeBrowser({ pathname: '/legal/', storage: writeFailureStorage, withOptOut: true });
  writeFailure.context.localStorage.setItem = () => { throw new Error('quota exceeded'); };
  await writeFailure.optOut.dispatchEvent({ type: 'click' });
  assert.equal(writeFailure.context.window.stokeAnalytics.getAnonymousId(), '');
  writeFailure.context.window.stokeAnalytics.track('$pageview');
  assert.equal(writeFailure.beacons.length, 1);
  const freshWriteFailure = makeBrowser({ pathname: '/join/', storage: writeFailureStorage, storageWriteThrows: true, withForm: true });
  assert.equal(freshWriteFailure.beacons.length, 0);
  assert.equal(freshWriteFailure.context.window.stokeAnalytics.getAnonymousId(), '');
  assert.equal(writeFailureStorage.has('stoke_analytics_opt_out'), false);
  assert.equal(writeFailureStorage.has('stoke_analytics_storage_probe'), false);
  const enabled = makeBrowser({ pathname: '/join/', storage, dnt: '1', withOptOut: true });
  await enabled.optIn.dispatchEvent({ type: 'click' });
  assert.equal(storage.has('stoke_analytics_opt_out'), false);
  assert.equal(enabled.beacons.length, 0);
}

async function testWaitlistScript() {
  const browser = makeBrowser({ pathname: '/join/', withForm: true });
  const form = browser.form;
  const named = {
    notice_version: { value: 'waitlist-v2' },
    namedItem(name) { return this[name]; }
  };
  const controls = {
    '[data-waitlist-fields]': new Element(),
    '[data-waitlist-submit]': new Element(),
    '[data-waitlist-status]': new Element(),
    '[data-waitlist-error]': new Element(),
    '[data-waitlist-note]': new Element(),
    '[data-notice-version]': new Element()
  };
  form.querySelector = (selector) => controls[selector];
  form.elements = named;
  form.reportValidity = () => true;
  form.reset = () => { form.wasReset = true; };
  form.addEventListener = (name, handler) => { form.listeners[name] = handler; };
  browser.context.FormData = class {
    constructor() { this.values = [['email', 'alice@example.com'], ['persona', 'team'], ['use_case', 'private customer detail'], ['consent', 'yes'], ['notice_version', 'waitlist-v2']]; }
    *[Symbol.iterator]() { yield* this.values; }
  };
  browser.context.fetch = async (url, options) => {
    browser.fetches.push({ url, options });
    return url.includes('?format=json')
      ? new Response(JSON.stringify({ open: true, notice_version: 'waitlist-v2' }), { status: 200, headers: { 'content-type': 'application/json' } })
      : new Response(null, { status: 202 });
  };
  vm.runInNewContext(waitlistSource, browser.context, { filename: 'landing/newwaitlist.js' });
  await new Promise((resolve) => setTimeout(resolve, 0));
  await form.listeners.submit({ preventDefault() {} });
  await new Promise((resolve) => setTimeout(resolve, 0));
  const submission = browser.fetches.find(({ url }) => url === '/api/waitlist');
  assert.ok(submission);
  const submissionBody = new URLSearchParams(submission.options.body);
  assert.equal(submissionBody.get('analytics_id'), validId);
  assert.equal(submissionBody.get('email'), 'alice@example.com');
  assert.equal(form.wasReset, true);
  assert.ok((await browser.beaconBodies()).every((body) => !body.includes('alice@example.com') && !body.includes('private customer detail')));
}

await testHttpHandler();
await testBrowserScript();
await testWaitlistScript();
console.log('public PostHog/notification tests passed');
