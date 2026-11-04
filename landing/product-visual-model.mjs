export const SCENARIOS = Object.freeze(['privacy', 'budget', 'local']);

export const DURATIONS = Object.freeze({
  privacy: 5600,
  budget: 2600,
  local: 6500,
});

const TIMELINES = Object.freeze({
  privacy: Object.freeze([
    ['ingress', 0, 850],
    ['inspect', 850, 1550],
    ['mask', 1550, 2350],
    ['dispatch', 2350, 3500],
    ['response', 3500, 4300],
    ['return', 4300, 5600],
  ]),
  budget: Object.freeze([
    ['ingress', 0, 800],
    ['inspect', 800, 1500],
    ['stopped', 1500, 2600],
  ]),
  local: Object.freeze([
    ['ingress', 0, 850],
    ['inspect', 850, 1500],
    ['dispatch-laptop', 1500, 2600],
    ['connection-failed', 2600, 3400],
    ['permitted-retry', 3400, 4550],
    ['response', 4550, 5350],
    ['return', 5350, 6500],
  ]),
});

const clamp = (value, min, max) => Math.max(min, Math.min(max, value));
const between = (value, start, end) => clamp((value - start) / (end - start), 0, 1);
const freezePacket = (path, progress, response = false) => Object.freeze({
  path,
  progress: clamp(progress, 0, 1),
  response,
});

export function phaseAt(scenario, elapsedMs) {
  validateScenario(scenario);
  const elapsed = clampElapsed(scenario, elapsedMs);
  if (elapsed >= DURATIONS[scenario]) return 'complete';
  const mark = TIMELINES[scenario].find(([, start, end]) => elapsed >= start && elapsed < end);
  return mark ? mark[0] : 'complete';
}

function validateScenario(scenario) {
  if (!SCENARIOS.includes(scenario)) throw new RangeError(`Unknown product visual scenario: ${scenario}`);
}

function clampElapsed(scenario, elapsedMs) {
  if (typeof elapsedMs !== 'number' || !Number.isFinite(elapsedMs)) {
    throw new TypeError('Elapsed time must be a finite number');
  }
  return clamp(elapsedMs, 0, DURATIONS[scenario]);
}

function pathState() {
  return {
    ingress: false,
    outboundOpenai: false,
    outboundLaptop: false,
    outboundWorkstation: false,
  };
}

