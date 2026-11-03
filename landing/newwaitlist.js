(() => {
  "use strict";

  const form = document.querySelector("#waitlist-form");
  if (!form) return;

  const endpoint = "/api/waitlist";
  const fields = form.querySelector("[data-waitlist-fields]");
  const submit = form.querySelector("[data-waitlist-submit]");
  const status = form.querySelector("[data-waitlist-status]");
  const error = form.querySelector("[data-waitlist-error]");
  const note = form.querySelector("[data-waitlist-note]");
  const controller = form.querySelector("[data-notice-controller]");
  const privacyEmail = form.querySelector("[data-notice-email]");
  const noticeVersion = form.querySelector("[data-notice-version]");
  const noticeInput = form.elements.namedItem("notice_version");
  let open = false;
  let pending = false;

  const configuredText = (value) => typeof value === "string" && value.trim() !== "";

  const setStatus = (message) => {
    status.textContent = message;
    status.hidden = false;
  };

  const setError = (message) => {
    error.textContent = message;
    error.hidden = !message;
  };

  const setNotice = (config) => {
    if (controller) controller.textContent = config.controller_name;
    if (privacyEmail) privacyEmail.textContent = config.privacy_email;
    if (noticeVersion) noticeVersion.textContent = config.notice_version;
  };

  const closeWaitlist = (message) => {
    open = false;
    fields.disabled = true;
    if (submit) submit.disabled = true;
    setStatus(message);
    if (note) note.textContent = "No information was submitted. Check back when the waitlist is available.";
  };

  const loadAvailability = async () => {
    fields.disabled = true;
    if (submit) submit.disabled = true;
    setStatus("Checking waitlist availability…");
    try {
      const response = await fetch(`${endpoint}?format=json`, {
        headers: { Accept: "application/json" },
        credentials: "same-origin"
      });
      if (!response.ok) throw new Error("availability request failed");
      const config = await response.json();
      const configured = config && config.open === true
        && configuredText(config.controller_name)
        && configuredText(config.privacy_email)
        && configuredText(config.notice_version)
        && config.notice_version === noticeInput.value;
      if (!configured) {
        closeWaitlist(config && config.open === false
          ? "The waitlist is currently closed. The form is disabled."
          : "The waitlist is unavailable until its service and privacy notice are configured.");
        return;
      }
      setNotice(config);
      open = true;
      fields.disabled = false;
      if (submit) submit.disabled = false;
      setStatus("The waitlist is open. Requests are reviewed manually; submitting is not acceptance.");
      if (note) note.textContent = "If approved, you will receive an invitation when Cloud is ready. There is no automatic acceptance.";
    } catch (_error) {
      closeWaitlist("The waitlist is unavailable right now. The form is disabled; no information was submitted.");
    }
  };

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!open || pending) return;

    setError("");
    const body = new URLSearchParams(new FormData(form));
    pending = true;
    fields.disabled = true;
    submit.disabled = true;
    setStatus("Sending your request for manual review…");

    try {
      const response = await fetch(endpoint, {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8"
        },
        credentials: "same-origin",
        body
      });
      if (response.status !== 202) throw new Error("waitlist submission failed");
      form.reset();
      setStatus("Application received for manual review. This is not acceptance; approved participants will be invited when Cloud is ready.");
      if (note) note.textContent = "Your request was received. The first 10 accepted participants may receive 30 days from activation; acceptance is not automatic.";
    } catch (_error) {
      fields.disabled = false;
      submit.disabled = false;
      setError("We could not submit your request. Your entries are still here; check the fields and try again.");
      setStatus("Submission failed. Your entries are kept for a retry.");
    } finally {
      pending = false;
    }
  });

  loadAvailability();
})();
