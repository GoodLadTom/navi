// Small DOM helpers and shared components. No framework: h() builds elements, components are
// functions that return elements.
import { icon } from './icons.js';

export function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (k === 'html') el.innerHTML = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (v === true) el.setAttribute(k, '');
    else el.setAttribute(k, v);
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}

export function ico(name, size = 18, stroke) {
  return h('span', { class: 'i', style: { display: 'inline-grid', placeItems: 'center', lineHeight: 0 }, html: icon(name, size, stroke) });
}

export function btn(label, { variant = '', icon: ic, onClick, title, size = '', disabled, iconRight = false } = {}) {
  const b = h('button', { class: `btn ${variant} ${size} ${label ? '' : 'icon'}`.trim(), title, onClick, disabled });
  const i = ic ? ico(ic, size === 'sm' ? 15 : 16) : null;
  if (i && !iconRight) b.append(i);
  if (label) b.append(document.createTextNode(label));
  if (i && iconRight) b.append(i);
  return b;
}

export function toggle(on, onChange) {
  const t = h('button', { class: `toggle ${on ? 'on' : ''}`, role: 'switch', 'aria-checked': String(!!on) });
  t.addEventListener('click', () => {
    const next = !t.classList.contains('on');
    t.classList.toggle('on', next);
    t.setAttribute('aria-checked', String(next));
    onChange?.(next);
  });
  return t;
}

export function select(options, value, onChange, cls = '') {
  const s = h('select', { class: `select ${cls}` });
  for (const o of options) {
    const opt = typeof o === 'string' ? { value: o, label: o } : o;
    s.append(h('option', { value: opt.value, selected: String(opt.value) === String(value) }, opt.label));
  }
  s.addEventListener('change', () => onChange?.(s.value));
  return s;
}

export function seg(options, value, onChange) {
  const wrap = h('div', { class: 'seg' });
  for (const o of options) {
    const b = h('button', { class: o.value === value ? 'on' : '' }, o.label);
    b.addEventListener('click', () => {
      wrap.querySelectorAll('button').forEach((x) => x.classList.remove('on'));
      b.classList.add('on');
      onChange?.(o.value);
    });
    wrap.append(b);
  }
  return wrap;
}

// The text sits in its own span so a long one can end in an ellipsis (app.css .chip .ct).
export function chip(text, tone = '', dot = false) {
  return h('span', { class: `chip ${tone}` }, dot ? h('span', { class: 'd' }) : null, h('span', { class: 'ct' }, text));
}

export function keycaps(keys, size = '') {
  const wrap = h('span', { class: 'row keycaps', style: { gap: size === 'lg' ? '12px' : '6px' } });
  keys.forEach((k, i) => {
    const label = keyLabel(k);
    const cap = h('span', { class: `kbd ${size} ${size === 'lg' && label.length > 3 ? 'wide' : ''}` }, label);
    // Each "+" stays with the key after it, so a long shortcut wraps between keys.
    wrap.append(i ? h('span', { class: 'kc' }, h('span', { class: 'plus' }, '+'), cap) : cap);
  });
  return wrap;
}

// Friendly names for Navi's key names (mirrors hotkey::key_label in Rust).
const KEY_LABELS = {
  Win: 'Win', LWin: 'Left Win', RWin: 'Right Win', Ctrl: 'Ctrl', LCtrl: 'Left Ctrl', RCtrl: 'Right Ctrl',
  Alt: 'Alt', LAlt: 'Left Alt', RAlt: 'Right Alt', Shift: 'Shift', LShift: 'Left Shift', RShift: 'Right Shift',
  Space: 'Space', Escape: 'Esc', CapsLock: 'Caps Lock', ScrollLock: 'Scroll Lock', NumLock: 'Num Lock',
  PrintScreen: 'Print Screen', PageUp: 'Page Up', PageDown: 'Page Down', Left: 'Left Arrow', Right: 'Right Arrow',
  Up: 'Up Arrow', Down: 'Down Arrow', Backslash: '\\', IntlBackslash: '\\', Backquote: '`', Semicolon: ';',
  Quote: "'", Comma: ',', Period: '.', Slash: '/', Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']',
  MediaPlayPause: 'Play/Pause', VolumeMute: 'Mute',
};

// Labels from Rust in this computer's keyboard layout (a UK keyboard's ` key is "Oem8" inside
// Navi). Filled by app.js and the shortcut picker; consulted first.
export const keyLabelCache = {};

export function keyLabel(k) {
  if (keyLabelCache[k]) return keyLabelCache[k];
  if (KEY_LABELS[k]) return KEY_LABELS[k];
  const num = /^Numpad(\d)$/.exec(k);
  return num ? `Num ${num[1]}` : k;
}

export function chordLabel(keys) {
  return (keys || []).map(keyLabel).join(' + ');
}

