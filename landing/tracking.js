(function () {
  'use strict';

  function source() {
    return new URLSearchParams(window.location.search).get('utm_source') || '';
  }

  function track(event, detail) {
    var payload = JSON.stringify({
      event: event,
      path: window.location.pathname,
      source: source(),
      target: detail || ''
    });
    if (navigator.sendBeacon) {
      navigator.sendBeacon("/events", new Blob([payload], { type: 'application/json' }));
    } else {
      fetch('/events', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: payload,
        keepalive: true
      }).catch(function () {});
    }
  }

  document.addEventListener('click', function (event) {
    var target = event.target.closest('[data-track], a[href^="https://github.com/Ozperium/stoke"]');
    if (!target) return;
    track(target.dataset.track || 'github_clicked', target.href || '');
  });

  var install = document.getElementById('install');
  if (install && 'IntersectionObserver' in window) {
    var seen = false;
    new IntersectionObserver(function (entries, observer) {
      if (seen || !entries[0].isIntersecting) return;
      seen = true;
      track('install_section_viewed');
      observer.disconnect();
    }, { threshold: 0.25 }).observe(install);
  }
})();
