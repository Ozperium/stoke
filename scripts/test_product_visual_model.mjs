import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {resolve} from 'node:path';
import {
  DURATIONS,
  SCENARIOS,
  phaseAt,
  sampleScenario,
} from '../landing/product-visual-model.mjs';

test('the model exposes the three approved scenarios and finite durations', () => {
  assert.deepEqual(SCENARIOS, ['privacy', 'budget', 'local']);
  for (const scenario of SCENARIOS) {
    assert.ok(Number.isFinite(DURATIONS[scenario]));
    assert.equal(phaseAt(scenario, DURATIONS[scenario]), 'complete');
  }
});

test('privacy has dense, deterministic decision boundaries and redacted response', () => {
  const checks = [
    [0, 'ingress'], [849, 'ingress'], [850, 'inspect'], [1549, 'inspect'],
    [1550, 'mask'], [2349, 'mask'], [2350, 'dispatch'], [3499, 'dispatch'],
    [3500, 'response'], [4299, 'response'], [4300, 'return'], [5599, 'return'],
    [5600, 'complete'],
  ];
  for (const [elapsed, phase] of checks) assert.equal(phaseAt('privacy', elapsed), phase, `privacy ${elapsed}`);

  const masked = sampleScenario('privacy', 2000);
  assert.equal(masked.policy, 'allowed');
  assert.deepEqual(masked.visiblePaths, ['ingress']);
  assert.equal(masked.packet, null);

  const dispatch = sampleScenario('privacy', 2700);
  assert.deepEqual(dispatch.visiblePaths, ['ingress', 'outboundOpenai']);
  assert.equal(dispatch.selectedNode, 'openai');
  assert.equal(dispatch.packet.path, 'outboundOpenai');
  assert.equal(dispatch.packet.response, false);

  const response = sampleScenario('privacy', 3900);
  assert.equal(response.packet.path, 'outboundOpenai');
  assert.equal(response.packet.response, true);
  assert.ok(response.packet.progress < 1, 'response must travel in reverse');

  const complete = sampleScenario('privacy', DURATIONS.privacy);
  assert.equal(complete.packet, null);
  assert.equal(complete.selectedNode, 'openai');
  assert.equal(complete.policy, 'allowed');
});

test('every scenario sends a deterministic ingress packet only until the Stoke boundary', () => {
  const ingressDurations = {privacy: 850, budget: 800, local: 850};
  for (const scenario of SCENARIOS) {
    const end = ingressDurations[scenario];
    for (const elapsed of [0, 1, Math.floor(end / 2), end - 1]) {
      const state = sampleScenario(scenario, elapsed);
      assert.equal(state.phase, 'ingress', `${scenario} ingress phase at ${elapsed}`);
      assert.deepEqual(state.visiblePaths, ['ingress']);
      assert.equal(state.packet.path, 'ingress');
      assert.equal(state.packet.response, false);
      assert.equal(state.packet.progress, elapsed / end);
    }
    const boundary = sampleScenario(scenario, end);
    assert.equal(boundary.phase, 'inspect', `${scenario} boundary phase`);
    assert.equal(boundary.packet, null, `${scenario} packet must stop at Stoke`);
    assert.deepEqual(boundary.visiblePaths, ['ingress']);
  }
});

test('budget never exposes or activates an outgoing path', () => {
  const boundaries = [0, 799, 800, 1499, 1500, 2000, DURATIONS.budget];
  for (const elapsed of boundaries) {
    const state = sampleScenario('budget', elapsed);
    assert.ok(state.visiblePaths.every(path => path === 'ingress'), `visible outgoing path at ${elapsed}`);
    assert.deepEqual(state.activePaths, [], `active outgoing path at ${elapsed}`);
    if (elapsed < 800) {
      assert.equal(state.packet.path, 'ingress', `ingress packet escaped at ${elapsed}`);
    } else {
      assert.equal(state.packet, null, `packet escaped at ${elapsed}`);
    }
  }
  assert.equal(phaseAt('budget', 1499), 'inspect');
  assert.equal(phaseAt('budget', 1500), 'stopped');
  assert.equal(sampleScenario('budget', 1500).policy, 'blocked');
  assert.equal(sampleScenario('budget', DURATIONS.budget).phase, 'complete');
});

