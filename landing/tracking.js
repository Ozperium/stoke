(function () {
  'use strict';

  var OPT_OUT_KEY = 'stoke_analytics_opt_out';
  var STORAGE_PROBE_KEY = 'stoke_analytics_storage_probe';
  var PRODUCTION_HOSTS = new Set(['stokegate.com', 'www.stokegate.com']);
  var EVENTS = new Set([
    '$pageview',
    'cloud_viewed',
    'pilot_form_opened',
    'install_cta_clicked',
    'install_section_viewed',
    'github_clicked',
    'panel_demo_clicked',
    'local_capacity_cta_clicked',
    'guides_clicked',
    'cloud_invitation_requested'
  ]);
  var PUBLIC_PATHS = new Set([
    '/',
    '/cloud/',
    '/contact/',
    '/gateway/',
    '/guides/',
    '/guides/block-prompt-injection-at-the-gateway/',
    '/guides/cache-identity/',
    '/guides/cache-policy/',
    '/guides/cap-claude-code-api-spend/',
    '/guides/expose-ollama-to-your-network-safely/',
    '/guides/optional-headroom/',
    '/guides/run-ollama-across-two-machines/',
    '/guides/stop-a-runaway-ai-agent/',
    '/guides/use-codex-and-ollama-with-stoke/',
    '/join/',
    '/legal/',
    '/security/',
    '/use-cases/',
    '/use-cases/agent-spend/',
    '/use-cases/local-models/',
    '/use-cases/node-failover/',
    '/use-cases/private-data/',
    '/use-cases/private-model-access/',
    '/use-cases/secret-redaction/'
  ]);
  var SOURCES = new Set(['cloud', 'direct', 'email', 'footer', 'github', 'hero', 'nav', 'product', 'social']);
  var TARGETS = new Set(['cloud', 'contact', 'gateway', 'github', 'install', 'join', 'legal']);
  var ONE_TIME_EVENTS = new Set(['$pageview', 'cloud_viewed', 'pilot_form_opened', 'install_section_viewed']);
  var UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
  var anonymousId = '';
  var sessionOptedOut = false;
  var storageWriteable = null;
  var sentOnce = new Set();

  function allowedValue(value, allowed) {
    return typeof value === 'string' && allowed.has(value) ? value : '';
  }

  function validUuid(value) {
    return typeof value === 'string' && UUID_V4.test(value);
  }

  function configured() {
    return window.location.protocol === 'https:' && PRODUCTION_HOSTS.has(window.location.hostname);
  }

  function privacySignal(value) {
    return value === true || value === 1 || value === '1' || value === 'yes';
  }

  function browserOptedOut() {
    return privacySignal(navigator.doNotTrack)
      || privacySignal(window.doNotTrack)
      || privacySignal(navigator.msDoNotTrack)
      || privacySignal(window.msDoNotTrack)
      || privacySignal(navigator.globalPrivacyControl)
      || privacySignal(window.globalPrivacyControl);
  }

  function localOptedOut() {
    if (sessionOptedOut) return true;
    try {
      if (!storageIsWriteable()) return true;
      return window.localStorage.getItem(OPT_OUT_KEY) === '1';
    } catch (_error) {
      return true;
    }
  }

  function storageIsWriteable() {
    if (storageWriteable !== null) return storageWriteable;
    try {
      if (!window.localStorage) return false;
      window.localStorage.setItem(STORAGE_PROBE_KEY, '1');
      window.localStorage.removeItem(STORAGE_PROBE_KEY);
      storageWriteable = true;
    } catch (_error) {
      storageWriteable = false;
    }
    return storageWriteable;
  }

  function allowed() {
    return configured() && !browserOptedOut() && !localOptedOut();
  }

  function publicPath() {
    return allowedValue(window.location.pathname, PUBLIC_PATHS);
  }

  function source() {
    try {
      var value = new URLSearchParams(window.location.search).get('utm_source') || '';
      return allowedValue(value, SOURCES);
    } catch (_error) {
      return '';
    }
  }

  function targetName(value) {
    return allowedValue(value, TARGETS);
  }

  function getAnonymousId() {
    if (!allowed()) return '';
    if (!anonymousId) {
      try {
        anonymousId = window.crypto && typeof window.crypto.randomUUID === 'function'
          ? window.crypto.randomUUID()
          : '';
      } catch (_error) {
        anonymousId = '';
      }
      if (!validUuid(anonymousId)) anonymousId = '';
    }
    return anonymousId;
  }

  function send(event, detail, once) {
    if (!EVENTS.has(event) || !publicPath() || !allowed()) return;
    if (once && sentOnce.has(event)) return;
    var id = getAnonymousId();
    if (!id || !allowed()) return;
    var payload = JSON.stringify({
      event: event,
      path: publicPath(),
      source: source(),
      target: targetName(detail),
      analytics_id: id
    });

    try {
      if (navigator.sendBeacon && navigator.sendBeacon("/events", new Blob([payload], { type: 'application/json' }))) {
        if (once) sentOnce.add(event);
        return;
      }
    } catch (_error) {}

    if (!allowed()) return;
    try {
      fetch('/events', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: payload,
        keepalive: true,
        credentials: 'same-origin'
      }).catch(function () {});
      if (once) sentOnce.add(event);
    } catch (_error) {}
  }

  function adoptTransferredId() {
    if (window.location.pathname !== '/join/' || !window.location.hash.startsWith('#stoke-analytics=')) return;
    var transferred = window.location.hash.slice('#stoke-analytics='.length);
    try {
      window.history.replaceState(null, '', window.location.pathname + window.location.search);
    } catch (_error) {
      return;
    }
    if (validUuid(transferred) && allowed()) anonymousId = transferred;
  }

  function decorateJoinLink(anchor) {
    if (!anchor || window.location.pathname === '/join/' || !allowed()) return;
    var id = getAnonymousId();
    if (!id || !allowed()) return;
    try {
      var url = new URL(anchor.getAttribute('href') || anchor.href, window.location.href);
      if (url.origin !== window.location.origin || url.pathname !== '/join/') return;
      anchor.setAttribute('href', window.location.origin + '/join/#stoke-analytics=' + id);
    } catch (_error) {}
  }

  function updatePrivacyControls() {
    var optOut = document.querySelector('[data-analytics-opt-out]');
    var enable = document.querySelector('[data-analytics-enable]');
    var status = document.querySelector('[data-analytics-status]');
    if (!optOut || !enable || !status) return;
    var optedOut = localOptedOut();
    optOut.hidden = optedOut;
    enable.hidden = !optedOut;
    status.textContent = optedOut || browserOptedOut()
      ? 'Optional analytics is off.'
      : 'Optional analytics is on.';
  }

  function setOptOut(value) {
    if (value) {
      sessionOptedOut = true;
      anonymousId = '';
    }
    try {
      if (!window.localStorage) return false;
      if (value) window.localStorage.setItem(OPT_OUT_KEY, '1');
      else window.localStorage.removeItem(OPT_OUT_KEY);
      storageWriteable = true;
      if (!value) sessionOptedOut = false;
      anonymousId = '';
      updatePrivacyControls();
      return true;
    } catch (_error) {
      storageWriteable = false;
      updatePrivacyControls();
      return false;
    }
  }

  adoptTransferredId();
  window.stokeAnalytics = Object.freeze({
    getAnonymousId: getAnonymousId,
    isOptedOut: localOptedOut,
    optOut: function () { return setOptOut(true); },
    enable: function () { return setOptOut(false); },
    track: function (event, detail) { send(event, detail, ONE_TIME_EVENTS.has(event)); }
  });

  document.addEventListener('click', function (event) {
    var anchor = event.target.closest('a[href]');
    decorateJoinLink(anchor);

    var target = event.target.closest('[data-track], a[href^="https://github.com/Ozperium/stoke"]');
    if (!target) return;
    var eventName = target.dataset.track || 'github_clicked';
    send(eventName, eventName === 'github_clicked' ? 'github' : target.dataset.trackTarget, false);
  });

  var install = document.getElementById('install');
  if (install && 'IntersectionObserver' in window) {
    var seen = false;
    new IntersectionObserver(function (entries, observer) {
      if (seen || !entries[0].isIntersecting) return;
      seen = true;
      send('install_section_viewed', '', true);
      observer.disconnect();
    }, { threshold: 0.25 }).observe(install);
  }

  var path = publicPath();
  send('$pageview', '', true);
  if (path === '/cloud/') send('cloud_viewed', '', true);
  if (path === '/join/' && document.getElementById('waitlist-form')) send('pilot_form_opened', '', true);

  var optOutControl = document.querySelector('[data-analytics-opt-out]');
  var enableControl = document.querySelector('[data-analytics-enable]');
  if (optOutControl) optOutControl.addEventListener('click', function () { setOptOut(true); });
  if (enableControl) enableControl.addEventListener('click', function () { setOptOut(false); });
  updatePrivacyControls();
})();
