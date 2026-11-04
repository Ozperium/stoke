(() => {
  'use strict';

  const form = document.querySelector('#waitlist-form');
  if (!form) return;

  const endpoint = '/api/waitlist';
  const fields = form.querySelector('[data-waitlist-fields]');
  const submit = form.querySelector('[data-waitlist-submit]');
  const status = form.querySelector('[data-waitlist-status]');
  const error = form.querySelector('[data-waitlist-error]');
  const note = form.querySelector('[data-waitlist-note]');
  const noticeVersion = form.querySelector('[data-notice-version]');
  const noticeInput = form.elements.namedItem('notice_version');
  let open = false;
  let pending = false;

  const setStatus = (message) => {
    status.textContent = message;
    status.hidden = false;
  };

  const setError = (message) => {
    error.textContent = message;
    error.hidden = !message;
  };

  const closeWaitlist = (message) => {
    open = false;
    fields.disabled = true;
    submit.disabled = true;
    setStatus(message);
    note.textContent = 'No information was submitted. Check back when requests are available.';
  };

  const loadAvailability = async () => {
    fields.disabled = true;
    submit.disabled = true;
    setStatus('Checking request availability…');
    try {
      const response = await fetch(`${endpoint}?format=json`, {
        headers: { Accept: 'application/json' },
        credentials: 'same-origin'
      });
      if (!response.ok) throw new Error('availability request failed');
      const config = await response.json();
      const configured = config && config.open === true
        && typeof config.notice_version === 'string'
        && config.notice_version.trim() !== ''
        && config.notice_version === noticeInput.value;
      if (!configured) {
        closeWaitlist(config && config.open === false
          ? 'Requests are currently closed. The form is disabled.'
          : 'Requests are unavailable until the service and privacy notice are configured.');
        return;
      }
      noticeVersion.textContent = config.notice_version;
      open = true;
      fields.disabled = false;
      submit.disabled = false;
      setStatus('Requests are open.');
      note.textContent = 'Requests are reviewed manually.';
    } catch (_error) {
      closeWaitlist('Requests are unavailable right now. The form is disabled; no information was submitted.');
    }
  };

  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (!open || pending || !form.reportValidity()) return;

    setError('');
    const body = new URLSearchParams(new FormData(form));
    pending = true;
    fields.disabled = true;
    submit.disabled = true;
    setStatus('Sending your request for manual review…');

    try {
      const response = await fetch(endpoint, {
        method: 'POST',
        headers: {
          Accept: 'application/json',
          'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8'
        },
        credentials: 'same-origin',
        body
      });
      if (response.status !== 202) throw new Error('waitlist submission failed');
      form.reset();
      fields.disabled = true;
      setStatus("Request received. We'll review it.");
      note.textContent = '';
    } catch (_error) {
      fields.disabled = false;
      submit.disabled = false;
      setError("Couldn't send your request. Your details are still here. Try again.");
      setStatus('Submission failed. Your details are kept for a retry.');
    } finally {
      pending = false;
    }
  });

  loadAvailability();
})();
