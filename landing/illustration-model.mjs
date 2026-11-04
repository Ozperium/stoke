export const SCENARIOS = Object.freeze(['routing', 'policy', 'fallback']);
export const DURATIONS = Object.freeze({routing: 4200, policy: 2800, fallback: 5600});
const progress = (time, start, end) => Math.max(0, Math.min(1, (time - start) / (end - start)));
const packet = (time, path, start, end, reverse = false) => time >= start && time < end
  ? {path, progress: reverse ? 1 - progress(time, start, end) : progress(time, start, end)} : null;

// A deterministic illustration, not a simulation of live gateway state.
export function sampleScenario(scenario, elapsedMs) {
  if (!SCENARIOS.includes(scenario)) throw new RangeError('Unknown illustration scenario');
  if (typeof elapsedMs !== 'number' || Number.isNaN(elapsedMs)) throw new TypeError('Elapsed time must be a number');
  const duration = DURATIONS[scenario];
  const elapsed = Math.max(0, Math.min(duration, elapsedMs));
  const finished = elapsed === duration;
  const state = {scenario, elapsed, duration, finished, phase: 'incoming', policy: 'pending',
    selectedNode: null, failedNode: null, paths: {ingress: progress(elapsed, 0, 850), a: 0, b: 0},
    packet: packet(elapsed, 'ingress', 0, 850)};
  if (elapsed >= 850) state.phase = 'checking';
  if (scenario === 'policy') {
    if (elapsed >= 1150) { state.policy = 'blocked'; state.phase = 'blocked'; }
    state.packet ||= packet(elapsed, 'ingress', 1700, 2500, true);
    if (state.packet && elapsed >= 1700) state.phase = 'refusal-return';
  } else {
    if (elapsed >= 1150) { state.policy = 'allowed'; state.selectedNode = 'a'; }
    state.paths.a = progress(elapsed, 1250, scenario === 'routing' ? 2150 : 2050);
    if (elapsed >= 1250) state.phase = 'routing-a';
    state.packet ||= packet(elapsed, 'a', 1250, scenario === 'routing' ? 2150 : 2050);
    if (scenario === 'routing') {
      if (elapsed >= 2150) state.phase = 'answering';
      if (elapsed >= 2600) state.phase = 'answer-return';
      state.packet ||= packet(elapsed, 'a', 2600, 3350, true) || packet(elapsed, 'ingress', 3350, 4200, true);
    } else {
      if (elapsed >= 2200) { state.failedNode = 'a'; state.phase = 'connect-failed'; }
      if (elapsed >= 2500) { state.selectedNode = 'b'; state.phase = 'routing-b'; }
      state.paths.b = progress(elapsed, 2500, 3500);
      state.packet ||= packet(elapsed, 'b', 2500, 3500);
      if (elapsed >= 3500) state.phase = 'answering';
      if (elapsed >= 3900) state.phase = 'answer-return';
      state.packet ||= packet(elapsed, 'b', 3900, 4700, true) || packet(elapsed, 'ingress', 4700, 5600, true);
    }
  }
  if (finished) { state.packet = null; state.phase = scenario === 'policy' ? 'blocked' : 'answered'; }
  return state;
}
