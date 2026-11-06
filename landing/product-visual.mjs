import {DURATIONS, SCENARIOS, sampleScenario} from './product-visual-model.mjs';

const COPY = Object.freeze({
  privacy: Object.freeze({
    request: 'Summarize these notes.<span class="product-visual__content-line">alex@example.invalid</span>',
    ingress: ['Request entering Stoke', 'Summarize these notes.<br><span class="product-visual__datum">alex@example.invalid</span>', 'Waiting for policy', 'Inspecting configured content rules'],
    inspect: ['Inspecting supported content', 'Summarize these notes.<br><span class="product-visual__datum">alex@example.invalid</span>', 'Pattern match found', 'Opt-in pattern filtering'],
    mask: ['Matched value replaced inside Stoke', 'Summarize these notes.<br><span class="product-visual__datum product-visual__datum--matched">EMAIL:[REDACTED]</span>', 'Permission checked', 'Task preserved · matched value masked'],
    dispatch: ['Masked request leaving Stoke', 'Summarize these notes.<br><span class="product-visual__datum product-visual__datum--matched">EMAIL:[REDACTED]</span>', 'Allowed · sending', 'Only the masked request is dispatched'],
    response: ['Response returning to Stoke', 'Summarize these notes.<br><span class="product-visual__datum product-visual__datum--matched">EMAIL:[REDACTED]</span>', 'Response returning', 'Sensitive values remain redacted'],
    return: ['Response returning to app', 'Summarize these notes.<br><span class="product-visual__datum product-visual__datum--matched">EMAIL:[REDACTED]</span>', 'Response returning', 'Returning through Stoke to the same app'],
    complete: ['Masked inside Stoke', 'Summarize these notes.<br><span class="product-visual__datum product-visual__datum--matched">EMAIL:[REDACTED]</span>', 'Reply returned', 'Redacted value stays redacted'],
  }),
  budget: Object.freeze({
    request: 'Draft a new project brief.<span class="product-visual__content-line">Independent request</span>',
    ingress: ['New request entering Stoke', 'Draft a new project brief.', 'Waiting for policy', 'Independent illustrative request'],
    inspect: ['Budget control evaluated', 'Draft a new project brief.', 'Checking budget', 'Configured limit applies to this request'],
    stopped: ['Stopped at Stoke', '<span class="product-visual__datum product-visual__datum--matched">Request stopped.</span><br><span class="product-visual__datum">Provider not called</span>', 'Budget limit', 'No outgoing route'],
    complete: ['Stopped at Stoke', '<span class="product-visual__datum product-visual__datum--matched">Request stopped.</span><br><span class="product-visual__datum">Provider not called</span>', 'Budget limit', 'No outgoing route'],
  }),
  local: Object.freeze({
    request: 'Review this draft locally.<span class="product-visual__content-line">Same configured model</span>',
    ingress: ['Local request entering Stoke', 'Review this draft locally.', 'Waiting for policy', 'Independent local request'],
    inspect: ['Local policy evaluated', 'Review this draft locally.<br><span class="product-visual__datum">Same configured model</span>', 'Nodes permitted', 'Configured model stays unchanged'],
    'dispatch-laptop': ['Connecting from Stoke', 'Laptop · same configured model', 'Attempting Laptop', 'Direct route from Stoke'],
    'connection-failed': ['Connection failed before output', '<span class="product-visual__datum product-visual__datum--matched">Laptop unavailable.</span><br><span class="product-visual__datum">No output received</span>', 'Retry bounded', 'Failure occurred before inference output'],
    'permitted-retry': ['Permitted retry from Stoke', 'Workstation · same configured model', 'Workstation selected', 'Direct route · configured retry limit'],
    response: ['Response returning to Stoke', 'Workstation answered.<br><span class="product-visual__datum">Same configured model</span>', 'Response returning', 'Bounded retry complete · Laptop failure retained'],
    return: ['Response returning to app', 'Workstation answered.<br><span class="product-visual__datum">Same configured model</span>', 'Response returning', 'Returning through Stoke to the same app'],
    complete: ['Response returned through Stoke', 'Workstation answered.<br><span class="product-visual__datum product-visual__datum--matched">Same configured model</span>', 'Reply returned', 'Failed Laptop retained · Workstation selected'],
  }),
});

const CAPTIONS = Object.freeze({
  ingress: 'A request reaches Stoke.',
  inspect: 'Stoke checks the configured rule before dispatch.',
  mask: 'The matched value is replaced inside Stoke.',
  dispatch: 'Only the transformed request leaves Stoke.',
  response: 'The model response is returning through Stoke.',
  return: 'The response returns to the same app.',
  stopped: 'The request stops at Stoke. No provider is called.',
  'dispatch-laptop': 'Stoke tries the first permitted local node.',
  'connection-failed': 'Laptop fails before any output is received.',
  'permitted-retry': 'Stoke selects Workstation on the same configured model.',
  complete: 'Illustrative flow complete.',
});