test('local fallback fails Laptop before output, then retries one configured Workstation', () => {
  const checks = [
    [0, 'ingress'], [850, 'inspect'], [1500, 'dispatch-laptop'], [2599, 'dispatch-laptop'],
    [2600, 'connection-failed'], [3399, 'connection-failed'], [3400, 'permitted-retry'],
    [4549, 'permitted-retry'], [4550, 'response'], [5349, 'response'], [5350, 'return'],
    [DURATIONS.local, 'complete'],
  ];
  for (const [elapsed, phase] of checks) assert.equal(phaseAt('local', elapsed), phase, `local ${elapsed}`);

  const failed = sampleScenario('local', 2800);
  assert.equal(failed.failedNode, 'laptop');
  assert.equal(failed.selectedNode, null);
  assert.deepEqual(failed.activePaths, []);
  assert.equal(failed.packet, null, 'failure must happen before any output');
  assert.deepEqual(failed.failedPaths, ['outboundLaptop']);

  const retry = sampleScenario('local', 3900);
  assert.equal(retry.failedNode, 'laptop');
  assert.equal(retry.selectedNode, 'workstation');
  assert.deepEqual(retry.activePaths, ['outboundWorkstation']);
  assert.equal(retry.packet.path, 'outboundWorkstation');
  assert.equal(retry.packet.response, false);

  const returned = sampleScenario('local', 5700);
  assert.equal(returned.packet.path, 'ingress');
  assert.equal(returned.packet.response, true);
  assert.equal(returned.failedNode, 'laptop');
  assert.equal(returned.selectedNode, 'workstation');
});

test('elapsed time clamps while invalid inputs remain errors', () => {
  assert.equal(sampleScenario('privacy', -100).elapsed, 0);
  assert.equal(sampleScenario('privacy', 99999).elapsed, DURATIONS.privacy);
  assert.throws(() => sampleScenario('unknown', 0), RangeError);
  assert.throws(() => sampleScenario('privacy', Number.NaN), TypeError);
  assert.throws(() => sampleScenario('privacy', Infinity), TypeError);
});


test('sampling is deterministic and does not mutate prior state', () => {
  const first = sampleScenario('local', 3900);
  const second = sampleScenario('local', 3900);
  assert.deepEqual(first, second);
  assert.throws(() => { first.visiblePaths.push('outboundLaptop'); }, TypeError);
});

