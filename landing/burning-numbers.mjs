const STATIC_TIME = 4.35;

const clamp = (value, min, max) => Math.max(min, Math.min(max, value));
const seeded = (value) => {
  const raw = Math.sin(value * 127.1 + 42) * 43758.5453;
  return raw - Math.floor(raw);
};
const queryAll = (root, selector) => [...root.querySelectorAll(selector)];

export function sampleEmber(time, phase) {
  if (!Number.isFinite(time) || time < 0 || !Number.isFinite(phase) || phase < 0 || phase >= 1) {
    throw new RangeError('Ember time and seeded phase must be finite and nonnegative');
  }
  const life = (time / 10 + phase) % 1;
  return {
    life,
    stage: life < .30 ? 'cold' : life < .76 ? 'hot' : 'ash',
    warm: clamp((life - .30) / .32, 0, 1),
    fade: Math.min(1, life / .1),
    fragment: life < .76 ? 0 : (life - .76) / .24,
  };
}

function makeAnchors(panel, compositionWidth, compositionHeight) {
  const count = compositionWidth < 700 ? 36 : 72;
  const baseCount = count - 12;
  const left = panel.left;
  const right = panel.right;
  const top = panel.top;
  const bottom = panel.bottom;
  return Array.from({length: count}, (_, index) => {
    const side = index >= baseCount;
    const onLeft = index % 2 === 0;
    const x = side
      ? (onLeft ? left - 14 - seeded(index) * 17 : right + 14 + seeded(index) * 17)
      : left + 20 + (right - left - 40) * (index + .5) / baseCount;
    const y = side
      ? top + (bottom - top) * (.45 + seeded(index + 5) * .44)
      : bottom - 35 + Math.sin(index * .35) * 12 + seeded(index + 9) * 13;
    return {
      x: clamp(x, -20, compositionWidth + 20),
      y: clamp(y, -20, compositionHeight + 20),
      size: 11 + seeded(index + 7) * 6,
      phase: seeded(index + 13),
      digit: String((index * 7 + 3) % 10),
    };
  });
}

function exclusionRects(compositionRect) {
  // Keep decorative numerals away from both live content and semantic wires.
  return queryAll(document, [
    '.product-visual__toolbar',
    '.product-visual__actor-label',
    '.product-visual__card',
    '.product-visual__destination',
    '.product-visual__core',
    '.product-visual__note',
    '.burning-controls',
    '.product-visual__route',
  ].join(',')).map((element) => {
    const rect = element.getBoundingClientRect();
    return {
      x: rect.left - compositionRect.left - 10,
      y: rect.top - compositionRect.top - 10,
      width: rect.width + 20,
      height: rect.height + 20,
    };
  });
}

export function findComposition(root) {
  return root.matches('.burning-composition') ? root : root.querySelector('.burning-composition');
}