const q = (root, selector) => root.querySelector(selector);
const qa = (root, selector) => [...root.querySelectorAll(selector)];

function pointFor(element, side, canvasRect) {
  const rect = element.getBoundingClientRect();
  if (side === 'left') return {x: rect.left - canvasRect.left, y: rect.top - canvasRect.top + rect.height / 2};
  if (side === 'right') return {x: rect.right - canvasRect.left, y: rect.top - canvasRect.top + rect.height / 2};
  if (side === 'top') return {x: rect.left - canvasRect.left + rect.width / 2, y: rect.top - canvasRect.top};
  return {x: rect.left - canvasRect.left + rect.width / 2, y: rect.bottom - canvasRect.top};
}

function routeBetween(from, to, canvas) {
  const canvasRect = canvas.getBoundingClientRect();
  const fromRect = from.getBoundingClientRect();
  const toRect = to.getBoundingClientRect();
  const horizontal = toRect.left - fromRect.right > 22;
  const stackedDestination = !horizontal && from.dataset.routeAnchor === 'stoke' && to.dataset.destination;
  const a = pointFor(from, horizontal ? 'right' : 'bottom', canvasRect);
  if (horizontal) {
    const b = pointFor(to, 'left', canvasRect);
    const bend = Math.max(32, (b.x - a.x) * .48);
    return `M${a.x},${a.y} C${a.x + bend},${a.y} ${b.x - bend},${b.y} ${b.x},${b.y}`;
  }
  if (stackedDestination) {
    const useLeftGutter = toRect.left + toRect.width / 2 < canvasRect.left + canvasRect.width / 2;
    const gutter = useLeftGutter ? 11 : canvasRect.width - 11;
    const targetSide = useLeftGutter ? 'left' : 'right';
    const b = pointFor(to, targetSide, canvasRect);
    // Stay inside the inter-actor gap before descending beside the labels.
    const emberFlow = !!canvas.closest('[data-ember-flow]');
    const turn = emberFlow ? 10 : 3;
    const clearance = emberFlow ? 12 : 6;
    return `M${a.x},${a.y} C${a.x},${a.y + turn} ${gutter},${a.y + turn} ${gutter},${a.y + clearance} L${gutter},${b.y} L${b.x},${b.y}`;
  }
  const b = pointFor(to, 'top', canvasRect);
  const bend = Math.max(24, (b.y - a.y) * .48);
  return `M${a.x},${a.y} C${a.x},${a.y + bend} ${b.x},${b.y - bend} ${b.x},${b.y}`;
}

function installRoutes(root) {
  const canvas = q(root, '[data-visual-canvas]');
  if (!canvas) return {};
  const anchors = Object.fromEntries(qa(canvas, '[data-route-anchor]').map((el) => [el.dataset.routeAnchor, el]));
  const routes = Object.fromEntries(qa(canvas, '[data-route]').map((el) => [el.dataset.route, el]));
  const pairs = {
    ingress: ['client', 'stoke'],
    outboundOpenai: ['stoke', 'openai'],
    outboundLaptop: ['stoke', 'laptop'],
    outboundWorkstation: ['stoke', 'workstation'],
  };
  for (const [name, [fromName, toName]] of Object.entries(pairs)) {
    if (routes[name] && anchors[fromName] && anchors[toName]) routes[name].setAttribute('d', routeBetween(anchors[fromName], anchors[toName], canvas));
  }
  return Object.fromEntries(Object.entries(routes).map(([name, route]) => [name, route.getTotalLength ? route.getTotalLength() : 0]));
}

function setRouteState(route, visible, state) {
  route.toggleAttribute('hidden', !visible);
  route.setAttribute('data-route-state', state || 'idle');
}

function renderCloud(root) {
  root.classList.add('product-visual__js-ready');
  installRoutes(root);
  const route = q(root, '[data-route="ingress"]');
  const outbound = q(root, '[data-route="outboundWorkstation"]');
  if (route) setRouteState(route, true, 'active');
  if (outbound) setRouteState(outbound, true, 'active');
  const resize = () => installRoutes(root);
  if ('ResizeObserver' in window) new ResizeObserver(resize).observe(q(root, '[data-visual-canvas]'));
  else window.addEventListener('resize', resize);
}

