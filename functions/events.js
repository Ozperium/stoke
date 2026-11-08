const EVENTS = new Set([
  '$pageview',
  'cloud_viewed',
  'pilot_form_opened',
  'install_cta_clicked',
  'install_section_viewed',
  'github_clicked',
  'panel_demo_clicked',
  'local_capacity_cta_clicked',
  'guides_clicked',
  'cloud_invitation_requested',
]);

const PUBLIC_PATHS = new Set([
  '/',
  '/cloud/',
  '/contact/',
  '/gateway/',
  '/guides/',
  '/guides/block-prompt-injection-at-the-gateway/',
  '/guides/cache-identity/',
  '/guides/cache-policy/',
  '/guides/cap-claude-code-api-spend/',
  '/guides/expose-ollama-to-your-network-safely/',
  '/guides/optional-headroom/',
  '/guides/run-ollama-across-two-machines/',
  '/guides/stop-a-runaway-ai-agent/',
  '/guides/use-codex-and-ollama-with-stoke/',
  '/join/',
  '/legal/',
  '/security/',
  '/use-cases/',
  '/use-cases/agent-spend/',
  '/use-cases/local-models/',
  '/use-cases/node-failover/',
  '/use-cases/private-data/',
  '/use-cases/private-model-access/',
  '/use-cases/secret-redaction/',
]);

const SOURCES = new Set(['cloud', 'direct', 'email', 'footer', 'github', 'hero', 'nav', 'product', 'social']);
const TARGETS = new Set(['cloud', 'contact', 'gateway', 'github', 'install', 'join', 'legal']);
const POSTHOG_ENDPOINT = 'https://eu.i.posthog.com/i/v0/e/';
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const MAX_REQUEST_BYTES = 16 * 1024;
const PRODUCTION_ORIGINS = new Set(['https://stokegate.com', 'https://www.stokegate.com']);

function allowedValue(value, allowed) {
  return typeof value === 'string' && allowed.has(value) ? value : '';
}

function validAnalyticsId(value) {
  return typeof value === 'string' && UUID_V4.test(value) ? value : '';
}

async function forwardToPostHog(env, event, analyticsId, path, source, target) {
  if (!env.POSTHOG_PROJECT_KEY || !analyticsId) return;

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 1500);
  try {
    await fetch(POSTHOG_ENDPOINT, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        api_key: env.POSTHOG_PROJECT_KEY,
        distinct_id: analyticsId,
        event,
        properties: {
          $process_person_profile: false,
          $geoip_disable: true,
          $ip: '0.0.0.0',
          path,
          source,
          target,
        },
      }),
      redirect: 'manual',
      signal: controller.signal,
    });
  } catch {
    // Analytics is best-effort and must never affect the public site response.
  } finally {
    clearTimeout(timeout);
  }
}

function privacySignal(value) {
  return typeof value === 'string' && ['1', 'yes', 'true'].includes(value.trim().toLowerCase());
}

function requestOriginIsAllowed(request) {
  let url;
  try {
    url = new URL(request.url);
  } catch {
    return false;
  }
  if (!PRODUCTION_ORIGINS.has(url.origin)) return false;
  const origin = request.headers.get('origin');
  return !origin || origin === url.origin;
}

function requestPrivacyBlocked(request) {
  return privacySignal(request.headers.get('dnt'))
    || privacySignal(request.headers.get('sec-gpc'))
    || privacySignal(request.headers.get('global-privacy-control'));
}

async function readJson(request) {
  const contentLength = Number(request.headers.get('content-length'));
  if (Number.isFinite(contentLength) && contentLength > MAX_REQUEST_BYTES) {
    throw new RangeError('request too large');
  }
  const reader = request.body?.getReader();
  if (!reader) {
    const text = await request.text();
    if (new TextEncoder().encode(text).byteLength > MAX_REQUEST_BYTES) throw new RangeError('request too large');
    return JSON.parse(text);
  }
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_REQUEST_BYTES) {
        await reader.cancel();
        throw new RangeError('request too large');
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return JSON.parse(new TextDecoder().decode(bytes));
}

export async function onRequestPost({ request, env }) {
  if (request.method !== 'POST') return new Response(null, { status: 405 });
  if (!requestOriginIsAllowed(request)) return new Response(null, { status: 403 });
  if (requestPrivacyBlocked(request)) return new Response(null, { status: 204 });
  const contentType = request.headers.get('content-type') || '';
  if (!contentType.toLowerCase().startsWith('application/json')) {
    return new Response(null, { status: 415 });
  }

  let body;
  try {
    body = await readJson(request);
  } catch (error) {
    return new Response(null, { status: error instanceof RangeError ? 413 : 400 });
  }

  if (!body || typeof body !== 'object' || Array.isArray(body) || !EVENTS.has(body.event)) {
    return new Response(null, { status: 400 });
  }

  const path = allowedValue(body.path, PUBLIC_PATHS);
  const source = allowedValue(body.source, SOURCES);
  const target = allowedValue(body.target, TARGETS);
  const analyticsId = validAnalyticsId(body.analytics_id);

  if (env.STOKE_EVENTS) {
    await env.STOKE_EVENTS.prepare(
      'INSERT INTO events (event, path, source, target, created_at) VALUES (?, ?, ?, ?, ?)'
    ).bind(body.event, path, source, target, new Date().toISOString()).run();
  }

  await forwardToPostHog(env, body.event, analyticsId, path, source, target);

  return new Response(null, {
    status: 204,
    headers: { 'Cache-Control': 'no-store' },
  });
}
