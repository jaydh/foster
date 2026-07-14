// Real Lighthouse audit widget — same independent-polling shape as
// screening.js: a hand-rolled setInterval against /api/lighthouse, nothing
// to do with Foster's SSE for the "lighthouse" machine (which only tracks
// the button's idle/started label).

export function initLighthouse() {
  const button = document.getElementById('lighthouse-start');
  const statusEl = document.getElementById('lighthouse-status');
  const scoresEl = document.getElementById('lighthouse-scores');
  const linkEl = document.getElementById('lighthouse-report-link');
  if (!button || !statusEl || !scoresEl) return;

  let polling = null;

  function scoreClass(score) {
    if (score >= 90) return 'lh-good';
    if (score >= 50) return 'lh-mid';
    return 'lh-poor';
  }

  async function poll() {
    const res = await fetch('/api/lighthouse');
    const data = await res.json();
    statusEl.textContent = data.status;

    if (data.status === 'complete') {
      scoresEl.innerHTML = ['performance', 'accessibility', 'bestPractices', 'seo']
        .map((key) => {
          const label = key === 'bestPractices' ? 'best practices' : key;
          const score = Math.round(data[key]);
          return `<div class="lh-score ${scoreClass(score)}"><div class="lh-score-value">${score}</div><div class="lh-score-label">${label}</div></div>`;
        })
        .join('');
      linkEl.href = data.reportUrl;
      linkEl.style.display = 'inline';
      clearInterval(polling);
      polling = null;
    } else if (data.status === 'failed') {
      scoresEl.innerHTML = `<div class="lh-error">${data.error}</div>`;
      clearInterval(polling);
      polling = null;
    } else {
      scoresEl.innerHTML = '';
      linkEl.style.display = 'none';
    }
  }

  button.addEventListener('click', async () => {
    scoresEl.innerHTML = '';
    statusEl.textContent = 'running';
    await fetch('/api/lighthouse/start', { method: 'POST' });
    if (polling) clearInterval(polling);
    polling = setInterval(poll, 2000);
  });

  poll();
}
