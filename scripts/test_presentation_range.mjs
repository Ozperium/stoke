import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
const source=await readFile(new URL('../functions/media/stoke-preview.mp4.js',import.meta.url),'utf8');
const {onRequest}=await import('data:text/javascript,'+encodeURIComponent(source));
const bytes=await readFile(new URL('../landing/media/stoke-presentation.mp4',import.meta.url));
function context(headers={},method='GET',status=200){
 const calls=[];return {request:new Request('https://example.invalid/media/stoke-presentation.mp4',{method,headers}),calls,env:{ASSETS:{fetch:async request=>{calls.push(request);return new Response(method==='HEAD'||status!==200?null:bytes,{status,headers:{'Content-Type':'video/mp4','Content-Length':String(bytes.length),'ETag':'"sample-etag"'}});}}}};
}
test('full GET returns the exact asset and advertises byte ranges',async()=>{const c=context();const r=await onRequest(c);assert.equal(r.status,200);assert.equal(r.headers.get('accept-ranges'),'bytes');assert.deepEqual(Buffer.from(await r.arrayBuffer()),bytes);});
for(const [range,start,end] of [['bytes=0-1023',0,1023],['bytes=120000-121023',120000,121023],['bytes=120000-',120000,bytes.length-1],['bytes=-1024',bytes.length-1024,bytes.length-1]]){
 test('single range '+range,async()=>{const c=context({Range:range});const r=await onRequest(c);assert.equal(r.status,206);assert.equal(r.headers.get('content-range'),`bytes ${start}-${end}/${bytes.length}`);assert.equal(c.calls[0].headers.get('range'),null);assert.deepEqual(Buffer.from(await r.arrayBuffer()),bytes.subarray(start,end+1));});
}
for(const range of ['bytes=999999999-','bytes=100-10','bytes=0-1,4-5','bytes=-0','bytes=9999999999999999999999-'])test('unsatisfiable range '+range,async()=>{const r=await onRequest(context({Range:range}));assert.equal(r.status,416);assert.equal(r.headers.get('content-range'),`bytes */${bytes.length}`);assert.equal((await r.arrayBuffer()).byteLength,0);});
test('If-Range mismatch sends full response',async()=>{const r=await onRequest(context({Range:'bytes=0-9','If-Range':'"stale"'}));assert.equal(r.status,200);assert.equal((await r.arrayBuffer()).byteLength,bytes.length);});
test('matching If-Range permits partial response',async()=>{const r=await onRequest(context({Range:'bytes=0-9','If-Range':'"sample-etag"'}));assert.equal(r.status,206);assert.equal((await r.arrayBuffer()).byteLength,10);});
test('HEAD has full length and no body',async()=>{const r=await onRequest(context({},'HEAD'));assert.equal(r.status,200);assert.equal(r.headers.get('content-length'),String(bytes.length));assert.equal((await r.arrayBuffer()).byteLength,0);});
test('non-success asset status is preserved',async()=>{const r=await onRequest(context({},'GET',404));assert.equal(r.status,404);});
test('unsupported methods do not call assets',async()=>{const c=context({},'POST');const r=await onRequest(c);assert.equal(r.status,405);assert.equal(c.calls.length,0);assert.equal(r.headers.get('Allow'),'GET, HEAD');});
