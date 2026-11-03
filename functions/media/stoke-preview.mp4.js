// This small public preview asset needs explicit single-byte-range support:
// the Pages preview asset server can otherwise ignore Range and break seeking.
export async function onRequest(context) {
  const { request } = context;
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    return new Response(null, { status: 405, headers: { Allow: 'GET, HEAD' } });
  }
  const input = new Headers(request.headers);
  const range = input.get('Range');
  const ifRange = input.get('If-Range');
  input.delete('Range');
  input.delete('If-Range');
  input.set('Accept-Encoding', 'identity');
  const asset = await context.env.ASSETS.fetch(new Request(request, { headers: input }));
  if (asset.status !== 200) return asset;
  const headers = new Headers(asset.headers);
  headers.set('Accept-Ranges', 'bytes');
  headers.set('Cache-Control', 'public, max-age=0, must-revalidate');
  if (request.method === 'HEAD') return new Response(null, { status: 200, headers });
  const body = await asset.arrayBuffer();
  const size = body.byteLength;
  headers.set('Content-Length', String(size));
  if (!range || (ifRange && ifRange !== asset.headers.get('ETag'))) {
    return new Response(body, { status: 200, headers });
  }
  const invalid = () => {
    headers.set('Content-Range', `bytes */${size}`);
    headers.set('Content-Length', '0');
    return new Response(null, { status: 416, headers });
  };
  const match = /^bytes=(\d*)-(\d*)$/.exec(range);
  if (!match || (!match[1] && !match[2])) return invalid();
  let start, end;
  if (!match[1]) {
    const suffix = Number(match[2]);
    if (!Number.isSafeInteger(suffix) || suffix <= 0) return invalid();
    start = Math.max(0, size - suffix);
    end = size - 1;
  } else {
    start = Number(match[1]);
    end = match[2] ? Number(match[2]) : size - 1;
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start >= size || end < start) return invalid();
    end = Math.min(end, size - 1);
  }
  if (start > end) return invalid();
  const partial = body.slice(start, end + 1);
  headers.set('Content-Range', `bytes ${start}-${end}/${size}`);
  headers.set('Content-Length', String(partial.byteLength));
  return new Response(partial, { status: 206, headers });
}
