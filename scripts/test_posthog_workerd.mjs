#!/usr/bin/env node

import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(fileURLToPath(new URL('..', import.meta.url)));
const MINIFLARE_PACKAGE = process.env.STOKE_MINIFLARE_PACKAGE;
assert.ok(MINIFLARE_PACKAGE && path.isAbsolute(MINIFLARE_PACKAGE), 'Set STOKE_MINIFLARE_PACKAGE to an absolute package.json path with Miniflare installed; workerd verification has not run.');
const require = createRequire(MINIFLARE_PACKAGE);
const { Miniflare, convertV4MiniflareOptions } = require('miniflare');

const validId = '33333333-3333-4333-8333-333333333333';
const posthogUrl = 'https://eu.i.posthog.com/i/v0/e/';
const foreignRedirect = 'https://foreign.example/redirected';
const workerModule = `import { onRequestPost } from '../functions/events.js';

export default {
  async fetch(request, env) {
    if (new URL(request.url).pathname === '/events') {
      return onRequestPost({ request, env });
    }
    return new Response(null, { status: 404 });
  },
};`;

async function exerciseRuntime() {
  const outboundRequests = [];
  const options = {
    workers: [{
      rootPath: ROOT,
      modules: [
        { type: 'ESModule', path: 'scripts/posthog_workerd_worker.mjs', contents: workerModule },
        { type: 'ESModule', path: 'functions/events.js' },
      ],
      modulesRoot: ROOT,
      compatibilityDate: '2026-03-12',
      bindings: { POSTHOG_PROJECT_KEY: 'offline-project-key' },
      outboundService: async (request) => {
        outboundRequests.push({
          url: request.url,
          body: await request.text(),
        });
        return new Response(null, {
          status: 302,
          headers: { location: foreignRedirect },
        });
      },
    }],
  };
  const miniflare = new Miniflare(convertV4MiniflareOptions
    ? convertV4MiniflareOptions(options)
    : options);

  try {
    const response = await miniflare.dispatchFetch('https://stokegate.com/events', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        event: '$pageview',
        analytics_id: validId,
        path: '/cloud/',
        source: 'hero',
        target: 'cloud',
        email: 'must-not-leave-worker@example.com',
        use_case: 'must-not-leave-worker',
      }),
    });

    assert.equal(response.status, 204);
    assert.equal(outboundRequests.length, 1, 'valid events request must invoke offline outbound once');
    assert.equal(outboundRequests[0].url, posthogUrl);
    assert.deepEqual(JSON.parse(outboundRequests[0].body), {
      api_key: 'offline-project-key',
      distinct_id: validId,
      event: '$pageview',
      properties: {
        $process_person_profile: false,
        $geoip_disable: true,
        $ip: '0.0.0.0',
        path: '/cloud/',
        source: 'hero',
        target: 'cloud',
      },
    });
    assert.ok(!outboundRequests[0].body.includes('must-not-leave-worker'));
    assert.ok(!outboundRequests[0].body.includes('email'));
    assert.equal(outboundRequests[0].url, posthogUrl, 'redirect must not follow foreign host');
    assert.equal(outboundRequests.filter(({ url }) => url === foreignRedirect).length, 0);
  } finally {
    await miniflare.dispose();
  }
}

await exerciseRuntime();
console.log('workerd PostHog redirect regression passed');
