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
        setMenuOpen(!menu.classList.contains('is-open'), false);
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
      var handleMenuMediaChange = function (event) {
        if (!event.matches) setMenuOpen(false, false);
      };
      if (mobileMenu.addEventListener) mobileMenu.addEventListener('change', handleMenuMediaChange);
      else mobileMenu.addListener(handleMenuMediaChange);
      header.classList.add('nav-enhanced');
    } catch (error) {
      // Leave the unenhanced navigation visible if setup cannot finish.
    }
  }

  var preview = document.getElementById('product-preview');
  var watch = document.querySelector('a[href="#demo"]');
  if (preview && watch) {
    watch.addEventListener('click', function (event) {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button > 0) return;
      var playback = preview.play();
      if (playback && playback.catch) playback.catch(function () { /* Native controls remain available. */ });
    });
  }

  document.querySelectorAll('[data-year]').forEach(function (node) {
    node.textContent = String(new Date().getFullYear());
  });
}());
