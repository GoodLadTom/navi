// The listening companion: Navi's own firefly (chosen on 15 Oct 2026). A small bright body and
// one pair of rounded wings that beat together, glowing in the Mist lilac. Inline SVG keeps the wings
// animatable.
let nextId = 0;

export function createFirefly() {
  const id = `navi-firefly-${++nextId}`;
  const el = document.createElement('div');
  el.className = 'navi-firefly';
  el.setAttribute('aria-hidden', 'true');
  const wing = `<path d="M124 114C112 96 92 78 74 76C62 75 56 84 62 93C72 108 96 118 118 122Q127 121 124 114Z" fill="url(#${id}-wing)" stroke="#A68CE6" stroke-opacity=".75" stroke-width="1.4"/><path d="M119 116C104 104 87 93 69 86" fill="none" stroke="#A68CE6" stroke-opacity=".6" stroke-width="1.1" stroke-linecap="round"/>`;
  el.innerHTML = `<div class="ff-flight"><div class="ff-hover">
    <svg viewBox="0 0 256 256" focusable="false" aria-hidden="true">
      <defs>
        <radialGradient id="${id}-halo"><stop offset="0" stop-color="#EEE6FF" stop-opacity=".6"/><stop offset=".45" stop-color="#E6DBFF" stop-opacity=".24"/><stop offset="1" stop-color="#E6DBFF" stop-opacity="0"/></radialGradient>
        <radialGradient id="${id}-core"><stop offset="0" stop-color="#FFFFFF" stop-opacity=".78"/><stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/></radialGradient>
        <radialGradient id="${id}-body" cx="37%" cy="31%" r="79%"><stop offset="0" stop-color="#FFFFFF"/><stop offset=".45" stop-color="#F6F1FF"/><stop offset="1" stop-color="#BFA8F0"/></radialGradient>
        <linearGradient id="${id}-wing" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#FFFFFF" stop-opacity=".98"/><stop offset=".55" stop-color="#F2EBFF" stop-opacity=".96"/><stop offset="1" stop-color="#C2ABF1" stop-opacity=".96"/></linearGradient>
      </defs>
      <circle class="ff-glow" cx="134" cy="122" r="104" fill="url(#${id}-halo)"/>
      <circle cx="134" cy="122" r="46" fill="url(#${id}-core)"/>
      <g class="ff-wing">${wing}</g>
      <g transform="translate(268 0) scale(-1 1)"><g class="ff-wing">${wing}</g></g>
      <circle cx="134" cy="122" r="24" fill="url(#${id}-body)" stroke="#A68CE6" stroke-opacity=".8" stroke-width="1.6"/>
    </svg>
  </div></div>`;
  return el;
}

// In the overlay the firefly comes in from the top right of the screen and hovers about in that
// corner while you talk (it stays at the top right, just hovering around, never out towards the
// middle), then flies off the nearest side when you stop. Positions are the centre of the sprite,
// in CSS pixels of the host (the overlay covers the screen's work area).
const SIZE = 78;           // the sprite box, as in bubble.css (half the first size)
const ROAM_SPEED = 34;     // px per second while hovering about
const ARRIVE_SPEED = 240;  // px per second coming in
const LEAVE_ACCEL = 1400;  // px per second², so it is gone in about a second
const TURN_BACK_MS = 1500; // a new take this soon after leaving turns it round mid-flight

// The corner it keeps to: at most 400 x 260 px (less on a small screen), just inside the top right.
function zone(w, h) {
  const x1 = w - 70;
  const y0 = 60;
  return { x0: Math.max(70, x1 - Math.min(400, w * 0.3)), x1, y0, y1: Math.max(y0 + 60, Math.min(y0 + 260, h * 0.36)) };
}

export class FireflyRoam {
  constructor(el) {
    this.el = el;
    el.classList.add('roam');
    this.state = 'off'; // off | arriving | roaming | leaving
    this.raf = 0;
    this.pos = { x: 0, y: 0 };
    this.vel = { x: 0, y: 0 };
    this.target = { x: 0, y: 0 };
    this.tilt = 0;
    this.scale = 1;
    this.leftAt = 0;
    this.reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
  }

  bounds() {
    const host = this.el.parentElement;
    return { w: host?.clientWidth || innerWidth, h: host?.clientHeight || innerHeight };
  }

  arrive() {
    if (this.state === 'arriving' || this.state === 'roaming') return;
    const { w, h } = this.bounds();
    // Long gone (or the overlay was hidden mid-flight): come in fresh from the top right.
    if (this.state === 'off' || performance.now() - this.leftAt > TURN_BACK_MS) {
      this.pos = { x: w + SIZE * 0.7, y: -SIZE * 0.5 };
      this.vel = { x: -ARRIVE_SPEED * 0.85, y: ARRIVE_SPEED * 0.45 };
      this.scale = 0.86;
      this.tilt = -10;
    }
    this.state = 'arriving';
    const z = zone(w, h);
    this.target = { x: z.x0 + (z.x1 - z.x0) * (0.45 + Math.random() * 0.3), y: z.y0 + (z.y1 - z.y0) * (0.3 + Math.random() * 0.3) };
    // Placed before it is shown, so it never flashes up at the top left for a frame.
    this.render(performance.now() / 1000, 0);
    this.el.classList.remove('departing');
    this.el.classList.add('active');
    if (this.reduced) {
      // No flight: it simply appears, still, just inside the top right.
      cancelAnimationFrame(this.raf);
      this.raf = 0;
      this.state = 'roaming';
      this.pos = { x: w - 110, y: 90 };
      this.vel = { x: 0, y: 0 };
      this.scale = 1;
      this.tilt = 0;
      this.render(0, 0);
      return;
    }
    this.run();
  }