export function setRow(title, desc, control) {
  return h('div', { class: 'set-row' },
    h('div', { class: 'txt' }, h('div', { class: 'h3' }, title), desc ? h('p', {}, desc) : null),
    h('div', { class: 'ctl' }, control));
}

export function field(label, control) {
  return h('div', { class: 'field' }, h('label', {}, label), control);
}

// ---------------------------------------------------------------- toasts and dialogs
let toastHost;
export function toast(message, { error = false, ms = 2600 } = {}) {
  toastHost = toastHost || document.body.appendChild(h('div', { class: 'toasts' }));
  const t = h('div', { class: `toast ${error ? 'err' : ''}` }, h('span', { class: 'ti', html: icon(error ? 'info' : 'check', 13, 2.2) }), message);
  toastHost.append(t);
  setTimeout(() => { t.classList.add('out'); setTimeout(() => t.remove(), 240); }, ms);
}

export function confirmDialog(title, body, { ok = 'Confirm', danger = false } = {}) {
  return new Promise((resolve) => {
    const close = (v) => { scrim.remove(); resolve(v); };
    const scrim = h('div', { class: 'scrim', onClick: (e) => { if (e.target === scrim) close(false); } },
      h('div', { class: 'dialog' },
        h('div', { class: 'h2' }, title),
        h('p', { class: 'quiet', style: { marginTop: '8px' } }, body),
        h('div', { class: 'actions' }, btn('Cancel', { variant: 'ghost', onClick: () => close(false) }), btn(ok, { variant: danger ? 'danger' : 'primary', onClick: () => close(true) }))));
    document.body.append(scrim);
  });
}

export function promptDialog(title, fields, { ok = 'Save', note = '' } = {}) {
  return new Promise((resolve) => {
    const inputs = fields.map((f) => {
      const el = f.multiline ? h('textarea', { class: 'textarea', placeholder: f.placeholder || '' }) : h('input', { class: 'input', placeholder: f.placeholder || '', value: f.value || '' });
      if (f.multiline && f.value) el.value = f.value;
      return [f.key, el, f.label];
    });
    const close = (v) => { scrim.remove(); resolve(v); };
    const submit = () => close(Object.fromEntries(inputs.map(([k, el]) => [k, el.value.trim()])));
    const scrim = h('div', { class: 'scrim', onClick: (e) => { if (e.target === scrim) close(null); } },
      h('div', { class: 'dialog' },
        h('div', { class: 'h2', style: { marginBottom: '16px' } }, title),
        h('div', { class: 'stack', style: { gap: '12px' } }, inputs.map(([, el, label]) => field(label, el))),
        note ? h('p', { class: 'small quiet', style: { marginTop: '12px' } }, note) : null,
        h('div', { class: 'actions' }, btn('Cancel', { variant: 'ghost', onClick: () => close(null) }), btn(ok, { variant: 'primary', onClick: submit }))));
    document.body.append(scrim);
    setTimeout(() => inputs[0]?.[1].focus(), 50);
    scrim.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey && e.target.tagName !== 'TEXTAREA') submit(); if (e.key === 'Escape') close(null); });
  });
}

// ---------------------------------------------------------------- formatting
export function relTime(isoStr) {
  const d = new Date(isoStr);
  const s = (Date.now() - d.getTime()) / 1000;
  if (s < 50) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400 && d.getDate() === new Date().getDate()) return d.toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit' });
  if (s < 172800) return 'yesterday';
  return d.toLocaleDateString('en-GB', { day: 'numeric', month: 'short' });
}

export function dayLabel(isoStr) {
  const d = new Date(isoStr);
  const today = new Date();
  const y = new Date(Date.now() - 86400000);
  if (d.toDateString() === today.toDateString()) return 'Today';
  if (d.toDateString() === y.toDateString()) return 'Yesterday';
  return d.toLocaleDateString('en-GB', { weekday: 'long', day: 'numeric', month: 'long' });
}

export function clock(isoStr) {
  return new Date(isoStr).toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit' });
}

export function fmtNum(n) {
  return Math.round(n).toLocaleString('en-GB');
}

export function fmtDuration(ms) {
  const s = Math.round(ms / 1000);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s`;
}

export function countUp(el, to, { ms = 900, format = fmtNum } = {}) {
  const start = performance.now();
  const ease = (t) => 1 - Math.pow(1 - t, 4);
  const step = (now) => {
    const t = Math.min(1, (now - start) / ms);
    el.textContent = format(to * ease(t));
    if (t < 1) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

export function appInitial(name) {
  const clean = (name || '?').replace(/\.exe$/i, '');
  return clean.slice(0, 1).toUpperCase();
}

export function debounce(fn, ms = 250) {
  let t;
  return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); };
}

export function errText(e) {
  if (!e) return 'Something went wrong';
  if (typeof e === 'string') return e;
  return e.message || String(e);
}
