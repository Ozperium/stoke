import {sampleScenario} from './product-visual-model.mjs';

// The illustration exposes rounded milliseconds and an authoritative phase.
// Avoid dispatching/retrying half a millisecond before that phase changes.
export function presentationState(scenario, elapsed, phase) {
  const current = sampleScenario(scenario, elapsed);
  if (phase && current.phase !== phase && elapsed > 0) {
    const previous = sampleScenario(scenario, Math.max(0, elapsed - 1));
    if (previous.phase === phase) return previous;
  }
  return current;
}

export function wakeDistances(packet, length) {
  if (!Number.isFinite(length) || length <= 0) return [];
  if (!packet || !Number.isFinite(packet.progress) || packet.progress < 0 || packet.progress > 1) return [];
  const head = packet.progress * length;
  const direction = packet.response ? 1 : -1;
  return Array.from({length: 13}, (_, i) => head + direction * i * 3.3)
    .filter(distance => distance >= 0 && distance <= length);
}
