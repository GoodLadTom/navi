// Main window: shell, routing, live status, and the shared store the views read from.
import { invoke, listen, win, isTauri } from './bridge.js';
import { h, ico, btn, keycaps, keyLabel, keyLabelCache, toast, errText } from './ui.js';
import { icon } from './icons.js';
import { applyTheme } from './theme.js';

export const store = {
  settings: null,
  info: null,
  engine: { state: 'starting', message: 'Starting…' },
  suggestions: 0,
  listeners: new Set(),
  set(patch) { Object.assign(this, patch); this.listeners.forEach((f) => f(this)); },
  on(f) { this.listeners.add(f); return () => this.listeners.delete(f); },
  async saveSettings(mutate) {
    const next = structuredClone(this.settings);
    mutate(next);
    try {
      const saved = await invoke('save_settings', { settings: next });
      this.set({ settings: saved || next });
      applyTheme(this.settings.general?.theme);
      await refreshKeyLabels();
      renderShortcut();
      return true;
    } catch (e) {
      toast(String(e), { error: true });
      return false;
    }
  },
};

const VIEWS = [
  { id: 'home', label: 'Home', icon: 'home' },
  { id: 'history', label: 'History', icon: 'history' },
  { id: 'notes', label: 'Notes', icon: 'notes' },
  { id: 'modes', label: 'Modes', icon: 'modes' },
  { id: 'speak', label: 'Speak', icon: 'volume' },
  { id: 'dictionary', label: 'Dictionary', icon: 'book' },
  { id: 'packs', label: 'Packs', icon: 'packs' },
  { id: 'settings', label: 'Settings', icon: 'gear' },
];

const loaders = {
  home: () => import('./views/home.js'),
  history: () => import('./views/history.js'),
  notes: () => import('./views/notes.js'),
  modes: () => import('./views/modes.js'),
  speak: () => import('./views/speak.js'),
  dictionary: () => import('./views/dictionary.js'),
  packs: () => import('./views/packs.js'),
  settings: () => import('./views/settings.js'),
};

let current = { id: null, cleanup: null };
const main = document.getElementById('main');

function renderNav(active) {
  const nav = document.getElementById('nav');
  nav.innerHTML = '';
  for (const v of VIEWS) {
    const b = h('button', { class: v.id === active ? 'on' : '', onClick: () => go(v.id) }, ico(v.icon, 18), v.label);
    if (v.id === 'dictionary' && store.suggestions > 0) b.append(h('span', { class: 'badge' }, String(store.suggestions)));
    if (v.id === 'notes' && (store.meeting?.recording || store.watching)) b.append(h('span', { class: 'live-dot', title: store.meeting?.recording ? 'Taking notes' : 'Watching your screen' }));
    nav.append(b);
  }
}

// Fetch layout-correct labels for the keys in use, then redraw anything showing them.
export async function refreshKeyLabels() {
  const hk = store.settings?.hotkeys;
  if (!hk) return;
  const names = [...hk.talk, hk.rambleKey, hk.cancelKey].filter(Boolean);
  try {
    const labels = await invoke('key_labels', { keys: names });
    names.forEach((n, i) => { if (labels?.[i]) keyLabelCache[n] = labels[i]; });
  } catch {}
}

export function renderShortcut() {
  const card = document.getElementById('shortcutCard');
  if (!store.settings) return;
  const hk = store.settings.hotkeys;
  card.innerHTML = '';
  const active = store.settings.modes?.active || '';
  const modeName = active ? (store.modeNames?.[active] || active) : 'Auto';
  const ramble = `Add ${keyLabel(hk.rambleKey)} to ramble.`;
  card.append(
    h('div', { class: 'eyebrow' }, 'Hold to talk'),
    h('div', { class: 'keys' }, keycaps(hk.talk)),
    h('div', { class: 'hint' }, hk.doubleTapHandsFree ? `Double-tap for hands-free. ${ramble}` : ramble),
    h('button', { class: 'mode-line', title: 'Switch mode', onClick: () => go('modes') }, ico('modes', 14), 'Mode: ', h('b', {}, modeName)),
  );
}

