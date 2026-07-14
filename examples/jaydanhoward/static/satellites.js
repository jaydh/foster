// Satellite tracker — plain canvas + requestAnimationFrame, same pattern as
// life.js. Foster owns run/pause and the three ring-visibility toggles;
// this loop reads those straight off the DOM (`data-fx-state`, three
// `fx-if` markers) — same contract life.js uses. No Rust binding into
// foster-client, no round trip per frame.
//
// The satellites themselves are real: src/satellites.rs fetches real TLEs
// from CelesTrak and computes each one's real angle-right-now and real
// angular velocity (from the TLE's mean motion) via the sgp4 crate, once,
// at startup/refresh. Rather than a new fx-* attribute for list data, this
// reads it off `fx-for`'s own `data-fx-item` attribute (a JSON blob Foster
// already stamps on every cloned list item) — the same DOM-as-integration-
// boundary approach as data-fx-state, just for structured data instead of a
// single value. Real angular velocities are far too slow to see directly
// (a GEO satellite's real period is ~24h), so playback runs at TIME_SCALE×
// real speed for visibility — same idea as the real site's own "Speed"
// slider, which does the same time compression.

const TIME_SCALE = 300;
const RINGS = [
  { key: 'leo', radius: 70,  color: '#f59e0b', flagId: 'flag-leo' },
  { key: 'meo', radius: 120, color: '#34d399', flagId: 'flag-meo' },
  { key: 'geo', radius: 170, color: '#a78bfa', flagId: 'flag-geo' },
];

function isFlagOn(id) {
  const el = document.getElementById(id);
  if (!el) return true;
  return getComputedStyle(el).display !== 'none';
}

function readItems(ring) {
  const container = document.querySelector(`[fx-for="${ring.key}"]`);
  if (!container) return [];
  return Array.from(container.querySelectorAll('[data-fx-item]')).map((el) => {
    const item = JSON.parse(el.getAttribute('data-fx-item'));
    return { ring, angle: item.angle, angular_velocity: item.angular_velocity };
  });
}

export function initSatellites() {
  const canvas = document.getElementById('sat-canvas');
  const root = document.querySelector('[fx-machine="satellites"]');
  if (!canvas || !root) return;

  const ctx2d = canvas.getContext('2d');
  const cx = canvas.width / 2;
  const cy = canvas.height / 2;

  let points = RINGS.flatMap(readItems);
  let lastTs = performance.now();

  function draw() {
    ctx2d.fillStyle = getComputedStyle(document.body).getPropertyValue('--surface') || '#0f1420';
    ctx2d.fillRect(0, 0, canvas.width, canvas.height);

    ctx2d.beginPath();
    ctx2d.arc(cx, cy, 18, 0, Math.PI * 2);
    ctx2d.fillStyle = getComputedStyle(document.body).getPropertyValue('--accent') || '#60a5fa';
    ctx2d.fill();

    for (const ring of RINGS) {
      if (!isFlagOn(ring.flagId)) continue;
      ctx2d.beginPath();
      ctx2d.arc(cx, cy, ring.radius, 0, Math.PI * 2);
      ctx2d.strokeStyle = 'rgba(128,128,128,0.25)';
      ctx2d.stroke();
    }

    for (const p of points) {
      if (!isFlagOn(p.ring.flagId)) continue;
      const x = cx + Math.cos(p.angle) * p.ring.radius;
      const y = cy + Math.sin(p.angle) * p.ring.radius;
      ctx2d.beginPath();
      ctx2d.arc(x, y, 2.5, 0, Math.PI * 2);
      ctx2d.fillStyle = p.ring.color;
      ctx2d.fill();
    }
  }

  function frame(ts) {
    const dt = (ts - lastTs) / 1000;
    lastTs = ts;
    if (root.getAttribute('data-fx-state') === 'running') {
      for (const p of points) p.angle += p.angular_velocity * TIME_SCALE * dt;
    }
    draw();
    requestAnimationFrame(frame);
  }

  // Re-read the real data whenever the machine's snapshot changes (e.g.
  // after "Refresh TLEs") instead of animating from a stale fetch forever.
  const observer = new MutationObserver(() => { points = RINGS.flatMap(readItems); });
  observer.observe(root, { attributes: true, attributeFilter: ['data-fx-version'] });

  draw();
  requestAnimationFrame(frame);
}
