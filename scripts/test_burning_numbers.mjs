import test from 'node:test';
import assert from 'node:assert/strict';
import {sampleEmber, findComposition} from '../landing/burning-numbers.mjs';

test('numeric embers heat and fragment at literal cycle boundaries', () => {
  assert.equal(sampleEmber(0, 0).stage, 'cold');
  assert.equal(sampleEmber(2.99, 0).stage, 'cold');
  assert.equal(sampleEmber(3, 0).stage, 'hot');
  assert.equal(sampleEmber(6.2, 0).warm, 1);
  assert.equal(sampleEmber(7.59, 0).stage, 'hot');
  assert.equal(sampleEmber(7.6, 0).stage, 'ash');
  assert.equal(sampleEmber(10, 0).life, 0);
});
test('dense seeded lifecycle is finite, bounded, deterministic, and independent of product scenarios', () => {
  for (let i = 0; i < 1000; i++) {
    const a = sampleEmber(i / 37, (i % 97) / 97);
    assert.deepEqual(a, sampleEmber(i / 37, (i % 97) / 97));
    for (const value of Object.values(a).filter(x => typeof x === 'number')) assert(value >= 0 && value <= 1);
  }
});
test('invalid times and phases are rejected rather than painting NaN', () => {
  for (const [time, phase] of [[-1,0],[NaN,0],[Infinity,0],[1,-1],[1,1],[1,NaN]]) assert.throws(()=>sampleEmber(time,phase),RangeError);
});

test('initializer recognizes a composition that is the root itself', () => {
  const root = {matches: selector => selector === '.burning-composition', querySelector: () => {throw new Error('must not search descendants for itself');}};
  assert.equal(findComposition(root), root);
  const child = {};
  assert.equal(findComposition({matches:()=>false,querySelector:()=>child}), child);
});

for (const browserName of ['chromium', 'webkit']) test(`${browserName}: route ember and border pulse follow playback and policy`, {skip: process.env.STOKE_BROWSER_TESTS !== '1'}, async () => {
  const {[browserName]: engine} = await import('playwright');
  const {createServer} = await import('node:http');
  const {readFileSync} = await import('node:fs');
  const {fileURLToPath} = await import('node:url');
  const {resolve, extname, sep} = await import('node:path');
  const dir=fileURLToPath(new URL('../landing/',import.meta.url));
  const types={'.html':'text/html','.css':'text/css','.js':'text/javascript','.mjs':'text/javascript','.svg':'image/svg+xml','.woff2':'font/woff2'};
  const csp="default-src 'self'; script-src 'self'; style-src 'self'; style-src-attr 'none'; img-src 'self' data:; font-src 'self'; connect-src 'self'";
  const server=createServer((req,res)=>{try{const u=new URL(req.url,'http://localhost'),file=resolve(dir,'.'+u.pathname+(u.pathname.endsWith('/')?'index.html':''));if(!file.startsWith(dir.endsWith(sep)?dir:dir+sep)){res.writeHead(403).end();return}res.writeHead(200,{'content-type':types[extname(file)]||'application/octet-stream',...(file.endsWith('.html')?{'content-security-policy':csp}:{})}).end(readFileSync(file));}catch{res.writeHead(404).end();}});
  await new Promise(done=>server.listen(0,'127.0.0.1',done));let browser;
  try {
    browser=await engine.launch();const page=await browser.newPage({viewport:{width:1280,height:1100}}),errors=[];
    page.on('pageerror',e=>errors.push(e.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);await page.evaluate(()=>document.fonts.ready);
    await page.waitForFunction(()=>document.querySelector('[data-product-visual]').hasAttribute('data-ember-ready'),{},{timeout:5000});
    const button=page.locator('.burning-toggle'),overlay=page.locator('[data-ember-overlay]');assert(await button.isVisible());assert.equal(await page.locator('.burning-numbers').count(),0);
    await page.locator('[role=tab][data-scenario=privacy]').click();
    await page.waitForFunction(()=>document.querySelector('[data-product-visual]').dataset.phase==='dispatch');
    const outline=page.locator('.ember-flow__outline');
    assert.equal(await outline.getAttribute('fill'),'none','border never tints the content');
    assert.equal(await outline.getAttribute('transform'),null,'border stays on the exact panel contour');
    assert.equal(await outline.getAttribute('d'),await page.locator('.log-panel-surface>path').first().getAttribute('d'));
    assert.match(await outline.evaluate(el=>getComputedStyle(el).filter),/drop-shadow/,'edge has a warm glow');
    const initialHeat=await outline.getAttribute('opacity');
    await page.waitForFunction(before=>document.querySelector('.ember-flow__outline').getAttribute('opacity')!==before,initialHeat);
    assert.equal(await page.locator('[data-route-anchor=stoke]').evaluate(el=>getComputedStyle(el).transform),'none','content panel stays still');
    await button.click();await page.waitForFunction(()=>document.querySelector('[data-product-visual]').dataset.paused==='true');
    const frozen=await overlay.innerHTML(),frozenHeat=await outline.getAttribute('opacity');await page.waitForTimeout(250);assert.equal(await overlay.innerHTML(),frozen);
    assert.equal(await outline.getAttribute('opacity'),frozenHeat,'pause freezes border intensity');
    await button.click();await page.waitForFunction(before=>document.querySelector('[data-ember-overlay]').innerHTML!==before,frozen);assert.deepEqual(errors,[]);
    await page.waitForFunction(()=>document.querySelector('[data-product-visual]').dataset.phase==='complete');
    assert.equal(await outline.getAttribute('opacity'),'0','completed flow settles');
    await page.locator('[role=tab][data-scenario=budget]').click();
    await page.waitForFunction(()=>document.querySelector('[data-product-visual]').dataset.phase==='stopped');
    assert.equal(await outline.getAttribute('opacity'),'0','blocked request stops the glow');
    await page.locator('[role=tab][data-scenario=local]').click();
    await page.waitForFunction(()=>document.querySelector('[data-product-visual]').dataset.phase==='connection-failed');
    assert.equal(await outline.getAttribute('opacity'),'0','connection failure stops the glow');
    await page.waitForFunction(()=>document.querySelector('[data-product-visual]').dataset.phase==='permitted-retry');
    assert(Number(await outline.getAttribute('opacity'))>0,'permitted retry relights the border');
    await page.emulateMedia({reducedMotion:'reduce'});
    await page.waitForFunction(()=>document.querySelector('[data-product-visual]').dataset.phase==='complete');
    assert.equal(await outline.evaluate(el=>getComputedStyle(el).opacity),'0','reduced motion has no glow pulse');
    for(const width of [320,390,768,1280]){
      await page.setViewportSize({width,height:1500});
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`no border-induced overflow at ${width}px`);
    }
    assert.deepEqual(errors,[]);
  } finally {if(browser)await browser.close();await new Promise(done=>server.close(done));}
});
