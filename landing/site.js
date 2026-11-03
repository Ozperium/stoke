(function () {
  'use strict';

  var header = document.querySelector('.site-nav');
  var toggle = document.querySelector('[data-nav-toggle]');
  var menu = document.querySelector('[data-nav-menu]');
  if (header && toggle && menu) {
    function setMenuOpen(open, restoreFocus) {
      menu.classList.toggle('is-open', open);
      toggle.setAttribute('aria-expanded', String(open));
      document.body.classList.toggle('is-locked', open);
      if (!open && restoreFocus) toggle.focus();
    }
    try {
      toggle.addEventListener('click', function () {
        var open = !menu.classList.contains('is-open');
        setMenuOpen(open, false);
      });
      menu.addEventListener('click', function (event) {
        if (event.target.closest('a')) setMenuOpen(false, false);
      });
      document.addEventListener('keydown', function (event) {
        if (event.key === 'Escape' && menu.classList.contains('is-open')) {
          event.preventDefault();
          setMenuOpen(false, true);
        }
      });
      var mobileMenu = window.matchMedia('(max-width: 720px)');
      mobileMenu.addEventListener('change', function (event) {
        if (!event.matches) setMenuOpen(false, false);
      });
      header.classList.add('nav-enhanced');
    } catch (error) {
      // Leave the unenhanced navigation visible if setup cannot finish.
    }
  }

  var flow = document.querySelector('[data-flow-demo]');
  if (flow) {
    var tabs = Array.prototype.slice.call(flow.querySelectorAll('[data-flow-scene]'));
    var label = flow.querySelector('[data-flow-label]');
    var status = flow.querySelector('[data-flow-status]');
    var panel = flow.querySelector('[data-flow-panel]');
    var scenes = [
      { label: 'illustrative route', status: 'request → Stoke → owned Mac → workspace', active: 0 },
      { label: 'illustrative guardrail', status: 'policy checked before an upstream call', active: 1 },
      { label: 'illustrative portability', status: 'same gateway shape wherever you work', active: 2 }
    ];
    function setScene(index) {
      var scene = scenes[index];
      tabs.forEach(function (tab, tabIndex) {
        var selected = tabIndex === index;
        tab.classList.toggle('is-active', selected);
        tab.setAttribute('aria-selected', String(selected));
        tab.setAttribute('tabindex', selected ? '0' : '-1');
      });
      label.textContent = scene.label;
      status.textContent = scene.status;
      if (panel) panel.setAttribute('aria-labelledby', tabs[index].id);
      flow.setAttribute('data-active-scene', String(scene.active));
    }
    tabs.forEach(function (tab, index) {
      tab.addEventListener('click', function () { setScene(index); });
      tab.addEventListener('keydown', function (event) {
        if (event.key !== 'ArrowRight' && event.key !== 'ArrowLeft' && event.key !== 'Home' && event.key !== 'End') return;
        event.preventDefault();
        var next;
        if (event.key === 'Home') next = 0;
        else if (event.key === 'End') next = tabs.length - 1;
        else next = event.key === 'ArrowRight' ? (index + 1) % tabs.length : (index - 1 + tabs.length) % tabs.length;
        tabs[next].focus();
        setScene(next);
      });
    });
    setScene(0);
  }

  document.querySelectorAll('[data-year]').forEach(function (node) {
    node.textContent = String(new Date().getFullYear());
  });
}());
