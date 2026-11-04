(() => {
  'use strict';

  document.documentElement.classList.add('js-ready');
  const header = document.querySelector('.site-nav');
  const toggle = document.querySelector('[data-nav-toggle]');
  const menu = document.querySelector('[data-nav-menu]');
  if (header && toggle && menu) {
    const setMenu = (open, restoreFocus) => {
      menu.classList.toggle('is-open', open);
      toggle.setAttribute('aria-expanded', String(open));
      if (!open && restoreFocus) toggle.focus();
    };
    toggle.addEventListener('click', () => setMenu(!menu.classList.contains('is-open'), false));
    menu.addEventListener('click', (event) => {
      if (event.target.closest('a')) setMenu(false, false);
    });
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && menu.classList.contains('is-open')) {
        event.preventDefault();
        setMenu(false, true);
      }
    });
    const media = window.matchMedia('(min-width: 701px)');
    const reset = (event) => { if (event.matches) setMenu(false, false); };
    if (media.addEventListener) media.addEventListener('change', reset);
    else media.addListener(reset);
  }

  const dialog = document.querySelector('[data-film-open]') && document.getElementById('film-dialog');
  const openButton = document.querySelector('[data-film-open]');
  const closeButton = dialog && dialog.querySelector('[data-film-close]');
  const video = dialog && dialog.querySelector('video');
  let returnFocus;
  if (dialog && openButton && typeof dialog.showModal === 'function') {
    openButton.addEventListener('click', (event) => {
      event.preventDefault();
      returnFocus = openButton;
      dialog.showModal();
      closeButton.focus();
      if (video) video.play().catch(() => {});
    });
    closeButton.addEventListener('click', () => dialog.close());
    dialog.addEventListener('click', (event) => {
      if (event.target === dialog) dialog.close();
    });
    dialog.addEventListener('close', () => {
      if (video) video.pause();
      if (returnFocus) returnFocus.focus();
    });
  }

  const missingPath = document.querySelector('[data-missing-path]');
  if (missingPath) missingPath.textContent = window.location.pathname.slice(0, 120);

  document.querySelectorAll('[data-year]').forEach((node) => {
    node.textContent = String(new Date().getFullYear());
  });
})();