// Mode names for the sidebar's "Mode: …" line.
async function refreshModeNames() {
  try {
    const list = await invoke('list_modes');
    store.modeNames = Object.fromEntries(list.map((m) => [m.id, m.name]));
  } catch {}
}

function renderStatus() {
  const e = store.engine || {};
  const dot = document.getElementById('statusDot');
  const text = document.getElementById('statusText');
  dot.className = 'status-dot ' + (e.state === 'ready' ? 'ready' : e.state === 'starting' ? 'busy' : 'bad');
  const sub = document.getElementById('statusSub');
  const model = prettyModel(e.model);
  text.textContent = e.state === 'ready' ? 'Ready' : e.state === 'starting' ? 'Starting' : 'Needs attention';
  sub.textContent = e.state === 'ready' ? `${model} · ${e.device === 'cuda' ? 'GPU' : 'CPU'}` : e.message || 'Speech engine not ready';
}

export function prettyModel(id) {
  if (!id) return 'Whisper';
  const map = { 'large-v3-turbo': 'Whisper Turbo', 'large-v3-turbo-q5_0': 'Whisper Turbo compact', 'small.en': 'Whisper Small', 'base.en': 'Whisper Base' };
  return map[id] || id;
}

// Bumped on every navigation, so a page still rendering when you click elsewhere knows it lost.
let navSeq = 0;

export async function go(id, arg) {
  if (!loaders[id]) id = 'home';
  const seq = ++navSeq;
  const hash = arg ? `#${id}/${arg}` : `#${id}`;
  if (location.hash !== hash) history.replaceState(null, '', hash);
  if (current.cleanup) { try { current.cleanup(); } catch {} }
  current = { id, cleanup: null };
  renderNav(id);
  const mod = await loaders[id]();
  if (seq !== navSeq) return;
  const view = h('div', { class: 'view view-enter' });
  const inner = h('div', { class: 'view-inner' });
  view.append(inner);
  main.innerHTML = '';
  main.append(view);
  const cleanup = (await mod.render(inner, { arg, store, go })) || null;
  // Overtaken while rendering: tidy this page away now, or its listeners and timers ran on and
  // the next navigation tidied the wrong page.
  if (seq !== navSeq) { try { cleanup?.(); } catch {} return; }
  current = { id, cleanup };
}

// A small card in the sidebar, above the shortcut, when a new version is ready (update.rs offers
// it; Settings > About has the same button). "Later" hides it until the next version.
let updateCard = null;
const updateLater = new Set();
function renderUpdate(u) {
  const show = u && (u.state === 'available' || u.state === 'downloading') && !updateLater.has(u.version);
  if (!show) { updateCard?.remove(); updateCard = null; return; }
  if (!updateCard) { updateCard = h('div', { class: 'update-card', role: 'status' }); document.getElementById('updateSlot').append(updateCard); }
  const downloading = u.state === 'downloading';
  const pct = Math.round((u.progress || 0) * 100);
  // Nothing downloads until Update now is pressed, so the card mustn't promise a restart.
  updateCard.replaceChildren(...[
    h('span', { class: 'u-ic', html: icon('download', 15) }),
    h('div', { class: 'u-txt' },
      h('b', {}, downloading ? 'Downloading update' : `Navi ${u.version} is available`),
      h('span', {}, downloading ? `${pct}%. Navi restarts when it’s done.` : 'Update now restarts Navi to install it.')),
    downloading ? null : h('button', { class: 'u-x', title: 'Later', html: icon('x', 13, 2), onClick: () => { updateLater.add(u.version); renderUpdate(u); } }),
    downloading
      ? h('div', { class: 'u-bar' }, h('i', { style: { width: `${pct}%` } }))
      : btn('Update now', { size: 'sm', variant: 'primary', onClick: async () => {
        try { await invoke('install_update'); } catch (e) { toast(errText(e), { error: true }); }
      } }),
  ].filter(Boolean));
}

// The Dictionary badge. Views call this after accepting or ignoring a learned word.
export async function refreshSuggestions() {
  try {
    const list = await invoke('list_suggestions');
    store.set({ suggestions: list.length });
    renderNav(current.id);
  } catch {}
}

