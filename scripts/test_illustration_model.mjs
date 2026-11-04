import test from 'node:test';
import assert from 'node:assert/strict';

import {
  DURATIONS,
  SCENARIOS,
  sampleScenario,
} from '../landing/illustration-model.mjs';

const EXPECTED_SCENARIOS = ['routing', 'policy', 'fallback'];
const EXPECTED_DURATIONS = {routing: 4200, policy: 2800, fallback: 5600};
const PATHS = ['ingress', 'a', 'b'];

function timeline(scenario) {
  const duration = DURATIONS[scenario];
  const step = Math.max(1, Math.floor(duration / 40));
  const times = new Set([0, 1, 17, duration - 100, duration - 10, duration - 1, duration, duration + 1, duration + 100, Infinity]);
  for (let elapsed = 0; elapsed <= duration; elapsed += step) times.add(elapsed);
  return [...times].sort((a, b) => (a === Infinity ? 1 : b === Infinity ? -1 : a - b))
    .map(elapsed => ({elapsed, snapshot: sampleScenario(scenario, elapsed)}));
}

function assertSnapshotShape(snapshot, scenario) {
  assert.equal(snapshot.scenario, scenario);
  assert.equal(snapshot.duration, DURATIONS[scenario]);
  assert.equal(typeof snapshot.elapsed, 'number');
  assert.equal(typeof snapshot.finished, 'boolean');
  assert.ok(['pending', 'allowed', 'blocked'].includes(snapshot.policy));
  assert.ok(snapshot.selectedNode === null || snapshot.selectedNode === 'a' || snapshot.selectedNode === 'b');
  assert.ok(snapshot.failedNode === null || snapshot.failedNode === 'a');
  assert.deepEqual(Object.keys(snapshot.paths).sort(), PATHS.sort());
  for (const path of PATHS) {
    assert.equal(typeof snapshot.paths[path], 'number');
    assert.ok(Number.isFinite(snapshot.paths[path]));
    assert.ok(snapshot.paths[path] >= 0 && snapshot.paths[path] <= 1,
      `${scenario} ${path} progress out of bounds`);
  }
  if (snapshot.packet !== null) {
    assert.ok(snapshot.packet && typeof snapshot.packet === 'object');
    assert.ok(PATHS.includes(snapshot.packet.path));
    assert.equal(typeof snapshot.packet.progress, 'number');
    assert.ok(Number.isFinite(snapshot.packet.progress));
    assert.ok(snapshot.packet.progress >= 0 && snapshot.packet.progress <= 1,
      `${scenario} packet progress out of bounds`);
  }
}

function assertNonIncreasing(values, label) {
  for (let index = 1; index < values.length; index += 1) {
    assert.ok(values[index] <= values[index - 1], `${label} increased during reverse travel`);
  }
}

test('exports the fixed scenario and duration contract', () => {
  assert.deepEqual(SCENARIOS, EXPECTED_SCENARIOS);
  assert.deepEqual(DURATIONS, EXPECTED_DURATIONS);
});

test('samples are deterministic across a dense timeline for every scenario', () => {
  for (const scenario of SCENARIOS) {
    for (const {elapsed} of timeline(scenario)) {
      assert.deepEqual(
        sampleScenario(scenario, elapsed),
        sampleScenario(scenario, elapsed),
        `${scenario} at ${String(elapsed)}ms is not deterministic`,
      );
    }
  }
});

test('clamps negative time, rejects NaN and unknown scenarios', () => {
  for (const scenario of SCENARIOS) {
    assert.deepEqual(sampleScenario(scenario, -100), sampleScenario(scenario, 0));
    assert.deepEqual(sampleScenario(scenario, Number.NEGATIVE_INFINITY), sampleScenario(scenario, 0));
    assert.throws(() => sampleScenario(scenario, Number.NaN), TypeError);
  }
  assert.throws(() => sampleScenario('unknown', 0), RangeError);
});

test('keeps snapshot fields and all progress values valid at many times', () => {
  for (const scenario of SCENARIOS) {
    for (const {snapshot} of timeline(scenario)) assertSnapshotShape(snapshot, scenario);
  }
});

test('policy is decided before any upstream path progresses in every scenario', () => {
  for (const scenario of SCENARIOS) {
    for (const {elapsed, snapshot} of timeline(scenario)) {
      const upstreamProgressed = snapshot.paths.a > 0 || snapshot.paths.b > 0;
      assert.ok(!upstreamProgressed || snapshot.policy === 'allowed',
        `${scenario} has upstream progress before policy is allowed at ${elapsed}ms`);
    }
  }
});

test('routing enters ingress first and finishes on the selected a route', () => {
  const samples = timeline('routing');
  const firstIngress = samples.findIndex(({snapshot}) => snapshot.paths.ingress > 0);
  const firstUpstream = samples.findIndex(({snapshot}) => snapshot.paths.a > 0 || snapshot.paths.b > 0);
  assert.ok(firstIngress >= 0, 'routing never progresses ingress');
  assert.ok(firstUpstream > firstIngress, 'routing progresses upstream before ingress');
  for (const {elapsed, snapshot} of samples) {
    assert.equal(snapshot.paths.b, 0, `routing animated excluded b at ${elapsed}ms`);
    assert.notEqual(snapshot.selectedNode, 'b', `routing selected excluded b at ${elapsed}ms`);
    if (snapshot.packet !== null) assert.notEqual(snapshot.packet.path, 'b',
      `routing sent a packet over excluded b at ${elapsed}ms`);
  }

  const final = sampleScenario('routing', DURATIONS.routing);
  assert.equal(final.policy, 'allowed');
  assert.equal(final.selectedNode, 'a');
  assert.equal(final.failedNode, null);
  assert.deepEqual(final.paths, {ingress: 1, a: 1, b: 0});
  assert.equal(final.packet, null);
  assert.equal(final.finished, true);
});

