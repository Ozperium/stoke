#!/usr/bin/env node
// Optional authoring tool: npm install --no-save playwright; install cwebp separately.
// Export committed SVG sources with the site's own font files, never generated art.
import {chromium} from 'playwright';
import {readFile, writeFile, mkdtemp, rm} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {resolve, dirname, join} from 'node:path';
import {tmpdir} from 'node:os';

const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const manifestPath=join(root,'design/use-cases/editorial/manifest.json');
const manifest=JSON.parse(await readFile(manifestPath,'utf8'));
const font=(name)=>pathToFileURL(join(root,'landing/fonts/'+name)).href;
const fonts=`@font-face{font-family:Sora;src:url('${font('sora.woff2')}');font-weight:400 700}@font-face{font-family:'Source Sans 3';src:url('${font('source-sans-3.woff2')}');font-weight:400 700}html,body{margin:0;padding:0;background:transparent}svg{display:block}`;
const work=await mkdtemp(join(tmpdir(),'stoke-brand-'));
const browser=await chromium.launch();
const hash=async(p)=>createHash('sha256').update(await readFile(p)).digest('hex');
const exports=[];
async function vector(page,source,width,height,target){
  const svg=(await readFile(source,'utf8')).replace(/<\?xml[^>]*\?>/g,'');
  const wrapper=join(work,'vector.html');
  await writeFile(wrapper,`<!doctype html><meta charset="utf-8"><style>${fonts}</style>${svg}`);
  await page.setViewportSize({width,height});
  await page.goto(pathToFileURL(wrapper).href);
  await page.evaluate(()=>document.fonts.ready);
  await page.locator('svg').first().screenshot({path:target,omitBackground:true,caret:'initial'});
}
try{
  const page=await browser.newPage({viewport:{width:1200,height:630},deviceScaleFactor:1,colorScheme:'light'});
  await vector(page,join(root,'landing/og-image.svg'),1200,630,join(root,'landing/og-image.png'));
  for(const item of manifest.items){
    const png=join(work,item.slug+'.png');
    await vector(page,join(root,item.source),1152,720,png);
    const result=spawnSync('cwebp',['-quiet','-q','90',png,'-o',join(root,item.hero)],{encoding:'utf8'});
    if(result.status!==0)throw Error('cwebp export failed: '+item.slug+' '+result.stderr);
    await page.setViewportSize({width:1200,height:630});
    await page.goto(pathToFileURL(join(root,'design/use-cases/social',item.slug+'.html')).href);
    await page.evaluate(()=>document.fonts.ready);
    await page.locator('.art img').evaluate(async image=>{await image.decode()});
    await page.screenshot({path:join(root,item.social),caret:'initial'});
    item.source_sha256=await hash(join(root,item.source));
    item.hero_sha256=await hash(join(root,item.hero));
    item.social_sha256=await hash(join(root,item.social));
    exports.push({slug:item.slug,hero:item.hero,social:item.social});
  }
  await writeFile(manifestPath,JSON.stringify(manifest,null,2)+'\n');
  console.log(JSON.stringify({exported:exports.length,items:exports,siteSocial:'landing/og-image.png'}));
}finally{await browser.close();await rm(work,{recursive:true,force:true})}