function mountInteractive(root) {
  const canvas = q(root, '[data-visual-canvas]');
  const panel = q(root, '[data-visual-panel]');
  const tabs = qa(root, '[role="tab"]');
  const play = q(root, '[data-role="visual-play"]');
  const status = q(root, '[data-visual-status]');
  const request = q(root, '[data-visual-request]');
  const transformLabel = q(root, '[data-visual-transform-label]');
  const transform = q(root, '[data-visual-transform]');
  const decision = q(root, '[data-visual-decision]');
  const rule = q(root, '[data-visual-rule]');
  const packet = q(root, '[data-packet]');
  const routeElements = Object.fromEntries(qa(root, '[data-route]').map((el) => [el.dataset.route, el]));
  const destinations = qa(root, '[data-destination]');
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
  let scenario = root.dataset.scenario && SCENARIOS.includes(root.dataset.scenario) ? root.dataset.scenario : 'privacy';
  let elapsed = DURATIONS[scenario];
  let running = false;
  let paused = false;
  let frameId = 0;
  let lastTime = 0;
  let lastPhase = '';
  let lastScenario = '';
  let generation = 0;
  let routeLengths = {};
  let initialAutoStarted = false;
  let sceneStartPending = false;

  function updateControls(state) {
    const label = running ? 'Pause' : state.finished ? 'Replay' : paused ? 'Resume' : 'Replay';
    const icon = label === 'Pause' ? 'M8 5v14M16 5v14' : 'M8 5v14l11-7L8 5Z';
    play.querySelector('span').textContent = label;
    play.setAttribute('aria-label', `${label} animation`);
    play.querySelector('path').setAttribute('d', icon);
    root.dataset.paused = String(paused);
    root.dataset.playing = String(running);
  }

  function updateDestinations(state) {
    for (const destination of destinations) {
      const name = destination.dataset.destination;
      const selected = state.selectedNode === name;
      const failed = state.failedNode === name;
      destination.dataset.state = failed ? 'failed' : selected ? 'selected' : 'idle';
      const stateLabel = q(destination, '[data-destination-state]');
      const detail = q(destination, '[data-destination-detail]');
      if (stateLabel) stateLabel.textContent = selected ? 'Active' : failed ? 'Failed' : 'Quiet';
      if (detail && name === 'openai') detail.textContent = selected ? 'Selected route' : 'Eligible';
      if (detail && name === 'laptop') detail.textContent = failed ? 'Connection failed' : 'Same configured model';
      if (detail && name === 'workstation') detail.textContent = selected ? 'Selected · same model' : 'Same configured model';
    }
  }

  function updatePacket(state) {
    if (!state.packet) {
      packet.toggleAttribute('hidden', true);
      packet.setAttribute('visibility', 'hidden');
      return;
    }
    const path = routeElements[state.packet.path];
    const length = routeLengths[state.packet.path] || 0;
    packet.toggleAttribute('hidden', !path || !length);
    packet.setAttribute('visibility', path && length ? 'visible' : 'hidden');
    if (!path || !length) return;
    const point = path.getPointAtLength(length * state.packet.progress);
    packet.setAttribute('cx', String(point.x));
    packet.setAttribute('cy', String(point.y));
    packet.setAttribute('data-response', String(state.packet.response));
  }

  function applySceneState(state) {
    root.dataset.scenario = scenario;
    root.dataset.phase = state.phase;
    root.dataset.policy = state.policy;
    panel.setAttribute('aria-labelledby', `tab-${scenario}`);
    if (scenario !== lastScenario) request.innerHTML = COPY[scenario].request;
    const copy = COPY[scenario][state.phase] || COPY[scenario].complete;
    transformLabel.textContent = copy[0];
    transform.innerHTML = copy[1];
    decision.textContent = copy[2];
    rule.textContent = copy[3];
    updateDestinations(state);
    for (const [name, route] of Object.entries(routeElements)) {
      const visible = state.visiblePaths.includes(name);
      const routeState = state.failedPaths.includes(name) ? 'failed' : state.activePaths.includes(name) ? 'active' : 'idle';
      setRouteState(route, visible, routeState);
    }
    routeLengths = installRoutes(root);
    status.textContent = CAPTIONS[state.phase] || CAPTIONS.complete;
    lastPhase = state.phase;
    lastScenario = scenario;
  }

  function render(time) {
    const state = sampleScenario(scenario, time);
    root.dataset.elapsed = String(Math.round(state.elapsed));
    if (state.phase !== lastPhase || scenario !== lastScenario) applySceneState(state);
    updatePacket(state);
    return state;
  }

  function cancel() {
    generation += 1;
    if (frameId) cancelAnimationFrame(frameId);
    frameId = 0;
    running = false;
  }

  function pause() {
    if (!running) return;
    const now = performance.now();
    elapsed = Math.min(DURATIONS[scenario], elapsed + Math.max(0, now - lastTime));
    cancel();
    paused = true;
    const state = render(elapsed);
    updateControls(state);
  }

  function finish() {
    cancel();
    elapsed = DURATIONS[scenario];
    paused = false;
    const state = render(elapsed);
    updateControls(state);
  }

  function tick(now, token) {
    if (!running || token !== generation) return;
    elapsed = Math.min(DURATIONS[scenario], elapsed + Math.max(0, now - lastTime));
    lastTime = now;
    if (elapsed >= DURATIONS[scenario]) {
      finish();
      return;
    }
    const state = render(elapsed);
    frameId = requestAnimationFrame((next) => tick(next, token));
  }

  function beginAnimation(reset = false) {
    if (document.hidden || reduced.matches) return;
    if (reset) {
      elapsed = 0;
      paused = false;
      lastPhase = '';
      lastScenario = '';
    }
    running = true;
    paused = false;
    generation += 1;
    lastTime = performance.now();
    const token = generation;
    const state = render(elapsed);
    updateControls(state);
    frameId = requestAnimationFrame((now) => tick(now, token));
  }

  function playOrPause() {
    if (running) { pause(); return; }
    if (reduced.matches) { finish(); return; }
    if (document.hidden) return;
    beginAnimation(elapsed >= DURATIONS[scenario]);
  }

  function scheduleSceneStart() {
    sceneStartPending = true;
    if (document.hidden || reduced.matches) return;
    const token = generation;
    requestAnimationFrame(() => {
      if (token !== generation || document.hidden || reduced.matches) return;
      sceneStartPending = false;
      beginAnimation(true);
    });
  }

  function choose(next, focus = false) {
    if (!SCENARIOS.includes(next)) return;
    cancel();
    sceneStartPending = false;
    scenario = next;
    elapsed = DURATIONS[scenario];
    paused = false;
    lastPhase = '';
    lastScenario = '';
    tabs.forEach((tab) => {
      const selected = tab.dataset.scenario === scenario;
      tab.setAttribute('aria-selected', String(selected));
      tab.tabIndex = selected ? 0 : -1;
      if (selected && focus) tab.focus();
    });
    const state = render(elapsed);
    updateControls(state);
    if (!reduced.matches) scheduleSceneStart();
  }

  tabs.forEach((tab, index) => {
    tab.addEventListener('click', () => choose(tab.dataset.scenario));
    tab.addEventListener('keydown', (event) => {
      const positions = {ArrowRight: (index + 1) % tabs.length, ArrowLeft: (index + tabs.length - 1) % tabs.length, Home: 0, End: tabs.length - 1};
      if (Object.hasOwn(positions, event.key)) {
        event.preventDefault();
        choose(tabs[positions[event.key]].dataset.scenario, true);
      }
    });
  });
  play.addEventListener('click', playOrPause);
  const onVisibility = () => {
    if (document.hidden) pause();
    else if (!initialAutoStarted) startInitial();
    else if (sceneStartPending) scheduleSceneStart();
  };
  document.addEventListener('visibilitychange', onVisibility);
  window.addEventListener('pagehide', cancel, {once: true});
  const onMotionChange = () => { if (reduced.matches) finish(); else if (sceneStartPending) scheduleSceneStart(); };
  if (reduced.addEventListener) reduced.addEventListener('change', onMotionChange);
  else reduced.addListener(onMotionChange);

  const refreshLayout = () => {
    routeLengths = installRoutes(root);
    updatePacket(sampleScenario(scenario, elapsed));
  };
  if ('ResizeObserver' in window) new ResizeObserver(refreshLayout).observe(canvas);
  else window.addEventListener('resize', refreshLayout);
  if (document.fonts) {
    document.fonts.ready.then(refreshLayout);
    if (document.fonts.addEventListener) document.fonts.addEventListener('loadingdone', refreshLayout);
  }

  function startInitial() {
    if (initialAutoStarted || reduced.matches) {
      initialAutoStarted = true;
      return;
    }
    if (document.hidden) return;
    initialAutoStarted = true;
    beginAnimation(true);
  }

  root.classList.add('product-visual__js-ready');
  tabs.forEach((tab) => {
    const selected = tab.dataset.scenario === scenario;
    tab.setAttribute('aria-selected', String(selected));
    tab.tabIndex = selected ? 0 : -1;
  });
  const initialState = render(elapsed);
  updateControls(initialState);
  routeLengths = installRoutes(root);
  if ('IntersectionObserver' in window) {
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        observer.disconnect();
        startInitial();
      }
    }, {threshold: 0.15});
    observer.observe(canvas);
  } else {
    startInitial();
  }
}

for (const root of document.querySelectorAll('[data-product-visual]')) {
  if (root.dataset.visualMode === 'cloud') renderCloud(root);
  else mountInteractive(root);
}
