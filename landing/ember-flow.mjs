import {presentationState, wakeDistances} from './ember-flow-model.mjs';

const NS = 'http://www.w3.org/2000/svg';
let instance = 0;

export function findEmberComposition(root) {
  return root.matches('[data-ember-flow]') ? root : root.querySelector('[data-ember-flow]');
}

// Enhance the existing illustrative packet, never create a second request stream.
export function mountEmberFlow(root) {
  const composition = findEmberComposition(root);
  const svg = root.querySelector('.product-visual__routes');
  const frame = root.querySelector('[data-visual-canvas]');
  const core = root.querySelector('[data-route-anchor="stoke"]');
  const button = composition?.querySelector('.burning-toggle');
  const play = root.querySelector('[data-role="visual-play"]');
  const surface = core?.querySelector('.log-panel-surface');
  const basePath = surface?.querySelector('path');
  const controlsNode = composition?.querySelector('.burning-controls');
  if (!composition || !svg || !frame || !button || !play || !basePath || !controlsNode) return null;
  if (root.hasAttribute('data-ember-ready')) return null;

  const id = `ember-flow-${++instance}`;
  function element(tag, attrs, parent) {
    const node = document.createElementNS(NS, tag);
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
    parent?.append(node);
    return node;
  }
  const defs = element('defs', {}, svg);
  const mask = element('mask', {id: `${id}-mask`, maskUnits: 'userSpaceOnUse'}, defs);
  const base = element('rect', {fill: 'white'}, mask);
  const holes = element('g', {fill: 'black'}, mask);
  const overlay = element('g', {class: 'ember-flow__overlay', 'data-ember-overlay': '', mask: `url(#${id}-mask)`}, svg);
  const gradient = element('linearGradient', {id: `${id}-wake`, gradientUnits: 'userSpaceOnUse'}, defs);
  const stops = [
    element('stop', {offset: 0, 'stop-color': '#F15332', 'stop-opacity': '.65'}, gradient),
    element('stop', {offset: 1, 'stop-color': '#F15332', 'stop-opacity': 0}, gradient),
  ];
  const wake = element('path', {fill: 'none', stroke: `url(#${id}-wake)`, 'stroke-width': 2.5, 'stroke-linecap': 'round', opacity: 0}, overlay);
  const halo = element('circle', {r: 10, fill: '#F15332', opacity: 0}, overlay);
  const head = element('circle', {r: 3.5, fill: '#F15332', opacity: 0}, overlay);
  const outline = basePath.cloneNode(false);
  for (const [key, value] of Object.entries({class: 'ember-flow__outline', fill: '#F15332', 'fill-opacity': '.02', stroke: '#F15332', 'stroke-width': 2, opacity: 0})) outline.setAttribute(key, value);
  surface.append(outline);

  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  let routes = {}, inView = true, suspended = false, destroyed = false;
  let raf = 0, cacheFrame = 0, lastPaint = -Infinity;
  const readState = () => presentationState(root.dataset.scenario, Number(root.dataset.elapsed || 0), root.dataset.phase);
  let state = readState();
  const blocked = () => destroyed || suspended || !inView || document.hidden || reduced.matches || root.dataset.paused === 'true';
  function stop() { cancelAnimationFrame(raf); raf = 0; }
  function hide() {
    for (const node of [wake, halo, head, outline]) node.setAttribute('opacity', '0');
  }
  function rectHole(rect, bounds, pad = 2) {
    if (!rect.width || !rect.height) return;
    element('rect', {x: rect.left - bounds.left - pad, y: rect.top - bounds.top - pad, width: rect.width + 2 * pad, height: rect.height + 2 * pad}, holes);
  }
  function cache() {
    cacheFrame = 0;
    if (destroyed || suspended) return;
    const bounds = frame.getBoundingClientRect();
    for (const [key, value] of Object.entries({x: -64, y: -64, width: bounds.width + 128, height: bounds.height + 128})) base.setAttribute(key, value);
    routes = {};
    for (const path of svg.querySelectorAll('[data-route]')) {
      if (path.getAttribute('d')) routes[path.dataset.route] = {path, length: path.getTotalLength()};
    }
    holes.replaceChildren();
    for (const node of root.querySelectorAll('.product-visual__card,.product-visual__destination,.product-visual__core')) rectHole(node.getBoundingClientRect(), bounds, 1);
    for (const label of root.querySelectorAll('.product-visual__actor-label,.product-visual__group')) {
      for (const node of label.childNodes) {
        if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) {
          const range = document.createRange();
          range.selectNodeContents(node);
          for (const rect of range.getClientRects()) rectHole(rect, bounds, 3);
        } else if (node.nodeType === Node.ELEMENT_NODE) rectHole(node.getBoundingClientRect(), bounds, 3);
      }
    }
    if (!blocked()) draw();
  }
  function scheduleCache() {
    if (!cacheFrame && !suspended && !destroyed) cacheFrame = requestAnimationFrame(cache);
  }
  function draw() {
    state = readState();
    hide();
    if (reduced.matches) return;
    const working = state.policy === 'allowed' && !['ingress', 'connection-failed', 'complete'].includes(state.phase);
    const pulse = working ? Math.pow(Math.max(0, Math.sin((state.elapsed - 850) / 1500 * Math.PI)), 4) : 0;
    outline.setAttribute('opacity', String(pulse * .65));
    outline.setAttribute('transform', `translate(50 50) scale(${1 + pulse * .016}) translate(-50 -50)`);
    const packet = state.packet;
    const route = packet && routes[packet.path];
    if (!packet || !route || !state.visiblePaths.includes(packet.path)) return;
    if (state.failedPaths.includes(packet.path) || (packet.path !== 'ingress' && state.policy !== 'allowed')) return;
    const points = wakeDistances(packet, route.length).map(distance => route.path.getPointAtLength(distance));
    const point = points[0];
    if (!point) return;
    const color = packet.response ? '#191919' : packet.path === 'ingress' ? '#696963' : '#F15332';
    for (const node of [head, halo]) {
      node.setAttribute('transform', `translate(${point.x} ${point.y})`);
      node.setAttribute('fill', color);
    }
    head.setAttribute('opacity', '1');
    halo.setAttribute('opacity', '.085');
    wake.setAttribute('d', points.map((p, i) => `${i ? 'L' : 'M'}${p.x},${p.y}`).join(' '));
    const tail = points.at(-1);
    for (const [key, value] of Object.entries({x1: point.x, y1: point.y, x2: tail.x, y2: tail.y})) gradient.setAttribute(key, value);
    for (const node of stops) node.setAttribute('stop-color', color);
    wake.setAttribute('opacity', '1');
  }
  function controls() {
    const paused = root.dataset.paused === 'true';
    const action = paused ? 'Resume' : root.dataset.playing === 'true' ? 'Pause' : 'Replay';
    button.disabled = reduced.matches;
    button.setAttribute('aria-pressed', String(paused));
    button.textContent = reduced.matches ? 'Motion reduced' : `${action} motion`;
    button.setAttribute('aria-label', reduced.matches ? 'Reduced motion: static illustration' : `${action} illustration and route effect`);
  }
  function tick(now) {
    if (blocked()) { stop(); return; }
    if (now - lastPaint > 28) { draw(); lastPaint = now; }
    if (root.dataset.playing === 'true') raf = requestAnimationFrame(tick);
    else raf = 0;
  }
  function reconcile() {
    if (destroyed) return;
    controls();
    const changed = root.dataset.scenario !== state.scenario || root.dataset.phase !== state.phase;
    if (root.dataset.scenario !== state.scenario || (changed && !readState().packet)) { state = readState(); hide(); }
    if (blocked()) { stop(); if (reduced.matches) { state = readState(); hide(); } return; }
    if (changed) cache(); else draw();
    if (!raf && root.dataset.playing === 'true') raf = requestAnimationFrame(tick);
  }
  const toggle = () => { play.click(); reconcile(); };
  const pagehide = () => { suspended = true; stop(); cancelAnimationFrame(cacheFrame); cacheFrame = 0; };
  const pageshow = () => { suspended = false; scheduleCache(); reconcile(); };
  button.addEventListener('click', toggle);
  const metadata = new MutationObserver(reconcile);
  metadata.observe(root, {attributes: true, attributeFilter: ['data-playing', 'data-paused', 'data-scenario', 'data-phase']});
  // Never observe our own wake path: that would schedule layout every frame.
  const geometry = new MutationObserver(scheduleCache);
  for (const path of svg.querySelectorAll('[data-route]')) geometry.observe(path, {attributes: true, attributeFilter: ['d']});
  const resize = new ResizeObserver(scheduleCache);
  resize.observe(frame); resize.observe(core);
  const intersection = new IntersectionObserver(([entry]) => { inView = entry.isIntersecting; reconcile(); });
  intersection.observe(composition);
  document.addEventListener('visibilitychange', reconcile);
  reduced.addEventListener('change', reconcile);
  window.addEventListener('pagehide', pagehide);
  window.addEventListener('pageshow', pageshow);
  document.fonts.ready.then(scheduleCache);
  cache();
  root.setAttribute('data-ember-ready', '');
  controlsNode.hidden = false;
  reconcile();
  return {destroy() {
    destroyed = true; stop(); cancelAnimationFrame(cacheFrame);
    metadata.disconnect(); geometry.disconnect(); resize.disconnect(); intersection.disconnect();
    button.removeEventListener('click', toggle);
    document.removeEventListener('visibilitychange', reconcile);
    reduced.removeEventListener('change', reconcile);
    window.removeEventListener('pagehide', pagehide);
    window.removeEventListener('pageshow', pageshow);
    overlay.remove(); defs.remove(); outline.remove();
    root.removeAttribute('data-ember-ready');
    controlsNode.hidden = true;
  }};
}

if (typeof document !== 'undefined') {
  for (const root of document.querySelectorAll('[data-product-visual]')) mountEmberFlow(root);
}
