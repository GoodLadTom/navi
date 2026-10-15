// The shortcut picker, shared by setup and Settings: the shortcut as big keys that press down as
// Navi sees them, quick picks (one key or two), "Any key", plain warnings, and a note when another
// dictation app is running. Pressing the shortcut here is a real take, as it is in any other app:
// the bubble shows at the bottom of the screen and the picker says what Navi heard.
import { invoke, listen } from './bridge.js';
import { h, ico, btn, keycaps, keyLabelCache, errText } from './ui.js';

const PICKS = [
  { keys: ['RCtrl'], note: 'One key, right hand' },
  { keys: ['RAlt'], note: 'One key, by the space bar' },
  { keys: ['CapsLock'], note: 'One key, left hand' },
  { keys: ['Ctrl', 'Win'], note: 'Two keys, bottom left' },
  { keys: ['Insert'], note: 'One key you rarely use' },
];

const SIDES = { Ctrl: ['LCtrl', 'RCtrl'], Shift: ['LShift', 'RShift'], Alt: ['LAlt', 'RAlt'], Win: ['LWin', 'RWin'] };

const same = (a, b) => a.length === b.length && a.every((k, i) => k === b[i]);
// Does a key the hook saw ("LCtrl") press this key of the shortcut ("Ctrl", "RCtrl", "F9")?
const presses = (shortcutKey, seen) => shortcutKey === seen || (SIDES[shortcutKey] || []).includes(seen);
const isWin = (k) => k === 'Win' || k === 'LWin' || k === 'RWin';
const joinNames = (list) => (list.length < 2 ? list[0] : `${list.slice(0, -1).join(', ')} and ${list[list.length - 1]}`);

