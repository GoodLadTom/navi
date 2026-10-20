// The sound set picker, shared by setup and Settings: a wrapping grid of soft, low sound sets.
// Clicking a set picks it and plays its start sound; the play button previews start then done.
import { invoke } from './bridge.js';
import { h, ico } from './ui.js';

const CUES = [['start', 'Start'], ['stop', 'Stop'], ['done', 'Done'], ['ramble', 'Ramble'], ['cancel', 'Cancel'], ['error', 'Error'], ['learn', 'Learned']];

let cached = null;
export async function soundSets() {
  if (!cached) cached = await invoke('list_sound_sets').catch(() => null);
  return cached || { default: 'hush', sets: [] };
}

// Plays a cue the way Navi does: volume squared, which follows how loud it sounds.
export function playCue(store, set, cue) {
  const s = store.settings.sounds;
  if (s && s.enabled === false) return;
  const a = new Audio(`sounds/sets/${set}/${cue}.wav`);
  const v = Math.max(0, Math.min(1, s?.volume ?? 0.7));
  a.volume = v * v;
  a.play().catch(() => {});
}

export function soundPicker({ store, showCues = false, compact = false }) {
  const root = h('div', { class: `sp ${compact ? 'compact' : ''}` });
  const grid = h('div', { class: 'sp-grid' });
  const cuesRow = h('div', { class: 'sp-cues' });
  root.append(grid);
  if (showCues) root.append(h('div', { class: 'h3', style: { margin: '18px 0 10px' } }, 'This set’s sounds'), cuesRow);
  let data = { default: 'hush', sets: [] };
  let timers = [];

  const current = () => {
    const id = store.settings.sounds?.set;
    return data.sets.some((s) => s.id === id) ? id : data.default;
  };

  function preview(id) {
    timers.forEach(clearTimeout);
    playCue(store, id, 'start');
    timers = [setTimeout(() => playCue(store, id, 'done'), 650)];
  }

  async function choose(id) {
    if (id !== current()) await store.saveSettings((n) => { n.sounds.set = id; });
    draw();
    playCue(store, id, 'start');
  }

  function draw() {
    const on = current();
    grid.replaceChildren(...data.sets.map((s) => {
      const card = h('div', { class: `sp-card ${s.id === on ? 'on' : ''}`, role: 'button', tabindex: '0', onClick: (e) => { if (!e.target.closest('.sp-play')) choose(s.id); } },
        h('div', { class: 'sp-top' }, h('span', { class: 'nm' }, s.name),
          h('button', { class: 'sp-play', title: `Hear ${s.name}`, onClick: () => preview(s.id) }, ico('play', 11))),
        h('div', { class: 'small faint' }, s.description));
      card.addEventListener('keydown', (e) => { if (e.key === 'Enter') choose(s.id); });
      return card;
    }));
    if (showCues) {
      const cueBtn = ([cue, label]) => h('button', { class: 'cue', onClick: () => playCue(store, current(), cue) }, h('span', { class: 'pb' }, ico('play', 11)), label);
      // Two groups (4 and 3) that wrap as a whole, so a narrow card shows two even lines rather
      // than one cue alone on a second line.
      cuesRow.replaceChildren(h('div', { class: 'sp-cue-group' }, CUES.slice(0, 4).map(cueBtn)), h('div', { class: 'sp-cue-group' }, CUES.slice(4).map(cueBtn)));
    }
  }

  soundSets().then((d) => { data = d; draw(); });
  return { el: root, cleanup() { timers.forEach(clearTimeout); } };
}