function start(root) {
  const canvas = root.querySelector('.burning-numbers');
  const composition = findComposition(root);
  const panel = root.querySelector('[data-visual-canvas]');
  const button = root.querySelector('.burning-toggle');
  if (!canvas || !composition || !panel || !button) return;
  const context = canvas.getContext('2d');
  if (!context) return;
  root.querySelector('.burning-controls').hidden = false;
  document.documentElement.setAttribute('data-burning-ready', '');

  const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
  let paused = false;
  let inView = true;
  let time = 3;
  let lastTime = 0;
  let frameId = 0;
  let cacheFrame = 0;
  let width = 0;
  let height = 0;
  let pixelRatio = 1;
  let anchors = [];
  let exclusions = [];
  let suspended = false;

  function cacheLayout() {
    cacheFrame = 0;
    const compositionRect = canvas.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    width = compositionRect.width;
    height = compositionRect.height;
    pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.max(1, Math.round(width * pixelRatio));
    canvas.height = Math.max(1, Math.round(height * pixelRatio));
    context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
    anchors = makeAnchors({
      left: panelRect.left - compositionRect.left,
      right: panelRect.right - compositionRect.left,
      top: panelRect.top - compositionRect.top,
      bottom: panelRect.bottom - compositionRect.top,
    }, width, height);
    exclusions = exclusionRects(compositionRect);
    draw(reducedMotion.matches ? STATIC_TIME : time);
  }

  function scheduleCache() {
    if (cacheFrame || suspended) return;
    cacheFrame = requestAnimationFrame(cacheLayout);
  }

  function draw(atTime) {
    context.clearRect(0, 0, width, height);
    context.textAlign = 'center';
    context.textBaseline = 'middle';
    for (const anchor of anchors) {
      const {life, warm, fade, fragment} = sampleEmber(atTime, anchor.phase);
      const y = anchor.y - life * 25;
      context.font = `${anchor.size}px ui-monospace, SFMono-Regular, Menlo, monospace`;
      if (life < .76) {
        context.fillStyle = `rgba(25,25,25,${.14 * (1 - warm) * fade})`;
        context.fillText(anchor.digit, anchor.x, y);
        context.fillStyle = `rgba(241,83,50,${.45 * warm * fade})`;
        context.fillText(anchor.digit, anchor.x, y);
      } else {
        context.fillStyle = `rgba(241,83,50,${.34 * (1 - fragment)})`;
        for (let part = 0; part < 3; part += 1) {
          context.fillRect(anchor.x + (part - 1) * (2 + fragment * 5), y - fragment * (16 + part * 4), 1.2, 1.6);
        }
      }
    }
    for (const rect of exclusions) context.clearRect(rect.x, rect.y, rect.width, rect.height);
  }

  function stopFrame() {
    if (frameId) cancelAnimationFrame(frameId);
    frameId = 0;
    lastTime = 0;
  }

  function shouldStop() {
    return paused || suspended || document.hidden || !inView || reducedMotion.matches;
  }

  function updateControl() {
    const stoppedByMotion = reducedMotion.matches;
    button.setAttribute('aria-pressed', String(paused));
    button.textContent = paused ? 'Resume ambience' : 'Pause ambience';
    button.setAttribute('aria-label', stoppedByMotion ? 'Motion reduced; numbers are static' : `${paused ? 'Resume' : 'Pause'} ambient numbers`);
    document.documentElement.toggleAttribute('data-burning-paused', shouldStop());
  }

  function tick(now) {
    if (shouldStop()) {
      stopFrame();
      updateControl();
      draw(reducedMotion.matches ? STATIC_TIME : time);
      return;
    }
    if (!lastTime) lastTime = now;
    const delta = Math.min(now - lastTime, 80);
    if (delta >= 48) {
      time += delta / 1000;
      draw(time);
      lastTime = now;
    }
    frameId = requestAnimationFrame(tick);
  }

  function sync() {
    stopFrame();
    updateControl();
    draw(reducedMotion.matches ? STATIC_TIME : time);
    if (!shouldStop()) frameId = requestAnimationFrame(tick);
  }

  button.addEventListener('click', () => {
    paused = !paused;
    sync();
  });
  document.addEventListener('visibilitychange', sync);
  window.addEventListener('pagehide', () => {
    suspended = true;
    stopFrame();
    if (cacheFrame) cancelAnimationFrame(cacheFrame);
    cacheFrame = 0;
    updateControl();
  });
  window.addEventListener('pageshow', () => {
    suspended = false;
    scheduleCache();
    sync();
  });
  const motionChange = () => sync();
  if (reducedMotion.addEventListener) reducedMotion.addEventListener('change', motionChange);
  else reducedMotion.addListener(motionChange);

  const resizeObserver = new ResizeObserver(scheduleCache);
  resizeObserver.observe(composition);
  const mutationObserver = new MutationObserver(scheduleCache);
  mutationObserver.observe(panel, {subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ['data-phase', 'data-scenario', 'aria-labelledby', 'd']});
  const intersectionObserver = new IntersectionObserver(([entry]) => {
    inView = entry.isIntersecting;
    sync();
  });
  intersectionObserver.observe(composition);
  if (document.fonts) document.fonts.ready.then(scheduleCache);

  cacheLayout();
  sync();
}

if (typeof document !== 'undefined') {
  for (const root of document.querySelectorAll('[data-burning-numbers]')) start(root);
}