  leave() {
    if (this.state === 'off' || this.state === 'leaving') return;
    if (this.reduced) return this.stop();
    const { w } = this.bounds();
    this.state = 'leaving';
    this.leftAt = performance.now();
    // Off whichever side it is nearer, or heading for, rising a little.
    const right = this.pos.x + this.vel.x * 0.6 > w / 2;
    this.exit = { x: right ? 0.9 : -0.9, y: -0.44 };
    this.el.classList.remove('active');
    this.el.classList.add('departing');
    this.run();
  }

  stop() {
    cancelAnimationFrame(this.raf);
    this.raf = 0;
    this.state = 'off';
    this.el.classList.remove('active', 'departing');
  }

  run() {
    if (this.raf) return;
    let last = performance.now();
    const frame = (now) => {
      const dt = Math.min(0.05, Math.max(0, (now - last) / 1000));
      last = now;
      this.step(dt, now / 1000);
      this.raf = this.state === 'off' ? 0 : requestAnimationFrame(frame);
    };
    this.raf = requestAnimationFrame(frame);
  }

  step(dt, t) {
    const { w, h } = this.bounds();
    if (this.state === 'leaving') {
      this.vel.x += this.exit.x * LEAVE_ACCEL * dt;
      this.vel.y += this.exit.y * LEAVE_ACCEL * dt;
      const speed = Math.hypot(this.vel.x, this.vel.y);
      if (speed > 2200) { this.vel.x *= 2200 / speed; this.vel.y *= 2200 / speed; }
      this.pos.x += this.vel.x * dt;
      this.pos.y += this.vel.y * dt;
      this.scale += (0.82 - this.scale) * Math.min(1, dt * 2);
      if (this.pos.x < -SIZE || this.pos.x > w + SIZE || this.pos.y < -SIZE || this.pos.y > h + SIZE) return this.stop();
    } else {
      const arriving = this.state === 'arriving';
      const dx = this.target.x - this.pos.x;
      const dy = this.target.y - this.pos.y;
      const dist = Math.hypot(dx, dy) || 1;
      if (dist < (arriving ? 50 : 40)) {
        this.state = 'roaming';
        this.target = this.pickTarget(w, h);
      }
      // Eases off near a target but never stops: it drifts on towards the next one.
      const max = arriving ? ARRIVE_SPEED : ROAM_SPEED;
      const speed = max * Math.max(0.35, Math.min(1, dist / 160));
      const turn = Math.min(1, (arriving ? 2.2 : 0.8) * dt);
      this.vel.x += ((dx / dist) * speed - this.vel.x) * turn;
      this.vel.y += ((dy / dist) * speed - this.vel.y) * turn;
      // Kept to its corner, softly (it may overshoot a little on the way in).
      if (!arriving) {
        const z = zone(w, h);
        const slack = 24;
        if (this.pos.x < z.x0 - slack) this.vel.x += (z.x0 - slack - this.pos.x) * 2 * dt;
        if (this.pos.x > z.x1 + slack) this.vel.x -= (this.pos.x - z.x1 - slack) * 2 * dt;
        if (this.pos.y < z.y0 - slack) this.vel.y += (z.y0 - slack - this.pos.y) * 2 * dt;
        if (this.pos.y > z.y1 + slack) this.vel.y -= (this.pos.y - z.y1 - slack) * 2 * dt;
      }
      this.pos.x += this.vel.x * dt;
      this.pos.y += this.vel.y * dt;
      this.scale += (1 - this.scale) * Math.min(1, dt * 1.5);
    }
    this.render(t, dt);
  }

  // Somewhere else in its corner, a short hop away, so it hovers about rather than travels.
  pickTarget(w, h) {
    const z = zone(w, h);
    for (let i = 0; i < 16; i++) {
      const x = z.x0 + Math.random() * (z.x1 - z.x0);
      const y = z.y0 + Math.random() * (z.y1 - z.y0);
      const d = Math.hypot(x - this.pos.x, y - this.pos.y);
      if (d > 50 && d < 220) return { x, y };
    }
    return { x: (z.x0 + z.x1) / 2, y: (z.y0 + z.y1) / 2 };
  }

  render(t, dt) {
    // Leans a little into its flight, and bobs gently so it floats rather than glides.
    const lean = Math.max(-14, Math.min(14, this.vel.x / (this.state === 'leaving' ? 60 : 14)));
    this.tilt += (lean - this.tilt) * Math.min(1, dt * 5);
    const bobY = this.reduced ? 0 : Math.sin(t * 1.9) * 5 + Math.sin(t * 0.7) * 3;
    const bobX = this.reduced ? 0 : Math.sin(t * 1.1 + 1.3) * 3;
    const x = this.pos.x - SIZE / 2 + bobX;
    const y = this.pos.y - SIZE / 2 + bobY;
    this.el.style.transform = `translate3d(${x.toFixed(1)}px, ${y.toFixed(1)}px, 0) rotate(${this.tilt.toFixed(2)}deg) scale(${this.scale.toFixed(3)})`;
  }
}