test('routing packet reverses on a and then on ingress near the end', () => {
  const samples = timeline('routing');
  const late = samples.filter(({elapsed}) => elapsed >= DURATIONS.routing * 0.6 && elapsed < DURATIONS.routing);
  const aReturn = late
    .filter(({snapshot}) => snapshot.packet?.path === 'a' && snapshot.paths.a === 1)
    .map(({snapshot}) => snapshot.packet.progress);
  const ingressReturn = late
    .filter(({snapshot}) => snapshot.packet?.path === 'ingress')
    .map(({snapshot}) => snapshot.packet.progress);

  assert.ok(aReturn.length >= 2, 'routing has no sampled reverse packet on a');
  assert.ok(new Set(aReturn).size > 1, 'routing packet on a never moves in reverse');
  assertNonIncreasing(aReturn, 'routing packet on a');
  assert.ok(ingressReturn.length >= 2, 'routing has no sampled reverse packet on ingress near the end');
  assert.ok(new Set(ingressReturn).size > 1, 'routing packet on ingress never moves in reverse');
  assertNonIncreasing(ingressReturn, 'routing packet on ingress');
});

test('policy refusal never selects or progresses an upstream route', () => {
  const samples = timeline('policy');
  assert.ok(samples.some(({snapshot}) => snapshot.policy === 'blocked'), 'policy never becomes blocked');
  for (const {elapsed, snapshot} of samples) {
    assert.equal(snapshot.selectedNode, null, `policy selected a node at ${elapsed}ms`);
    assert.equal(snapshot.failedNode, null, `policy reports a node failure at ${elapsed}ms`);
    assert.equal(snapshot.paths.a, 0, `policy progressed a at ${elapsed}ms`);
    assert.equal(snapshot.paths.b, 0, `policy progressed b at ${elapsed}ms`);
    if (snapshot.packet !== null) assert.equal(snapshot.packet.path, 'ingress',
      `policy sent a packet upstream at ${elapsed}ms`);
  }
  const final = sampleScenario('policy', DURATIONS.policy);
  assert.equal(final.policy, 'blocked');
  assert.equal(final.packet, null);
  assert.equal(final.finished, true);
});

test('fallback only starts b after a has failed and b is selected', () => {
  const samples = timeline('fallback');
  assert.ok(samples.some(({snapshot}) => snapshot.failedNode === 'a'), 'fallback never records a failure');
  assert.ok(samples.some(({snapshot}) => snapshot.selectedNode === 'b'), 'fallback never selects b');
  for (const {elapsed, snapshot} of samples) {
    if (snapshot.paths.b > 0) {
      assert.equal(snapshot.failedNode, 'a', `fallback progressed b before a failed at ${elapsed}ms`);
      assert.equal(snapshot.selectedNode, 'b', `fallback progressed b before b was selected at ${elapsed}ms`);
    }
  }
  const failure = samples.find(({snapshot}) => snapshot.failedNode === 'a');
  assert.ok(failure.snapshot.paths.a > 0, 'fallback failure occurred without attempting a');

  const final = sampleScenario('fallback', DURATIONS.fallback);
  assert.equal(final.policy, 'allowed');
  assert.equal(final.failedNode, 'a');
  assert.equal(final.selectedNode, 'b');
  assert.deepEqual(final.paths, {ingress: 1, a: 1, b: 1});
  assert.equal(final.packet, null);
  assert.equal(final.finished, true);
});

test('literal decision boundaries retain policy and failure ordering', () => {
  const admitted = sampleScenario('routing', 1150);
  assert.equal(admitted.policy, 'allowed');
  assert.equal(admitted.selectedNode, 'a');
  assert.deepEqual(admitted.paths, {ingress: 1, a: 0, b: 0});
  const blocked = sampleScenario('policy', 1150);
  assert.equal(blocked.policy, 'blocked');
  assert.equal(blocked.selectedNode, null);
  const failed = sampleScenario('fallback', 2200);
  assert.equal(failed.failedNode, 'a');
  assert.equal(failed.selectedNode, 'a');
  assert.equal(failed.paths.b, 0);
  const next = sampleScenario('fallback', 2500);
  assert.equal(next.failedNode, 'a');
  assert.equal(next.selectedNode, 'b');
  assert.equal(next.paths.b, 0);
});

test('finished snapshots are static and contain no packet', () => {
  for (const scenario of SCENARIOS) {
    const duration = DURATIONS[scenario];
    const final = sampleScenario(scenario, duration);
    for (const elapsed of [duration, duration + 1, duration * 2, Number.POSITIVE_INFINITY]) {
      const snapshot = sampleScenario(scenario, elapsed);
      assert.equal(snapshot.finished, true, `${scenario} is not finished at ${String(elapsed)}ms`);
      assert.equal(snapshot.packet, null, `${scenario} has a packet after finishing`);
      assert.equal(snapshot.elapsed, duration, `${scenario} did not clamp elapsed at duration`);
      assert.deepEqual(snapshot, final, `${scenario} final state changed after completion`);
    }
  }
});
