// Satellite tracker — plain canvas + requestAnimationFrame, same pattern as
// life.js but at real animation frame rate: dozens of points animating
// continuously, not a coarse ~90ms simulation step. Foster owns run/pause and
// the three ring-visibility toggles; this loop reads all four straight off
// the DOM — `data-fx-state` for run/pause, and three `fx-if` marker elements
// (display:block/none) for the ring toggles — the same contract life.js
// uses. No Rust binding into foster-client, no round trip per frame.

const RINGS = [
  { name: 'leo', radius: 70,  count: 14, speed: 0.020, color: '#f59e0b', flagId: 'flag-leo' },
  { name: 'meo', radius: 120, count: 8,  speed: 0.008, color: '#34d399', flagId: 'flag-meo' },
  { name: 'geo', radius: 170, count: 4,  speed: 0.003, color: '#a78bfa', flagId: 'flag-geo' },
];

function isFlagOn(id) {
  const el = document.getElementById(id);
  if (!el) return true;
  return getComputedStyle(el).display !== 'none';
}

export function initSatellites() {
  const canvas = document.getElementById('sat-canvas');
  const root = document.querySelector('[fx-machine="satellites"]');
  if (!canvas || !root) return;

  const ctx2d = canvas.getContext('2d');
  const cx = canvas.width / 2;
  const cy = canvas.height / 2;

  const points = RINGS.flatMap((ring) =>
    Array.from({ length: ring.count }, (_, i) => ({
      ring,
      angle: (i / ring.count) * Math.PI * 2,
    }))
  );

  function draw() {
    ctx2d.fillStyle = getComputedStyle(document.body).getPropertyValue('--surface') || '#0f1420';
    ctx2d.fillRect(0, 0, canvas.width, canvas.height);

    // Earth
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
      ctx2d.arc(x, y, 3, 0, Math.PI * 2);
      ctx2d.fillStyle = p.ring.color;
      ctx2d.fill();
    }
  }

  function frame() {
    if (root.getAttribute('data-fx-state') === 'running') {
      for (const p of points) p.angle += p.ring.speed;
    }
    draw();
    requestAnimationFrame(frame);
  }

  draw();
  requestAnimationFrame(frame);
}
