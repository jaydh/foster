// Conjunction-screening widget — polls a plain axum route on its own
// setInterval, entirely independent of Foster's SSE stream for the
// "conjunction" machine (which is only tracking whether the button has been
// clicked). This is the actual thing this section exists to test: does a
// widget's own polling loop against a hand-rolled endpoint interfere with
// Foster's SSE connection for the same page section — five machines' worth
// of EventSource connections are already open by the time this one starts,
// right at the six-per-origin HTTP/1.1 limit Foster's README warns about.

export function initScreening() {
  const button = document.getElementById('screening-start');
  const statusEl = document.getElementById('screening-status');
  const eventsEl = document.getElementById('screening-events');
  if (!button || !statusEl || !eventsEl) return;

  let polling = null;

  async function poll() {
    const res = await fetch('/api/screening');
    const data = await res.json();
    statusEl.textContent = data.status;

    if (data.status === 'complete') {
      eventsEl.innerHTML = data.events.map((e) => `<li>${e}</li>`).join('');
      clearInterval(polling);
      polling = null;
    } else {
      eventsEl.innerHTML = '';
    }
  }

  // The button also carries `fx-on="click->start_screening"` — Foster's own
  // delegated listener handles that half independently of this one.
  button.addEventListener('click', async () => {
    eventsEl.innerHTML = '';
    statusEl.textContent = 'running';
    await fetch('/api/screening/start', { method: 'POST' });
    if (polling) clearInterval(polling);
    polling = setInterval(poll, 1200);
  });

  poll();
}
