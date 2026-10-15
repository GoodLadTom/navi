// The Navi bubble as a reusable view: the overlay window and the home-page demo both use it.
// States: hidden, listening, handsFree, ramble, processing, done, error, learned, notice.
import { icon } from './icons.js';
import { createFirefly, FireflyRoam } from './firefly.js';

const WIDTH = { listening: 236, handsFree: 236, ramble: 400, processing: 136, done: 44 };
// Messages: the widest the bubble gets, and the dot, gaps and padding beside the text.
const MSG_MAX = 420;
const MSG_PAD = 58;

// How long a message stays if Navi doesn't hide it first: longer for a long one, as it may run to
// two lines, and then a little past controller.rs read_ms so Navi's own timing wins.
const holdMs = (text, atLeast) => Math.min(6600, Math.max(atLeast, 1600 + 50 * text.length));

export class BubbleView {
  // roam: the firefly has the whole host to fly about in (the overlay window). Otherwise it hovers
  // above the bubble (the home-page demo).
  constructor(host, { roam = false } = {}) {
    this.el = document.createElement('div');
    this.el.className = 'bubble hidden';
    this.el.dataset.state = 'hidden';
    this.el.innerHTML = `
      <div class="b-row">
        <span class="b-dot"></span>
        <span class="b-lock">${icon('lock', 14, 2)}</span>
        <span class="b-spark">${icon('spark', 15, 1.8)}</span>
        <span class="b-label"></span>
        <canvas class="b-wave"></canvas>
        <span class="b-think"><i></i><i></i><i></i></span>
        <span class="b-msg"></span>
        <span class="b-time">0:00</span>
        <span class="b-chip"></span>
        <svg class="b-check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>
      </div>
      <div class="b-live"><span></span></div>`;
    this.firefly = createFirefly();
    host.append(this.firefly, this.el);
    this.fly = roam ? new FireflyRoam(this.firefly) : perch(this.firefly);
    // Settings > General > Show the firefly.
    this.fireflyOn = true;
    this.$ = (s) => this.el.querySelector(s);
    this.canvas = this.$('.b-wave');
    this.ctx = this.canvas.getContext('2d');
    this.levels = new Array(36).fill(0);
    this.smoothed = 0;
    this.state = 'hidden';
    this.started = 0;
    this.timer = null;
    this.raf = null;
    this.hideTimer = null;
    this.sizeCanvas();
  }

  sizeCanvas() {
    const dpr = window.devicePixelRatio || 1;
    const w = this.canvas.clientWidth || 112;
    const hgt = this.canvas.clientHeight || 26;
    this.canvas.width = Math.round(w * dpr);
    this.canvas.height = Math.round(hgt * dpr);
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.cw = w; this.ch = hgt;
  }