// This is a deterministic presentation model, not live gateway state.
export function sampleScenario(scenario, elapsedMs) {
  validateScenario(scenario);
  const elapsed = clampElapsed(scenario, elapsedMs);
  const phase = phaseAt(scenario, elapsed);
  const paths = pathState();
  const state = {
    scenario,
    elapsed,
    duration: DURATIONS[scenario],
    phase,
    finished: phase === 'complete',
    policy: 'pending',
    selectedNode: null,
    failedNode: null,
    visiblePaths: [],
    activePaths: [],
    failedPaths: [],
    packet: null,
    paths,
  };

  // Retain the incoming connection in completed and reduced-motion scenes.
  paths.ingress = true;
  state.visiblePaths.push('ingress');
  if (phase === 'ingress') {
    state.packet = freezePacket('ingress', between(elapsed, 0, TIMELINES[scenario][0][2]));
  }

  if (scenario === 'privacy') {
    if (phase === 'inspect' || phase === 'mask' || phase === 'dispatch' || phase === 'response' || phase === 'return' || phase === 'complete') {
      state.policy = 'allowed';
    }
    if (phase === 'dispatch') {
      paths.outboundOpenai = true;
      state.visiblePaths.push('outboundOpenai');
      state.activePaths.push('outboundOpenai');
      state.selectedNode = 'openai';
      state.packet = freezePacket('outboundOpenai', between(elapsed, 2350, 3500));
    } else if (phase === 'response') {
      paths.outboundOpenai = true;
      state.visiblePaths.push('outboundOpenai');
      state.activePaths.push('outboundOpenai');
      state.selectedNode = 'openai';
      state.packet = freezePacket('outboundOpenai', 1 - between(elapsed, 3500, 4300), true);
    } else if (phase === 'return') {
      paths.outboundOpenai = true;
      state.visiblePaths.push('outboundOpenai', 'ingress');
      state.activePaths.push('outboundOpenai');
      state.selectedNode = 'openai';
      state.packet = freezePacket('ingress', 1 - between(elapsed, 4300, 5200), true);
    } else if (phase === 'complete') {
      paths.outboundOpenai = true;
      state.visiblePaths.push('outboundOpenai');
      state.activePaths.push('outboundOpenai');
      state.selectedNode = 'openai';
    }
    return finalize(state);
  }

  if (scenario === 'budget') {
    if (phase === 'inspect') state.policy = 'pending';
    if (phase === 'stopped' || phase === 'complete') {
      state.policy = 'blocked';
      state.visiblePaths = state.visiblePaths.filter(path => path === 'ingress');
    }
    return finalize(state);
  }

  // Local fallback deliberately keeps the failed Laptop visible and only retries
  // Workstation after the connection fails, before any response exists.
  if (phase === 'inspect' || phase === 'dispatch-laptop' || phase === 'connection-failed' || phase === 'permitted-retry' || phase === 'response' || phase === 'return' || phase === 'complete') {
    state.policy = 'allowed';
  }
  if (phase === 'dispatch-laptop') {
    paths.outboundLaptop = true;
    state.visiblePaths.push('outboundLaptop');
    state.activePaths.push('outboundLaptop');
    state.selectedNode = 'laptop';
    state.packet = freezePacket('outboundLaptop', between(elapsed, 1500, 2600));
  } else if (phase === 'connection-failed') {
    paths.outboundLaptop = true;
    state.visiblePaths.push('outboundLaptop');
    state.failedPaths.push('outboundLaptop');
    state.failedNode = 'laptop';
  } else if (phase === 'permitted-retry') {
    paths.outboundLaptop = true;
    paths.outboundWorkstation = true;
    state.visiblePaths.push('outboundLaptop', 'outboundWorkstation');
    state.failedPaths.push('outboundLaptop');
    state.activePaths.push('outboundWorkstation');
    state.failedNode = 'laptop';
    state.selectedNode = 'workstation';
    state.packet = freezePacket('outboundWorkstation', between(elapsed, 3400, 4550));
  } else if (phase === 'response') {
    paths.outboundLaptop = true;
    paths.outboundWorkstation = true;
    state.visiblePaths.push('outboundLaptop', 'outboundWorkstation');
    state.failedPaths.push('outboundLaptop');
    state.activePaths.push('outboundWorkstation');
    state.failedNode = 'laptop';
    state.selectedNode = 'workstation';
    state.packet = freezePacket('outboundWorkstation', 1 - between(elapsed, 4550, 5350), true);
  } else if (phase === 'return' || phase === 'complete') {
    paths.outboundLaptop = true;
    paths.outboundWorkstation = true;
    state.visiblePaths.push('outboundLaptop', 'outboundWorkstation');
    if (phase === 'return') state.visiblePaths.push('ingress');
    state.failedPaths.push('outboundLaptop');
    state.activePaths.push('outboundWorkstation');
    state.failedNode = 'laptop';
    state.selectedNode = 'workstation';
    if (phase === 'return') state.packet = freezePacket('ingress', 1 - between(elapsed, 5350, 6500), true);
  }
  return finalize(state);
}

function finalize(state) {
  state.visiblePaths = Object.freeze([...new Set(state.visiblePaths)]);
  state.activePaths = Object.freeze([...new Set(state.activePaths)]);
  state.failedPaths = Object.freeze([...new Set(state.failedPaths)]);
  state.paths = Object.freeze(state.paths);
  return Object.freeze(state);
}

export {TIMELINES};
export const sampleProductVisual = sampleScenario;