// mode 'auto': the keys light up while the picker is on screen (setup). 'button': a Test button
// lights them up for a short while (Settings, where you may be typing into other fields).
export function shortcutPicker({ store, mode = 'auto' }) {
  const S = () => store.settings;
  const root = h('div', { class: 'sc' });
  const now = h('div', { class: 'sc-now' });
  const heard = h('span', { class: 'sc-heard' }, ico('check', 14, 2.4), 'Navi heard it');
  const picks = h('div', { class: 'sc-picks' });
  const test = h('div', { class: 'sc-test' });
  const result = h('p', { class: 'sc-result' });
  const msg = h('p', { class: 'sc-msg' });
  const note = h('p', { class: 'sc-note' });
  const hint = h('p', { class: 'sc-note' });
  root.append(now, picks, test, result, msg, hint, note);

  const held = new Set();
  let caps = [];
  let live = false;
  let heardOnce = false;
  let partials = 0;
  let sawWin = false;
  let refresh = null;
  let stopTimer = null;
  let capturing = false;
  let disposed = false;
  const unsubs = [];

  function showMsg(text, tone = 'warn') {
    msg.textContent = text || '';
    msg.className = `sc-msg ${tone}`;
  }

  function showResult(text, tone = '') {
    result.textContent = text || '';
    result.className = `sc-result ${tone}`;
  }

  function drawNow() {
    const row = keycaps(S().hotkeys.talk, 'lg');
    caps = [...row.querySelectorAll('.kbd')];
    now.replaceChildren(row, heard);
    paint();
  }

  function paint() {
    const keys = S().hotkeys.talk;
    caps.forEach((el, i) => el.classList.toggle('down', [...held].some((n) => presses(keys[i], n))));
    if (mode === 'button' && !live) {
      test.replaceChildren(btn('Test it', { size: 'sm', icon: 'keyboard', onClick: () => arm() }), h('span', {}, 'See your keys light up as Navi hears them.'));
      return;
    }
    const extra = [...held].filter((n) => !keys.some((k) => presses(k, n)));
    const parts = [];
    if (heardOnce) parts.push(h('span', {}, 'That’s the one. Hold it to talk, let go to finish.'));
    else parts.push(h('span', {}, 'Try it: hold it and say a few words. The bubble appears at the bottom of your screen.'));
    if (extra.length) parts.push(h('span', { class: 'sc-extra' }, 'Also held:', keycaps(extra)));
    test.replaceChildren(...parts);
  }

  function drawPicks() {
    const current = S().hotkeys.talk;
    picks.replaceChildren();
    for (const p of PICKS) {
      picks.append(h('button', { class: `sc-pick ${same(p.keys, current) ? 'on' : ''}`, onClick: () => apply(p.keys) },
        keycaps(p.keys), h('span', { class: 'note' }, p.note)));
    }
    const custom = !PICKS.some((p) => same(p.keys, current));
    const any = h('button', { class: `sc-pick any ${custom ? 'on' : ''}` },
      custom ? keycaps(current) : h('span', { class: 'any-t' }, ico('keyboard', 16), 'Any key'),
      h('span', { class: 'note' }, custom ? 'Your own' : 'Press the key you want'));
    any.addEventListener('click', () => record(any));
    picks.append(any);
  }

  async function apply(keys) {
    let check;
    try { check = await invoke('check_shortcut', { keys }); } catch (e) { showMsg(errText(e), 'error'); return; }
    if (check?.error) { showMsg(check.error, 'error'); return; }
    (check?.labels || []).forEach((label, i) => { if (label && keys[i]) keyLabelCache[keys[i]] = label; });
    if (!(await store.saveSettings((n) => { n.hotkeys.talk = keys; }))) return;
    showMsg(check?.warning || '', 'warn');
    showResult('');
    heardOnce = false;
    partials = 0;
    sawWin = false;
    hint.textContent = '';
    heard.classList.remove('on');
    drawNow();
    drawPicks();
  }

  async function record(card) {
    if (capturing) return;
    capturing = true;
    card.classList.add('listening');
    card.replaceChildren(h('span', { class: 'any-t' }, 'Press your keys…'), h('span', { class: 'note' }, 'Then let go. Esc backs out.'));
    try {
      const keys = await invoke('capture_shortcut');
      if (keys?.length && !disposed) await apply(keys);
    } catch (e) {
      const text = errText(e);
      if (text !== 'Cancelled.' && !disposed) showMsg(text, 'error');
    }
    capturing = false;
    if (!disposed) drawPicks();
  }

  function onKey(k) {
    if (!k?.name) return;
    if (k.label) keyLabelCache[k.name] = k.label;
    if (k.up) held.delete(k.name); else held.add(k.name);
    if (!k.up && isWin(k.name)) sawWin = true;
    paint();
  }

  function onShortcut(state) {
    if (state === 'down') {
      heardOnce = true;
      heard.classList.remove('on'); void heard.offsetWidth; heard.classList.add('on');
      hint.textContent = '';
      showResult('');
      if (mode === 'button') { clearTimeout(stopTimer); stopTimer = setTimeout(disarm, 8000); }
    } else if (state === 'up') {
      held.clear();
    }
    paint();
  }

  // Pressing every key of the shortcut but Win, again and again, usually means a Win Lock.
  function watchForWinLock() {
    const keys = S().hotkeys.talk;
    if (heardOnce || sawWin || !keys.some(isWin)) return;
    const others = keys.filter((k) => !isWin(k));
    if (others.length && others.every((k) => [...held].some((n) => presses(k, n)))) {
      partials += 1;
      if (partials >= 2) hint.textContent = 'Navi hasn’t seen the Windows key. Some gaming keyboards have a Win Lock or game mode that hides it from apps. A one-key shortcut such as Right Ctrl avoids it.';
    }
  }

  async function arm() {
    if (live || disposed) return;
    live = true;
    try { await invoke('key_monitor', { on: true }); } catch (e) { live = false; showMsg(errText(e), 'error'); paint(); return; }
    // Closed while waiting: make sure the key display is off again, and set no timer.
    if (!live || disposed) { invoke('key_monitor', { on: false }).catch(() => {}); return; }
    refresh = setInterval(() => { if (live && !disposed) invoke('key_monitor', { on: true }).catch(() => {}); }, 20000);
    if (mode === 'button') { clearTimeout(stopTimer); stopTimer = setTimeout(disarm, 30000); }
    paint();
  }

  function disarm() {
    if (!live) return;
    live = false;
    clearInterval(refresh);
    clearTimeout(stopTimer);
    held.clear();
    invoke('key_monitor', { on: false }).catch(() => {});
    paint();
  }

  (async () => {
    unsubs.push(await listen('key-seen', (k) => { onKey(k); if (!k.up) watchForWinLock(); }));
    unsubs.push(await listen('shortcut', onShortcut));
    unsubs.push(await listen('settings-changed', () => { drawNow(); drawPicks(); }));
    // What the test take came to: the words, or why there were none.
    unsubs.push(await listen('take', (t) => {
      if (!t?.finalText) return;
      const words = t.finalText.length > 90 ? `${t.finalText.slice(0, 90)}…` : t.finalText;
      showResult(`Navi heard: “${words}”`, 'ok');
    }));
    unsubs.push(await listen('pill', (p) => { if (p?.state === 'error' && heardOnce) showResult(`${p.message}. Hold the key down while you talk.`, 'warn'); }));
    // Left already (a very quick Continue): undo the listeners and never turn the display on.
    if (disposed) { unsubs.splice(0).forEach((f) => { try { f(); } catch {} }); return; }
    // The keys only light up while you are in this window.
    const onBlur = () => disarm();
    const onFocus = () => { if (mode === 'auto') arm(); };
    window.addEventListener('blur', onBlur);
    window.addEventListener('focus', onFocus);
    unsubs.push(() => { window.removeEventListener('blur', onBlur); window.removeEventListener('focus', onFocus); });
    if (mode === 'auto') arm();
    const check = await invoke('check_shortcut', { keys: S().hotkeys.talk }).catch(() => null);
    if (check?.labels) { check.labels.forEach((label, i) => { const k = S().hotkeys.talk[i]; if (label && k) keyLabelCache[k] = label; }); drawNow(); drawPicks(); }
    if (check?.warning) showMsg(check.warning, 'warn');
    const apps = await invoke('other_dictation_apps').catch(() => []);
    if (apps?.length) {
      const many = apps.length > 1;
      note.textContent = `${joinNames(apps)} ${many ? 'are' : 'is'} also running. If ${many ? 'they use' : 'it uses'} the same keys, only one app will hear them.`;
    }
  })();

  drawNow();
  drawPicks();

  return {
    el: root,
    cleanup() {
      disposed = true;
      disarm();
      // A half-made recording must not pick up the next keys you press somewhere else.
      if (capturing) invoke('cancel_shortcut_capture').catch(() => {});
      unsubs.splice(0).forEach((f) => { try { f(); } catch {} });
    },
  };
}