  set(p) {
    clearTimeout(this.hideTimer);
    const s = p.state;
    if (s === 'hidden') return this.hide();
    const wasHidden = this.state === 'hidden';
    this.el.classList.remove('hidden', 'popout');
    let kind = s;
    if (s === 'listening') kind = p.kind === 'ramble' ? 'ramble' : p.kind === 'handsFree' ? 'handsFree' : 'listening';
    this.el.dataset.state = kind;
    const isListening = kind === 'listening' || kind === 'handsFree' || kind === 'ramble';
    if (isListening && this.fireflyOn) {
      this.firefly.classList.toggle('ramble', kind === 'ramble');
      this.fly.arrive();
    } else {
      this.fly.leave();
    }

    const chip = this.$('.b-chip');
    const label = this.$('.b-label');
    const msg = this.$('.b-msg');

    if (kind === 'listening' || kind === 'handsFree' || kind === 'ramble') {
      chip.textContent = p.modeName || 'Clean';
      chip.style.display = '';
      // Hands-free and rambles carry on after you let go, so say how to finish them.
      label.textContent = kind === 'listening' ? '' : 'Enter to finish';
      if (this.state !== 'listening' && this.state !== 'handsFree' && this.state !== 'ramble') {
        this.started = performance.now();
        this.levels.fill(0);
        this.$('.b-live span').textContent = '';
      }
      this.startTimer();
      this.startDraw();
    } else {
      this.stopTimer();
    }

    if (kind === 'processing') {
      label.textContent = p.modeName && p.modeName !== 'Clean' && p.modeName !== 'Raw' ? 'Writing' : 'Thinking';
      chip.textContent = p.modeName || '';
      chip.style.display = p.modeName ? '' : 'none';
      this.stopDraw();
    }
    if (kind === 'done') {
      this.stopDraw();
      this.hideTimer = setTimeout(() => this.popOut(), 650);
    }
    if (kind === 'error') {
      msg.textContent = p.message || 'Something went wrong';
      this.stopDraw();
      this.hideTimer = setTimeout(() => this.hide(), holdMs(msg.textContent, 2600));
    }
    if (kind === 'notice') {
      msg.textContent = p.message || '';
      this.stopDraw();
      this.hideTimer = setTimeout(() => this.hide(), holdMs(msg.textContent, 4000));
    }
    if (kind === 'learned') {
      msg.innerHTML = '';
      msg.append(Object.assign(document.createElement('b'), { textContent: 'Learned ' }));
      msg.append(Object.assign(document.createElement('s'), { textContent: p.from }));
      msg.append(document.createTextNode('  →  '));
      msg.append(Object.assign(document.createElement('b'), { textContent: p.to }));
      this.stopDraw();
      this.hideTimer = setTimeout(() => this.hide(), 2600);
    }

    this.state = kind;
    this.applyWidth(kind);
    if (wasHidden) {
      this.el.classList.remove('enter');
      void this.el.offsetWidth;
      this.el.classList.add('enter');
    }
  }

  applyWidth(kind) {
    let w = WIDTH[kind];
    let twoLines = false;
    if (kind === 'listening' || kind === 'handsFree') {
      // padding + dot/lock + wave + timer + gaps, then the mode chip's own width.
      const chip = this.$('.b-chip');
      w = 15 + 14 + 10 + 112 + 10 + 32 + 10 + Math.ceil(chip.scrollWidth) + 10;
      if (kind === 'handsFree') w += Math.ceil(this.$('.b-label').scrollWidth) + 10;
    }
    if (kind === 'error' || kind === 'learned' || kind === 'notice') {
      const msg = this.$('.b-msg');
      // Errors and notices wrap onto a second line (bubble.css): measure the text on one line to
      // see whether they need to. +2 because scrollWidth is rounded, and a width rounded down
      // would wrap a message that fits.
      const wraps = kind !== 'learned';
      if (wraps) Object.assign(msg.style, { display: 'block', whiteSpace: 'nowrap', width: '' });
      const text = Math.ceil(msg.scrollWidth) + (wraps ? 2 : 0);
      if (wraps) Object.assign(msg.style, { display: '', whiteSpace: '' });
      w = Math.min(MSG_MAX, text + MSG_PAD);
      twoLines = wraps && text + MSG_PAD > MSG_MAX;
      msg.style.width = wraps ? `${w - MSG_PAD}px` : '';
    }
    this.el.classList.toggle('two-line', twoLines);
    if (kind === 'processing') {
      const chip = this.$('.b-chip');
      w = 120 + (chip.style.display === 'none' ? 0 : chip.scrollWidth + 4);
    }
    if (w) this.el.style.width = `${w}px`;
    if (kind === 'ramble') requestAnimationFrame(() => this.sizeCanvas());
    // Text measured before Poppins has loaded comes out too narrow; measure again once it has.
    if (document.fonts && document.fonts.status !== 'loaded') {
      document.fonts.ready.then(() => { if (this.state === kind) this.applyWidth(kind); });
    }
  }