test('browser smoke covers visible startup, automatic scene flow, pause, and route geometry', {skip: process.env.STOKE_BROWSER_TESTS !== '1'}, async () => {
  const {chromium} = await import('playwright');
  const landingRoot = fileURLToPath(new URL('../landing/', import.meta.url));
  const server = createServer((request, response) => {
    const requested = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    const relative = requested === '/' ? 'index.html' : requested.replace(/^\/+/, '');
    const file = resolve(landingRoot, relative);
    if (!file.startsWith(landingRoot)) {
      response.writeHead(403).end();
      return;
    }
    try {
      const body = readFileSync(file);
      const type = file.endsWith('.html') ? 'text/html' : file.endsWith('.css') ? 'text/css' : file.endsWith('.svg') ? 'image/svg+xml' : file.endsWith('.mjs') || file.endsWith('.js') ? 'text/javascript' : 'application/octet-stream';
      response.writeHead(200, {'content-type': type}).end(body);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise((resolveServer) => server.listen(0, '127.0.0.1', resolveServer));
  const address = server.address();
  const browser = await chromium.launch({headless: true});
  try {
    const page = await browser.newPage({viewport: {width: 1280, height: 300}});
    await page.goto(`http://127.0.0.1:${address.port}/`, {waitUntil: 'networkidle'});
    const root = page.locator('[data-product-visual]');
    const canvas = page.locator('[data-visual-canvas]');
    const logo = page.locator('.product-visual__logo');
    const logoStyle = (property) => logo.evaluate((image, key) => getComputedStyle(image)[key], property);
    assert.equal(await logo.getAttribute('src'), '/stoke-mark.svg', 'reuse the canonical Stoke mark');
    assert.ok(await logo.evaluate(image => image.complete && image.naturalWidth > 0), 'logo loads');
    assert.equal(await logoStyle('animationName'), 'none', 'idle logo stays still');
    assert.equal(await root.getAttribute('data-phase'), 'complete', 'startup preserves completed output');
    await canvas.scrollIntoViewIfNeeded();
    await page.waitForFunction(() => document.querySelector('[data-product-visual]')?.dataset.playing === 'true');
    assert.notEqual(await root.getAttribute('data-phase'), 'complete', 'visible canvas starts the causal narrative');
    assert.equal(await logoStyle('animationPlayState'), 'running');
    const movingLogo = await logoStyle('transform');
    await page.waitForTimeout(180);
    assert.notEqual(await logoStyle('transform'), movingLogo, 'traffic animates the logo');
    assert.equal(await page.locator('.product-visual__mark').evaluate(el => getComputedStyle(el).transform), 'none', 'wordmark stays readable');
    assert.match(await page.locator('[data-route="ingress"]').getAttribute('d'), /^M/);
    assert.ok(await page.locator('[data-route="ingress"]').evaluate((path) => path.getTotalLength() > 0));

    await page.locator('[data-role="visual-play"]').click();
    const pausedElapsed = await root.getAttribute('data-elapsed');
    assert.equal(await root.getAttribute('data-paused'), 'true');
    assert.equal(await root.getAttribute('data-playing'), 'false');
    await logo.evaluate(async image => {
      await Promise.all(image.getAnimations().map(animation => animation.ready));
      await new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done)));
    });
    assert.equal(await logoStyle('animationPlayState'), 'paused');
    const frozenLogo = await logoStyle('transform');
    await page.waitForTimeout(120);
    assert.equal(await logoStyle('transform'), frozenLogo, 'pause freezes the same logo frame');
    assert.equal(await root.getAttribute('data-elapsed'), pausedElapsed, 'paused animation does not advance');
    await page.locator('[data-role="visual-play"]').click();
    assert.equal(await root.getAttribute('data-playing'), 'true', 'resume restarts the same run');
    await page.waitForTimeout(120);
    assert.notEqual(await logoStyle('transform'), frozenLogo, 'resume animates again');

    await page.waitForFunction(() => document.querySelector('[data-product-visual]')?.dataset.phase === 'complete');
    assert.equal(await logoStyle('animationName'), 'none', 'finished flow stops the logo');

    await page.locator('[data-scenario="budget"]').click();
    assert.equal(await root.getAttribute('data-scenario'), 'budget');
    await page.waitForFunction(() => document.querySelector('[data-product-visual]')?.dataset.phase !== 'complete');
    assert.ok(Number(await root.getAttribute('data-elapsed')) < DURATIONS.budget);
    assert.ok(await page.locator('[data-route="ingress"]').evaluate((path) => path.getTotalLength() > 0), 'scene switch keeps ingress geometry');
    await page.waitForFunction(() => document.querySelector('[data-product-visual]')?.dataset.phase === 'stopped');
    assert.equal(await logoStyle('animationName'), 'none', 'blocked traffic does not keep the logo working');
    await page.locator('[role="tab"][data-scenario="local"]').click();
    await page.waitForFunction(() => document.querySelector('[data-product-visual]')?.dataset.phase === 'connection-failed');
    assert.equal(await logoStyle('animationName'), 'none', 'failed connection stops the logo');
    await page.waitForFunction(() => document.querySelector('[data-product-visual]')?.dataset.phase === 'permitted-retry');
    assert.equal(await logoStyle('animationPlayState'), 'running', 'permitted retry wakes the logo');
    await page.setViewportSize({width: 390, height: 1500});
    await page.emulateMedia({reducedMotion: 'reduce'});
    await page.locator('[role="tab"][data-scenario="privacy"]').click();
    await page.evaluate(() => document.fonts.ready);
    assert.equal(await logoStyle('animationName'), 'none', 'reduced motion disables the effect');
    for (const width of [320, 390, 768, 1024, 1280]) {
      await page.setViewportSize({width, height: 1500});
      const markBox = await page.locator('.product-visual__mark').boundingBox();
      const decisionBox = await page.locator('.product-visual__decision').boundingBox();
      assert.ok(markBox.x + markBox.width <= decisionBox.x, `logo must not overlap the decision at ${width}px`);
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no horizontal overflow at ${width}px`);
    }
    await page.setViewportSize({width: 390, height: 1500});
    const labelHits = await page.evaluate(() => {
      const canvas = document.querySelector('[data-visual-canvas]').getBoundingClientRect();
      const boxes = [];
      for (const label of document.querySelectorAll('.product-visual__actor-label')) {
        for (const node of label.childNodes) {
          if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) {
            const range = document.createRange(); range.selectNodeContents(node);
            boxes.push(...range.getClientRects());
          }
        }
      }
      let hits = 0;
      for (const route of document.querySelectorAll('[data-route]')) {
        if (route.hasAttribute('hidden')) continue;
        const length = route.getTotalLength();
        for (let i = 1; i < 200; i++) {
          const point = route.getPointAtLength(length * i / 200);
          const x = point.x + canvas.left, y = point.y + canvas.top;
          if (boxes.some(b => x > b.left && x < b.right && y > b.top && y < b.bottom)) hits++;
        }
      }
      return hits;
    });
    assert.equal(labelHits, 0, 'mobile routes must not cross actor-label text');
  } finally {
    await browser.close();
    await new Promise((resolveServer) => server.close(resolveServer));
  }
});
