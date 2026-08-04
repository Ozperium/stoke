const EVENTS = new Set([
  'install_cta_clicked',
  'install_section_viewed',
  'github_clicked',
]);

export async function onRequestPost({ request, env }) {
  let body;
  try {
    body = await request.json();
  } catch {
    return new Response(null, { status: 400 });
  }

  if (!body || !EVENTS.has(body.event)) {
    return new Response(null, { status: 400 });
  }

  const path = typeof body.path === 'string' ? body.path.slice(0, 160) : '';
  const source = typeof body.source === 'string' ? body.source.slice(0, 80) : '';
  const target = typeof body.target === 'string' ? body.target.slice(0, 200) : '';

  if (env.STOKE_EVENTS) {
    await env.STOKE_EVENTS.prepare(
      'INSERT INTO events (event, path, source, target, created_at) VALUES (?, ?, ?, ?, ?)'
    ).bind(body.event, path, source, target, new Date().toISOString()).run();
  }

  return new Response(null, {
    status: 204,
    headers: { 'Cache-Control': 'no-store' },
  });
}