  level(v) {
    // Perceptual smoothing: quick to rise, slower to fall.
    const target = Math.max(0, Math.min(1, v));
    this.smoothed = target > this.smoothed ? this.smoothed * 0.35 + target * 0.65 : this.smoothed * 0.8 + target * 0.2;
    this.levels.push(this.smoothed);
    if (this.levels.length > 36) this.levels.shift();
    const dot = this.$('.b-dot');
    dot.style.setProperty('--lvl-scale', (1 + this.smoothed * 0.55).toFixed(3));
    dot.style.setProperty('--lvl-ring', (this.smoothed * 0.6).toFixed(3));
    dot.style.setProperty('--lvl-ring-scale', (1 + this.smoothed * 0.5).toFixed(3));
  }

  live(text) {
    const span = this.$('.b-live span');
    span.textContent = text.length > 140 ? '…' + text.slice(-140) : text;
  }

  startTimer() {
    if (this.timer) return;
    const t = this.$('.b-time');
    const tick = () => {
      const s = Math.floor((performance.now() - this.started) / 1000);
      t.textContent = `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
    };
    tick();
    this.timer = setInterval(tick, 250);
  }

  stopTimer() { clearInterval(this.timer); this.timer = null; }

  startDraw() {
    if (this.raf) return;
    // One colour for every bar, older bars fainter: a highlight on only the loudest recent bars
    // read as a stray patch of colour.
    const colour = getComputedStyle(this.el).getPropertyValue('--accent-ink').trim() || '#5B3FA0';
    const draw = () => {
      this.raf = requestAnimationFrame(draw);
      const { ctx, cw, ch } = this;
      ctx.clearRect(0, 0, cw, ch);
      const n = this.levels.length;
      const step = cw / n;
      const bar = Math.max(2, step * 0.6);
      const gap = step - bar;
      for (let i = 0; i < n; i++) {
        const v = this.levels[i];
        const hh = Math.max(3, v * (ch - 2));
        const x = cw - (n - i) * step + gap / 2;
        const y = (ch - hh) / 2;
        const age = i / (n - 1);
        ctx.globalAlpha = 0.22 + age * 0.78;
        ctx.fillStyle = colour;
        roundRect(ctx, x, y, bar, hh, bar / 2);
      }
      ctx.globalAlpha = 1;
    };
    draw();
  }

  stopDraw() { cancelAnimationFrame(this.raf); this.raf = null; }

  // Shows or hides the firefly (Settings > General). Turned off, it leaves at once and stays away.
  setFirefly(on) {
    this.fireflyOn = on !== false;
    this.firefly.hidden = !this.fireflyOn;
    if (!this.fireflyOn) this.fly.leave();
  }

  popOut() {
    this.el.classList.add('popout');
    this.hideTimer = setTimeout(() => this.hide(true), 340);
  }

  hide(immediate = false) {
    this.stopTimer();
    this.stopDraw();
    this.el.classList.remove('enter', 'popout');
    this.el.classList.add('hidden');
    // The firefly finishes flying off by itself.
    this.fly.leave();
    this.firefly.classList.remove('ramble');
    this.state = 'hidden';
    this.el.dataset.state = 'hidden';
    if (immediate) this.el.style.transition = 'none', requestAnimationFrame(() => (this.el.style.transition = ''));
    this.onHidden?.();
  }
}

// The home-page demo's firefly: flies in, hovers above the bubble, and flies away to the right.
function perch(el) {
  el.addEventListener('animationend', (event) => {
    if (event.animationName === 'fireflyLeave') el.classList.remove('departing');
  });
  return {
    arrive() {
      if (el.classList.contains('active')) return;
      el.classList.remove('departing');
      void el.offsetWidth;
      el.classList.add('active');
    },
    leave() {
      if (!el.classList.contains('active')) return;
      el.classList.remove('active');
      el.classList.add('departing');
    },
  };
}

function roundRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.fill();
}
