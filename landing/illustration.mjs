import {SCENARIOS, DURATIONS, sampleScenario} from './illustration-model.mjs';

const panel = document.querySelector('[data-illustration]');
if (panel) {
  const tabs = [...panel.querySelectorAll('[data-scenario]')];
  const play = panel.querySelector('[data-play-example]');
  const status = panel.querySelector('[data-story-status]');
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  const views = [...panel.querySelectorAll('[data-layout]')].map(svg => ({svg,
    paths: Object.fromEntries([...svg.querySelectorAll('[data-path]')].map(path => [path.dataset.path, {path, length: path.getTotalLength()}])),
    packet: svg.querySelector('[data-packet]')}));
  const captions = {
    incoming: 'A request reaches Stoke.', checking: 'Policy is checked first.', 'routing-a': 'An eligible node is selected.',
    answering: 'The model prepares an answer.', 'answer-return': 'The answer returns to your app.',
    blocked: 'Blocked before any model is called.', 'refusal-return': 'The refusal returns to your app.',
    'connect-failed': 'The first connection failed.', 'routing-b': 'Another permitted node is selected.', answered: 'The answer reaches your app.'
  };
  let scenario = 'routing';
  let elapsed = DURATIONS[scenario];
  let running = false;
  let frameId = 0;
  let startedAt = 0;
  let lastPhase = '';
  let hasPlayed = false;

  function render(time) {
    const state = sampleScenario(scenario, time);
    panel.dataset.currentScenario = scenario;
    panel.querySelector('[role=tabpanel]').setAttribute('aria-labelledby', `tab-${scenario}`);
    panel.dataset.phase = state.phase;
    panel.dataset.policy = state.policy;
    panel.dataset.selectedNode = state.selectedNode || '';
    panel.dataset.failedNode = state.failedNode || '';
    panel.dataset.elapsed = String(Math.round(state.elapsed));
    for (const view of views) {
      for (const [name, item] of Object.entries(view.paths)) {
        const value = state.paths[name];
        item.path.setAttribute('stroke-dasharray', String(item.length));
        item.path.setAttribute('stroke-dashoffset', String(item.length * (1 - value)));
        item.path.setAttribute('opacity', value > 0 ? '1' : '0');
        item.path.dataset.active = String(value > 0);
        item.path.dataset.failed = String(name === state.failedNode);
      }
      view.svg.querySelector('[data-policy-art]').dataset.policy = state.policy;
      view.svg.querySelectorAll('[data-node]').forEach(node => {
        node.dataset.nodeState = node.dataset.node === state.failedNode ? 'failed' : node.dataset.node === state.selectedNode ? 'selected' : 'idle';
      });
      const moving = state.packet && view.paths[state.packet.path];
      view.packet.setAttribute('visibility', moving ? 'visible' : 'hidden');
      if (moving) {
        const point = moving.path.getPointAtLength(moving.length * state.packet.progress);
        view.packet.setAttribute('cx', String(point.x)); view.packet.setAttribute('cy', String(point.y));
      }
    }
    if (state.phase !== lastPhase) {status.textContent = captions[state.phase]; lastPhase = state.phase;}
    play.textContent = running ? 'Pause' : state.finished ? (reduced.matches ? 'Show example' : hasPlayed ? 'Replay example' : 'Play example') : 'Resume';
    play.setAttribute('aria-label', `${running ? 'Pause' : state.finished ? 'Play' : 'Resume'} ${scenario} example`);
    play.setAttribute('aria-pressed', String(running));
    panel.dataset.playing = String(running);
  }

  function cancel() {cancelAnimationFrame(frameId); frameId = 0; running = false;}
  function pause() {
    if (!running) return;
    elapsed = Math.min(DURATIONS[scenario], performance.now() - startedAt);
    cancel(); render(elapsed);
  }
  function tick(now) {
    if (!running) return;
    elapsed = Math.min(DURATIONS[scenario], now - startedAt);
    if (elapsed >= DURATIONS[scenario]) cancel();
    render(elapsed);
    if (running) frameId = requestAnimationFrame(tick);
  }
  function choose(next, focus = false) {
    if (!SCENARIOS.includes(next)) return;
    cancel(); scenario = next; elapsed = DURATIONS[scenario]; lastPhase = ''; hasPlayed = false;
    tabs.forEach(tab => {const selected = tab.dataset.scenario === scenario; tab.setAttribute('aria-selected', String(selected)); tab.tabIndex = selected ? 0 : -1; if (selected && focus) tab.focus();});
    render(elapsed);
  }
  tabs.forEach((tab, index) => {
    tab.addEventListener('click', () => choose(tab.dataset.scenario));
    tab.addEventListener('keydown', event => {
      const positions = {ArrowRight:(index + 1) % tabs.length, ArrowLeft:(index + tabs.length - 1) % tabs.length, Home:0, End:tabs.length - 1};
      if (Object.hasOwn(positions, event.key)) {event.preventDefault(); choose(tabs[positions[event.key]].dataset.scenario, true);}
    });
  });
  play.addEventListener('click', () => {
    if (running) {pause(); return;}
    if (reduced.matches) {elapsed = DURATIONS[scenario]; render(elapsed); return;}
    if (elapsed >= DURATIONS[scenario]) elapsed = 0;
    hasPlayed = true; running = true; startedAt = performance.now() - elapsed;
    render(elapsed); frameId = requestAnimationFrame(tick);
  });
  reduced.addEventListener('change', () => {if (reduced.matches) {cancel(); elapsed = DURATIONS[scenario]; render(elapsed);}});
  document.addEventListener('visibilitychange', () => {if (document.hidden) pause();});
  window.addEventListener('pagehide', cancel);
  choose('routing'); panel.classList.add('illustration-ready');
}