// The close button says what it will do, as set in Settings > General.
function renderCloseTitle() {
  const toTray = store.settings?.general?.closeToTray !== false;
  document.getElementById('winClose').title = toTray ? 'Close (Navi keeps listening in the tray)' : 'Quit Navi';
}

function wireWindow() {
  document.getElementById('winMin').innerHTML = icon('minimise', 16, 1.6);
  document.getElementById('winMax').innerHTML = icon('maximise', 13, 1.6);
  document.getElementById('winClose').innerHTML = icon('close', 16, 1.6);
  renderCloseTitle();
  document.getElementById('winMin').onclick = () => win.minimize();
  document.getElementById('winMax').onclick = () => win.toggleMaximize();
  // Rust decides: hide to the tray (saying so the first time) or quit, as set in Settings.
  document.getElementById('winClose').onclick = () => invoke('hide_main').catch(() => win.hide());
  document.getElementById('statusRow').onclick = () => go('settings', 'engine');
}

// A button whose action fails without handling it itself still says what went wrong, rather than
// doing nothing.
window.addEventListener('unhandledrejection', (e) => {
  const r = e.reason;
  if (r && (r.name === 'AbortError' || r.name === 'NotAllowedError')) return; // a sound cut short
  e.preventDefault();
  console.warn('Not handled:', r);
  toast(errText(r), { error: true });
});

async function boot() {
  wireWindow();
  const [info, settings] = await Promise.all([invoke('app_info'), invoke('get_settings')]);
  store.set({ info, settings, engine: info?.engine || store.engine });
  applyTheme(settings.general?.theme);
  await Promise.all([refreshKeyLabels(), refreshModeNames()]);
  renderShortcut();
  renderStatus();
  renderCloseTitle();
  store.on(renderStatus);
  store.on(renderCloseTitle);

  listen('engine', (e) => store.set({ engine: e }));
  // History keeps its search, filters and playback, so it refreshes itself (history.js).
  listen('take', () => { if (current.id === 'home') go('home'); });
  // A mode was made, renamed or deleted, or came or went with a pack.
  listen('modes', async () => { await refreshModeNames(); renderShortcut(); });
  listen('suggestion', () => refreshSuggestions());
  // A dot by Notes while Navi is taking notes on a call.
  const meetingChanged = (m) => { const was = !!store.meeting?.recording; store.meeting = m; if (was !== !!m?.recording) renderNav(current.id); };
  listen('meeting', meetingChanged);
  invoke('meeting_status').then(meetingChanged).catch(() => {});
  // The same dot while Navi watches the screen ("Navi, watch", the tray or the Notes page).
  const watchChanged = (w) => { const was = !!store.watching; store.watching = !!w?.watching; if (was !== store.watching) renderNav(current.id); };
  listen('watch', watchChanged);
  invoke('watch_status').then(watchChanged).catch(() => {});
  listen('navigate', (where) => { const [id, arg] = String(where).split('/'); go(id, arg); });
  listen('update', renderUpdate);
  invoke('update_status').then(renderUpdate).catch(() => {});
  listen('settings-changed', async () => { store.set({ settings: await invoke('get_settings') }); applyTheme(store.settings.general?.theme); await Promise.all([refreshKeyLabels(), refreshModeNames()]); renderShortcut(); });

  refreshSuggestions();

  if (!settings.onboarded || (!isTauri && location.search.includes('onboarding'))) {
    const onb = await import('./views/onboarding.js');
    await onb.run({ store, go });
  }
  const [id, arg] = (location.hash.replace('#', '') || 'home').split('/');
  go(id, arg);
}

window.addEventListener('keydown', (e) => {
  if ((e.ctrlKey && e.key === 'r') || e.key === 'F5') { if (isTauri) e.preventDefault(); }
});
document.addEventListener('contextmenu', (e) => { if (isTauri && !e.target.closest('input, textarea, .selectable')) e.preventDefault(); });

boot().catch((e) => {
  console.error(e);
  main.innerHTML = '';
  main.append(h('div', { class: 'view' }, h('div', { class: 'empty' }, h('div', { class: 'h2' }, 'Navi could not start its window'), h('p', {}, String(e)))));
});
