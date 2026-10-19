import { invoke, listen, pickFile, openPath, isTauri } from '../bridge.js';
import { h, ico, btn, chip, toggle, select, seg, setRow, field, keycaps, toast, confirmDialog, errText, debounce } from '../ui.js';
import { prettyModel } from '../app.js';
import { shortcutPicker } from '../shortcut.js';
import { soundPicker, playCue } from '../soundpicker.js';

const SECTIONS = [
  ['general', 'General'], ['shortcuts', 'Shortcuts'], ['commands', 'Voice commands'], ['sound', 'Sound'], ['mic', 'Microphone'], ['engine', 'Speech engine'],
  ['ai', 'AI'], ['voice', 'Voice'], ['writing', 'Writing'], ['privacy', 'Context and privacy'], ['meetings', 'Meeting notes'], ['integrations', 'Notes and tasks'], ['about', 'About'],
];

const MODEL_HINTS = {
  anthropic: ['claude-haiku-4-5-20251001', 'claude-sonnet-5-5', 'claude-opus-5-5'],
  openai: ['gpt-5-mini', 'gpt-5'],
  ollama: ['llama3.2', 'qwen3:8b', 'gemma3'],
  custom: [],
};

// Settings > Voice commands. Rows are named as types.rs voice_action_label names the actions.
// Said on their own while you dictate, rather than straight after the wake word.
const IN_TAKE = ['cancel', 'finish'];
// Wake words common enough that Navi may hear them when nobody meant to wake it.
const COMMON_WAKE = ['hey', 'hi', 'hello', 'ok', 'okay', 'right', 'so', 'computer'];
// Two phrases are the same command when their keys match ("Don't worry!" and "dont worry"). Keep in
// step with phrase_key in src-tauri/src/types.rs.
const vkey = (s) => String(s ?? '').toLowerCase().replace(/['’‘]/g, '').replace(/[^\p{L}\p{N}]+/gu, ' ').trim();
const vcEntry = (vc, action) => (vc?.commands || []).find((c) => c.action === action);
const vcPhrases = (vc, action) => (vcEntry(vc, action)?.phrases || []).filter((p) => vkey(p));
// The wake word as the page quotes it: the first one, or "Navi" (types.rs wake_label).
const wakeLabel = (vc) => vcPhrases(vc, 'wake')[0] || 'Navi';
// How to say a command: "Navi, stop watching" (types.rs say). Null when the command is off.
const sayFor = (vc, action) => {
  const first = vcPhrases(vc, action)[0];
  if (!first) return null;
  return action === 'wake' ? wakeLabel(vc) : `${wakeLabel(vc)}, ${first}`;
};
// The words for jobs on calls: jobs.rs takes "Navi" alone only while it is a wake word
// (types.rs jobs_name_only), and otherwise wants "Hey Navi".
const jobsWake = (vc) => (vcPhrases(vc, 'wake').some((p) => vkey(p) === 'navi') ? 'Navi' : 'Hey Navi');
// A wake word short or common enough to set Navi off by accident. Navi's own name is what the
// listener is tuned for, so it never gets the warning.
const commonWake = (p) => {
  const k = vkey(p);
  return k !== 'navi' && ((!k.includes(' ') && [...k].length <= 4) || COMMON_WAKE.includes(k));
};

// Model downloads carry on when you leave Settings, so their progress is kept here rather than in
// the page. Coming back mid-download shows the bar again, not a second Download button.
const progress = {};
let redrawModels = null;
listen('download', (p) => { progress[p.id] = p; redrawModels?.(); }).catch(() => {});

export async function render(root, { store, arg }) {
  const cleanups = [];
  const S = () => store.settings;
  const save = (fn) => store.saveSettings(fn);

  root.append(h('div', { class: 'page-head' },
    h('div', {}, h('div', { class: 'eyebrow' }, 'Make it yours'), h('h1', { class: 'h1' }, 'Settings'))));

  const subnav = h('nav', { class: 'subnav' });
  const content = h('div', {});
  root.append(h('div', { class: 'settings' }, subnav, content));
  const sectionEls = {};

  const section = (id, title, lead, ...rows) => {
    const el = h('div', { class: 'section', id: `s-${id}` },
      h('div', { class: 'card' }, h('div', { class: 'h2' }, title), lead ? h('p', { class: 'small quiet', style: { margin: '4px 0 12px' } }, lead) : h('div', { style: { height: '10px' } }), ...rows));
    sectionEls[id] = el;
    content.append(el);
    return el;
  };

  // ---------------------------------------------------------------- general
  const nameInput = h('input', { class: 'input', style: { width: '220px' }, value: S().userName || '' });
  nameInput.addEventListener('change', () => save((n) => { n.userName = nameInput.value.trim(); }));
  section('general', 'General', null,
    setRow('Appearance', 'Light, dark, or the same as Windows.', seg([{ value: 'light', label: 'Light' }, { value: 'dark', label: 'Dark' }, { value: 'system', label: 'Match Windows' }], S().general.theme || 'light', (v) => save((n) => { n.general.theme = v; }))),
    setRow('Your name', 'For the greeting on the home page, and so your AI spells your name right.', nameInput),
    setRow('Start Navi when Windows starts', 'It sits in the tray, ready for your shortcut.', toggle(S().general.launchAtLogin, (v) => save((n) => { n.general.launchAtLogin = v; }))),
    setRow('Open in the tray', 'When Windows starts Navi, it stays in the tray without opening this window. Opening Navi yourself always shows it.', toggle(S().general.startMinimised, (v) => save((n) => { n.general.startMinimised = v; }))),
    setRow('Show the bubble', 'The small pill at the bottom of the screen while you talk.', toggle(S().general.showPill, (v) => save((n) => { n.general.showPill = v; }))),
    setRow('Show the firefly', 'The little firefly that flies in at the top right while you talk. Turn it off if you would rather not see it.', toggle(S().general.showFirefly !== false, (v) => save((n) => { n.general.showFirefly = v; }))),
    setRow('How text goes in', 'Paste is instant. Type works in the rare app that blocks pasting.', seg([{ value: 'paste', label: 'Paste' }, { value: 'type', label: 'Type' }], S().general.pasteMethod, (v) => save((n) => { n.general.pasteMethod = v; }))),
    setRow('Copy what I say to the clipboard', 'Off: whatever you had copied is put back, so a link you copied is still there to paste after you talk. On: what you said stays on the clipboard, ready for Ctrl + V.', toggle(!S().general.restoreClipboard, (v) => save((n) => { n.general.restoreClipboard = !v; }))),
    setRow('The close button', 'Keep Navi in the tray, listening for your shortcut, or quit it completely.', seg([{ value: 'tray', label: 'Keep in tray' }, { value: 'quit', label: 'Quit Navi' }], S().general.closeToTray === false ? 'quit' : 'tray', (v) => save((n) => { n.general.closeToTray = v === 'tray'; }))),
    setRow('Quit Navi', 'Stops listening until you open Navi again. The tray icon’s menu has this too.', btn('Quit', { size: 'sm', icon: 'close', onClick: () => invoke('quit_app') })),
  );

  // ---------------------------------------------------------------- shortcuts
  const picker = shortcutPicker({ store, mode: 'button' });
  cleanups.push(() => picker.cleanup());

  // Mode keys: one key per mode, plus Auto and "next mode". The bubble says which mode is on.
  const allModes = (await invoke('list_modes').catch(() => [])).filter((m) => m.kind !== 'notes');
  const modeKeysBlock = h('div', { class: 'set-row', style: { display: 'block' } });
  let capturingMode = null;
  const keyEntry = (mode) => (S().hotkeys.modeKeys || []).find((k) => k.mode === mode);
  const saveModeKey = (mode, keys) => save((n) => {
    const list = (n.hotkeys.modeKeys || []).filter((k) => k.mode !== mode);
    if (keys?.length) list.push({ mode, keys });
    n.hotkeys.modeKeys = list;
  });
  async function setModeKey(mode) {
    if (capturingMode) return;
    capturingMode = mode; drawModeKeys();
    try {
      const keys = await invoke('capture_shortcut');
      const check = await invoke('check_mode_key', { keys, mode });
      if (check?.error) toast(check.error, { error: true });
      else { await saveModeKey(mode, keys); if (check?.warning) toast(check.warning); }
    } catch (e) { const t = errText(e); if (t !== 'Cancelled.') toast(t, { error: true }); }
    capturingMode = null; drawModeKeys();
  }
  async function useFKeys() {
    const order = ['raw', 'clean', 'message', 'email', 'prompt'].filter((id) => allModes.some((m) => m.id === id));
    await save((n) => {
      const keep = (n.hotkeys.modeKeys || []).filter((k) => !order.includes(k.mode) && !/^F[1-5]$/.test((k.keys || []).join('')));
      n.hotkeys.modeKeys = [...keep, ...order.map((mode, i) => ({ mode, keys: [`F${i + 1}`] }))];
    });
    drawModeKeys();
    toast('F1 to F5 now switch modes');
  }
  function drawModeKeys() {
    const rows = [{ id: 'auto', name: 'Auto', note: 'Picks from the app' }, ...allModes.map((m) => ({ id: m.id, name: m.name })), { id: 'next', name: 'Next mode', note: 'Steps through them all' }];
    modeKeysBlock.replaceChildren(
      h('div', { class: 'row between', style: { marginBottom: '6px' } },
        h('div', {}, h('div', { class: 'h3' }, 'Mode keys'), h('p', { class: 'small quiet', style: { marginTop: '3px', maxWidth: '520px' } }, 'Press one anywhere to switch mode. The bubble shows which mode you are on. While Navi runs, a key you pick here stops doing its usual job (F1 opens Help, F5 refreshes).')),
        btn('Use F1 to F5', { size: 'sm', variant: 'ghost', icon: 'keyboard', onClick: useFKeys })),
      h('div', { class: 'mode-keys' }, rows.map((r) => {
        const entry = keyEntry(r.id);
        const busy = capturingMode === r.id;
        return h('div', { class: 'mode-key-row' },
          h('div', {}, h('span', { class: 'nm' }, r.name), r.note ? h('span', { class: 'small faint' }, ` · ${r.note}`) : null),
          h('div', { class: 'row', style: { gap: '6px' } },
            busy ? h('span', { class: 'chip accent' }, 'Press a key…') : entry ? keycaps(entry.keys) : h('span', { class: 'small faint' }, 'No key'),
            btn(entry ? 'Change' : 'Set', { size: 'sm', variant: 'ghost', disabled: !!capturingMode, onClick: () => setModeKey(r.id) }),
            entry && !busy ? btn('', { size: 'sm', variant: 'ghost', icon: 'x', title: 'Remove', onClick: async () => { await saveModeKey(r.id, null); drawModeKeys(); } }) : null));
      })));
  }
  drawModeKeys();
  cleanups.push(() => { if (capturingMode) invoke('cancel_shortcut_capture').catch(() => {}); });
  section('shortcuts', 'Shortcuts', 'Hold to talk, let go to finish. Works in almost every app: one running as administrator gets your words on the clipboard to paste instead.',
    h('div', { class: 'set-row', style: { display: 'block' } }, h('div', { class: 'h3', style: { marginBottom: '12px' } }, 'Talk'), picker.el),
    setRow('Hands-free', 'Double-tap the talk keys to keep listening without holding. Press Enter, or tap the keys again, to finish.', toggle(S().hotkeys.doubleTapHandsFree, (v) => save((n) => { n.hotkeys.doubleTapHandsFree = v; }))),
    setRow('Ramble', 'While holding the talk keys, tap this to switch to notes mode. A different sound plays and Navi keeps listening until you press Enter (or tap the talk keys again), then saves it to Notes.', select(['Space', 'Enter', 'Tab', 'N', 'R'], S().hotkeys.rambleKey, (v) => save((n) => { n.hotkeys.rambleKey = v; }))),
    setRow('Cancel', 'Throws the take away while it is recording.', keycaps([S().hotkeys.cancelKey])),
    modeKeysBlock,
    setRow('Voice edits', 'Start with “Navi”: “Navi, scratch that”, “Navi, make that shorter”, “Navi, change Tuesday to Wednesday”. Select text first and say “Navi, turn this into bullet points”, or “Navi, what’s wrong with this?” for a blunt list of its problems. “Scratch that” works without an AI, and so does “change Tuesday to Wednesday” when Navi finds the word. Shorter, bullet points and “what’s wrong” need an AI (set one up under AI).', chip('Always on', 'accent', true)),
  );

  // ---------------------------------------------------------------- voice commands
  const watchNote = h('span', { class: 'small faint' });
  const watchBtn = btn('Start watching', { size: 'sm', icon: 'play' });
  const drawWatch = (w) => {
    const on = !!w?.watching;
    watchBtn.lastChild.textContent = on ? 'Stop and write up' : 'Start watching';
    watchNote.textContent = on ? `Watching · ${w.shots || 0} screenshot${w.shots === 1 ? '' : 's'} so far` : '';
  };
  watchBtn.onclick = async () => {
    const now = await invoke('watch_status').catch(() => null);
    try { await invoke(now?.watching ? 'stop_watch' : 'start_watch'); } catch (e) { toast(errText(e), { error: true }); }
    drawWatch(await invoke('watch_status').catch(() => null));
  };
  invoke('watch_status').then(drawWatch).catch(() => {});
  listen('watch', drawWatch).then((off) => cleanups.push(off)).catch(() => {});

  // Each action lists the phrases that set it off, as chips. Every save goes through Rust's checks
  // (types.rs normalised) and the section redraws from what comes back, so a refused phrase never
  // shows as a chip and a failed save puts the chips back as they were.
  const VC = () => S().voiceCommands || { enabled: false, commands: [] };
  // The entry for `action` in settings being saved, made if it is missing.
  const entryIn = (n, action) => {
    n.voiceCommands = n.voiceCommands || { enabled: false, commands: [] };
    n.voiceCommands.commands = n.voiceCommands.commands || [];
    let e = n.voiceCommands.commands.find((c) => c.action === action);
    if (!e) { e = { action, phrases: [] }; n.voiceCommands.commands.push(e); }
    e.phrases = e.phrases || [];
    return e;
  };
  // Places that quote the words for jobs on calls; drawVoice keeps them in step with the wake words.
  const jobsSpans = [];
  const jobsWk = () => { const s = h('span', {}, jobsWake(VC())); jobsSpans.push(s); return s; };
  // The lead's example command follows the phrases too. With Watch my screen off, another command stands in.
  const vcExample = h('span', {});
  const example = (vc, order) => order.map((a) => sayFor(vc, a)).find(Boolean) || `${wakeLabel(vc)}, watch my screen`;
  const vcHost = h('div', {});
  const vcInputs = {};
  let vcDrawn = '';
  const vcToggle = toggle(!!VC().enabled, async (v) => {
    await save((n) => { n.voiceCommands = { ...(n.voiceCommands || { commands: [] }), enabled: v }; });
    drawVoice();
  });
  const addPhrase = async (action) => {
    const input = vcInputs[action];
    const typed = input.value.trim();
    if (!vkey(typed)) { input.value = ''; return; }
    const vc = VC();
    if (vcPhrases(vc, action).some((p) => vkey(p) === vkey(typed))) { toast('Already there'); return; }
    // A phrase another command has is refused by Rust (types.rs normalised_from), which names that
    // command after taking a wake word off the front and hearing "do not" as "don't", as the
    // listener does. Its message comes back as the toast.
    if (await save((n) => { entryIn(n, action).phrases.push(typed); })) vcInputs[action].value = '';
    drawVoice();
  };
  const removePhrase = async (action, phrase) => {
    await save((n) => { const e = entryIn(n, action); e.phrases = e.phrases.filter((p) => vkey(p) !== vkey(phrase)); });
    drawVoice();
  };
  const vcRow = (vc, action, name, desc) => {
    const phrases = vcPhrases(vc, action);
    const first = phrases[0];
    // The last wake word can't go: without one, nothing would start a take.
    const lastWake = action === 'wake' && phrases.length === 1;
    const chips = phrases.map((p) => h('span', { class: 'chip phrase', title: lastWake ? 'Keep at least one wake word' : null }, `“${p}”`,
      lastWake ? null : h('button', { html: '&times;', title: 'Remove', 'aria-label': `Remove “${p}”`, onClick: () => removePhrase(action, p) })));
    const input = h('input', { class: 'input vc-add', placeholder: 'Add a phrase', 'aria-label': `Add a phrase for ${name}`, spellcheck: 'false' });
    input.addEventListener('keydown', (e) => {
      if (e.isComposing || (e.key !== 'Enter' && e.key !== ',')) return;
      e.preventDefault();
      addPhrase(action);
    });
    vcInputs[action] = input;
    const say = !first ? h('div', { class: 'vc-off' }, 'Off. Add a phrase to use it.')
      : h('div', { class: 'vc-say' }, action === 'wake' ? `Say “${first}” and keep talking` : IN_TAKE.includes(action) ? `Say “${first}” on its own` : `Say “${sayFor(vc, action)}”`);
    const cancel = vcPhrases(vc, 'cancel')[0];
    const warns = action === 'wake' ? phrases.filter(commonWake).map((p) => h('div', { class: 'vc-warn' },
      `“${p}” is short and common, so Navi may wake when you didn’t mean it to. ${cancel ? `Say “${cancel}” or press Esc to send it away.` : 'Press Esc to send it away.'}`)) : [];
    return h('div', { class: 'vc-row' },
      h('div', {}, h('div', { class: 'nm' }, name), h('div', { class: 'ds' }, desc)),
      h('div', {}, h('div', { class: 'vc-phrases' }, chips, input), say, warns));
  };
  const vcGroup = (title, help, ...rows) => h('div', { class: 'vc-group' }, h('div', { class: 'h3' }, title), h('p', {}, help), h('div', {}, rows));
  function drawVoice() {
    const vc = VC();
    // A phrase half typed, and the box the cursor was in, survive the redraw.
    const typed = Object.fromEntries(Object.entries(vcInputs).map(([a, el]) => [a, el.value]));
    const focused = Object.keys(vcInputs).find((a) => vcInputs[a] === document.activeElement);
    vcDrawn = JSON.stringify(vc);
    vcToggle.classList.toggle('on', !!vc.enabled);
    vcToggle.setAttribute('aria-checked', String(!!vc.enabled));
    vcExample.textContent = example(vc, ['watch_on', 'notes_on', 'watch_off', 'notes_off', 'scratch_that']);
    jobsSpans.forEach((s) => { s.textContent = jobsWake(vc); });
    vcHost.replaceChildren(
      vcGroup('Start dictating', `Say it at the start of a sentence, then what you want typed: “${wakeLabel(vc)}, email Sam about Friday.” Navi stops after six seconds of quiet, when you say your finish phrase, or when you press Enter.`,
        vcRow(vc, 'wake', 'Wake word', 'Any word or phrase. Keep at least one.')),
      vcGroup('Straight after the wake word', `Say the wake word first, in the same breath or after a pause: “${example(vc, ['notes_on', 'watch_on', 'notes_off', 'watch_off', 'scratch_that'])}.” These work from the shortcut too.`,
        vcRow(vc, 'scratch_that', 'Scratch that', 'Removes the last thing Navi typed.'),
        vcRow(vc, 'watch_on', 'Watch my screen', 'Looks at your screen every few seconds, then writes up what you did.'),
        vcRow(vc, 'watch_off', 'Stop watching', 'Stops watching and writes it up on the Notes page.'),
        vcRow(vc, 'notes_on', 'Take notes', 'Starts call notes, like Take notes now on the Notes page.'),
        vcRow(vc, 'notes_off', 'Stop taking notes', 'Finishes the notes and writes them up. Works during a call.')),
      vcGroup('While you’re dictating', 'Say it on its own, with a short pause before and after, so “don’t worry about the deadline” is still typed.',
        vcRow(vc, 'cancel', 'Cancel', 'Throws the take away, like Esc. If Navi gets it wrong, the recording is kept in History so you can re-run it. With History off, a long take or a ramble asks you to say it twice.'),
        vcRow(vc, 'finish', 'Finish', 'Stops listening and types what you said, without the phrase.')));
    for (const [a, v] of Object.entries(typed)) if (vcInputs[a]) vcInputs[a].value = v;
    if (focused) vcInputs[focused]?.focus();
  }
  const resetVoice = async () => {
    if (!(await confirmDialog('Reset voice commands?', 'Every phrase goes back to how Navi ships: “Navi” and “Hey Navi” to wake it, “don’t worry” to cancel, “that’s all” to finish.', { ok: 'Reset' }))) return;
    let defaults;
    try { defaults = await invoke('voice_command_defaults'); } catch (e) { toast(errText(e), { error: true }); return; }
    if (await save((n) => { n.voiceCommands = { ...defaults, enabled: !!n.voiceCommands?.enabled }; })) toast('Back to the starting phrases');
    drawVoice();
  };
  drawVoice();
  // The tray's "Listen for" tick changes the settings from outside this page: follow it.
  cleanups.push(store.on(() => { if (JSON.stringify(VC()) !== vcDrawn) drawVoice(); }));
  section('commands', 'Voice commands', ['Talk to Navi without touching the keyboard. Say the wake word and keep talking, and Navi types what you say. Say the wake word and a command, such as “', vcExample, '”, and it happens straight away. Change any phrase to words that suit you.'],
    setRow('Listen for voice commands', ['The microphone stays on so Navi can hear you, and Windows shows its microphone icon. What it hears is checked on this computer and never kept. During call notes Navi only listens for the commands that stop the notes or watch your screen; “', jobsWk(), ', …” on a call still gives your AI a job.'], vcToggle),
    vcHost,
    setRow('Reset to the starting phrases', 'Puts every phrase back to how Navi ships. Your on/off choice stays.', btn('Reset', { size: 'sm', variant: 'ghost', onClick: resetVoice })),
    // Watch my screen, below a divider: its row title is the sub-heading.
    h('div', { class: 'vc-group' },
      setRow('Watch my screen', 'Navi looks at the screen every few seconds and keeps a screenshot only when enough of it has changed. When you stop, your AI picks the ones that matter and writes up what you did, on the Notes page. With no AI, every screenshot is kept, listed by time. Watching stops by itself after an hour.', h('div', { class: 'row' }, watchNote, watchBtn)),
      setRow('How often to look', null, select([{ value: '5', label: 'Every 5 seconds' }, { value: '10', label: 'Every 10 seconds' }, { value: '20', label: 'Every 20 seconds' }, { value: '30', label: 'Every 30 seconds' }], String(S().watch?.everySecs || 10), (v) => save((n) => { n.watch = { ...(n.watch || {}), everySecs: Number(v) }; })))),
    h('p', { class: 'small faint', style: { paddingTop: '14px', borderTop: '1px solid var(--line-soft)' } },
      'Screenshots stay on this computer, in Navi’s data folder. Your AI only gets the text read off them and the window names, never the pictures. Apps under “Never read these apps” (in Context and privacy) are never captured.'),
  );

  // ---------------------------------------------------------------- sound
  const sounds = soundPicker({ store, showCues: true });
  cleanups.push(() => sounds.cleanup());
  const vol = h('input', { type: 'range', class: 'slider', min: '0', max: '100', value: String(Math.round(S().sounds.volume * 100)) });
  vol.style.setProperty('--v', vol.value + '%');
  vol.addEventListener('input', () => vol.style.setProperty('--v', vol.value + '%'));
  vol.addEventListener('change', async () => { await save((n) => { n.sounds.volume = Number(vol.value) / 100; }); playCue(store, S().sounds.set || 'hush', 'done'); });
  section('sound', 'Sound', 'Soft, low sounds tell you when Navi starts and stops listening. Pick the one you like best.',
    h('div', { style: { margin: '6px 0 16px' } }, sounds.el),
    setRow('Sounds', 'Turn every cue off.', toggle(S().sounds.enabled, (v) => save((n) => { n.sounds.enabled = v; }))),
    setRow('Volume', null, vol),
    setRow('While you talk', 'What happens to music and videos playing on this computer while Navi listens. Pause stops them and plays them again when you finish. Mute silences other apps and brings them back at the same volume. Nothing changes while Navi takes call notes.', seg([{ value: 'keep', label: 'Keep playing' }, { value: 'pause', label: 'Pause' }, { value: 'mute', label: 'Mute' }, { value: 'both', label: 'Both' }], S().sounds.whileTalking || 'pause', (v) => save((n) => { n.sounds.whileTalking = v; }))),
    setRow('Your own sounds', 'Use any WAV file for a cue. Only its first 3 seconds play. Built-in sounds are used for the rest.', btn('Choose a file…', { size: 'sm', icon: 'folder', onClick: async () => {
      const path = await pickFile({ filters: [{ name: 'WAV audio', extensions: ['wav'] }] });
      if (!path) return;
      const which = await new Promise((resolve) => {
        const s2 = select([['talk', 'Start'], ['ramble', 'Ramble'], ['stop', 'Stop'], ['done', 'Done'], ['cancel', 'Cancel'], ['error', 'Error'], ['learn', 'Learned']].map(([value, label]) => ({ value, label })), 'talk');
        confirmDialog('Use this sound for…', '', { ok: 'Use it' }).then((ok) => resolve(ok ? s2.value : null));
        setTimeout(() => document.querySelector('.dialog p')?.replaceWith(s2), 0);
      });
      if (!which) return;
      await save((n) => { n.sounds.custom = { ...n.sounds.custom, [which]: path }; });
      toast('Sound set');
    } })),
  );

  // ---------------------------------------------------------------- microphone
  const devices = await invoke('list_devices').catch(() => []);
  const meter = h('div', { class: 'meter', style: { width: '220px' } }, h('i'));
  let testing = false;
  const testBtn = btn('Test', { size: 'sm', icon: 'mic' });
  testBtn.onclick = async () => {
    testing = !testing;
    testBtn.lastChild.textContent = testing ? 'Stop' : 'Test';
    try { await invoke(testing ? 'mic_test_start' : 'mic_test_stop', { device: S().mic.device }); } catch (e) { toast(errText(e), { error: true }); testing = false; testBtn.lastChild.textContent = 'Test'; }
    if (!testing) meter.firstChild.style.width = '0%';
  };
  cleanups.push(await listen('level', (v) => { if (testing) meter.firstChild.style.width = `${Math.round(v * 100)}%`; }));
  cleanups.push(() => { if (testing) invoke('mic_test_stop'); });
  // Pressing the talk shortcut ends a microphone test so the take can have the mic.
  cleanups.push(await listen('mic-test', (on) => { if (!on && testing) { testing = false; testBtn.lastChild.textContent = 'Test'; meter.firstChild.style.width = '0%'; } }));
  section('mic', 'Microphone', null,
    setRow('Microphone', 'Navi uses the Windows default unless you pick one.', select([{ value: '', label: 'Windows default' }, ...devices.map((d) => ({ value: d.name, label: d.name + (d.isDefault ? ' (default)' : '') }))], S().mic.device || '', (v) => save((n) => { n.mic.device = v || null; }))),
    setRow('Check the level', 'Say something: the bar should move most of the way across.', h('div', { class: 'row' }, meter, testBtn)),
    setRow('Ignore the start cue', 'Skips the first moment of the start sound, so laptop speakers never end up in your text.', toggle(S().mic.trimCue, (v) => save((n) => { n.mic.trimCue = v; }))),
  );

  // ---------------------------------------------------------------- engine
  const engineHost = h('div', {});
  const modelsHost = h('div', {});
  const drawEngine = () => {
    const e = store.engine || {};
    engineHost.innerHTML = '';
    engineHost.append(h('div', { class: 'card flat engine-card', style: { background: 'var(--well)', border: 0, marginBottom: '6px' } },
      h('span', { class: 'big-dot' }, ico(e.state === 'ready' ? 'bolt' : 'info', 20)),
      h('div', { style: { flex: 1 } },
        h('div', { class: 'h3' }, e.state === 'ready' ? `${prettyModel(e.model)} is ready` : e.state === 'starting' ? 'Starting the speech engine…' : 'The speech engine needs attention'),
        h('div', { class: 'small quiet' }, e.state === 'ready' ? `Running on ${e.device === 'cuda' ? (e.gpu || 'your graphics card') : 'the processor'}. Everything is transcribed on this computer.` : (e.message || ''))),
      btn('Restart', { size: 'sm', icon: 'rerun', onClick: async () => { try { store.set({ engine: await invoke('restart_engine') }); } catch (err) { toast(errText(err), { error: true }); } } })));
  };
  const drawModels = async () => {
    const models = (await invoke('list_models').catch(() => [])).filter((m) => m.id !== 'vad');
    modelsHost.innerHTML = '';
    for (const m of models) {
      const isActive = S().engine.model === m.id;
      const prog = progress[m.id];
      const right = prog && prog.state !== 'done' && prog.state !== 'error'
        ? h('div', { style: { width: '170px' } }, h('div', { class: 'progress' }, h('i', { style: { width: `${prog.total ? Math.round(prog.received / prog.total * 100) : 2}%` } })), h('div', { class: 'small faint', style: { marginTop: '5px' } }, prog.state === 'downloading' ? `${Math.round(prog.received / 1e6)} of ${Math.round(prog.total / 1e6)} MB` : prog.state))
        : m.installed
          // Set-up can pin a model file, which beat the model picked here: unpin it.
          ? (isActive ? chip('In use', 'accent', true) : btn('Use', { size: 'sm', onClick: async () => { await save((n) => { n.engine.model = m.id; n.engine.modelPath = null; }); drawModels(); } }))
          : btn(`Download · ${m.sizeMb >= 1000 ? (m.sizeMb / 1000).toFixed(1) + ' GB' : m.sizeMb + ' MB'}`, { size: 'sm', icon: 'download', onClick: async () => {
              progress[m.id] = { state: 'downloading', received: 0, total: m.sizeMb * 1e6 }; drawModels();
              try { await invoke('download_model', { id: m.id }); toast(`${m.name} downloaded`); } catch (err) { toast(errText(err), { error: true }); progress[m.id] = { state: 'error' }; }
              drawModels();
            } });
      modelsHost.append(h('div', { class: 'model-row' },
        h('div', {},
          h('div', { class: 'h3' }, m.name, m.recommended ? h('span', { class: 'chip accent', style: { marginLeft: '8px' } }, 'Recommended') : null),
          h('div', { class: 'small faint', style: { marginTop: '3px' } }, `${m.languages} · speed `, dots(m.speed), ' · accuracy ', dots(m.accuracy))),
        right));
    }
  };
  redrawModels = drawModels;
  cleanups.push(() => { if (redrawModels === drawModels) redrawModels = null; });
  cleanups.push(store.on(drawEngine));
  drawEngine();
  await drawModels();
  section('engine', 'Speech engine', 'Navi runs Whisper on this computer. Nothing you say is uploaded to transcribe it.',
    engineHost,
    h('div', { class: 'set-row', style: { display: 'block' } }, h('div', { class: 'h3', style: { marginBottom: '4px' } }, 'Models'), modelsHost),
    setRow('Language', 'Detect automatically handles 99 languages with a Large v3 Turbo model. Small (English) and Base (English) only understand English.', select([{ value: 'en', label: 'English' }, { value: 'auto', label: 'Detect automatically' }], S().engine.language, (v) => save((n) => { n.engine.language = v; }))),
    setRow('Use the graphics card', 'Much faster when you have an NVIDIA card.', toggle(S().engine.useGpu, (v) => save((n) => { n.engine.useGpu = v; }))),
    setRow('Trim silences', 'Skips pauses before transcribing. Faster, and fewer phantom words.', toggle(S().engine.vad, (v) => save((n) => { n.engine.vad = v; }))),
  );

  // ---------------------------------------------------------------- AI
  const ai = S().ai;
  const keyInput = h('input', { class: 'input', type: 'password', style: { width: '260px' }, placeholder: 'Paste your API key' });
  const keyState = h('span', { class: 'small faint' });
  const refreshKey = async () => {
    const p = S().ai.provider;
    if (p === 'off' || p === 'ollama') { keyState.textContent = ''; return; }
    const has = await invoke('has_secret', { name: p }).catch(() => false);
    keyState.textContent = has ? 'Saved in Windows Credential Manager' : p === 'custom' ? 'No key saved. Only needed if your server asks for one.' : 'No key saved';
  };
  const modelInput = h('input', { class: 'input', style: { width: '260px' }, value: ai.model, list: 'model-hints' });
  const hints = h('datalist', { id: 'model-hints' });
  const fillHints = () => { hints.innerHTML = ''; (MODEL_HINTS[S().ai.provider] || []).forEach((m) => hints.append(h('option', { value: m }))); };
  modelInput.addEventListener('change', () => save((n) => { n.ai.model = modelInput.value.trim(); }));
  const baseInput = h('input', { class: 'input', style: { width: '260px' }, value: ai.baseUrl || '', placeholder: 'http://localhost:11434' });
  baseInput.addEventListener('change', () => save((n) => { n.ai.baseUrl = baseInput.value.trim() || null; }));
  const testOut = h('span', { class: 'small faint' });
  const subHost = h('div', { class: 'ai-tools' });
  const apiHost = h('div', {});
  const offHost = h('div', {});
  const API = ['anthropic', 'openai', 'ollama', 'custom'];
  const TOOLS = ['claude-code', 'codex'];
  let tools = [];
  const redetect = async () => { tools = (await invoke('detect_ai_tools').catch(() => null)) || tools; };
  const setProvider = async (v, model) => {
    const before = S().ai.provider;
    await save((n) => {
      if (n.ai.provider !== v) {
        // A model for longer jobs belongs to the provider it was picked for: "opus" sent to OpenAI
        // or Ollama made every call write-up fail.
        n.ai.longModel = '';
        // The same field holds Ollama's or Custom's address and Claude Code's or Codex's path, so
        // it can't carry over to or from a tool (Claude Code's path would start Claude for Codex).
        if (TOOLS.includes(v) || TOOLS.includes(n.ai.provider)) n.ai.baseUrl = null;
      }
      n.ai.provider = v;
      if (model) n.ai.model = model;
      else if (MODEL_HINTS[v]?.length && !MODEL_HINTS[v].includes(n.ai.model)) n.ai.model = MODEL_HINTS[v][0];
    });
    // A tool's card says where it was found, which follows the path just cleared.
    if (before !== v && (TOOLS.includes(v) || TOOLS.includes(before))) await redetect();
    modelInput.value = S().ai.model; baseInput.value = S().ai.baseUrl || ''; testOut.textContent = ''; drawAi();
  };
  // Where Claude Code or Codex is, for when Navi can't find it. Typed on a tool's card, it also
  // switches to that tool.
  const setToolPath = async (t, path) => {
    if (!path && S().ai.provider !== t.id) return;
    const before = { ...S().ai };
    const ok = await save((n) => {
      if (n.ai.provider !== t.id) {
        n.ai.provider = t.id;
        n.ai.longModel = '';
        if (t.models?.length) n.ai.model = t.models[0];
      }
      n.ai.baseUrl = path || null;
    });
    if (!ok) return;
    await redetect();
    if (path && !tools.find((x) => x.id === t.id)?.found) {
      // A path that doesn't work never switches the AI away from the one that was working.
      if (before.provider !== t.id) { await save((n) => { n.ai = { ...n.ai, ...before }; }); await redetect(); }
      toast(`Navi can’t start ${t.name} from there. Check the path.`, { error: true });
    }
    modelInput.value = S().ai.model; testOut.textContent = ''; drawAi();
  };
  const testRow = () => setRow('Check it works', testOut, btn('Test', { size: 'sm', icon: 'spark', onClick: async () => {
    testOut.textContent = 'Asking…';
    try { const r = await invoke('test_ai'); testOut.textContent = `Working: “${String(r).slice(0, 40)}”`; } catch (e) { testOut.textContent = errText(e); }
  } }));
  const drawAi = () => {
    const p = S().ai.provider;
    // Use the AI you already pay for: Claude Code and Codex sign in with your plan, no key.
    subHost.replaceChildren(...tools.map((t) => {
      const using = p === t.id;
      const models = t.models || [];
      const current = using ? S().ai.model : '';
      const modelPick = using && models.length
        ? (t.id === 'claude-code'
          ? seg(models.map((m) => ({ value: m, label: m[0].toUpperCase() + m.slice(1) })), models.find((m) => current.includes(m)) || models[0], (v) => setProvider(t.id, v))
          : select(models, models.includes(current) ? current : models[0], (v) => setProvider(t.id, v)))
        : null;
      // Where the tool is, for when Navi can't find it: on the card in use, and on one not found.
      let pathRow = null;
      if (using || !t.found) {
        // An address left from Ollama or Custom isn't a path (cli_ai.rs passes over it).
        const path = using && !String(S().ai.baseUrl || '').includes('://') ? S().ai.baseUrl || '' : '';
        // Where each tool usually is: Claude Code's own installer puts it in .local\bin (where
        // cli_ai.rs looks first), and Codex comes through npm.
        const pathIn = h('input', { class: 'input', value: path, placeholder: t.id === 'codex' ? 'C:\\Users\\you\\AppData\\Roaming\\npm\\codex.cmd' : 'C:\\Users\\you\\.local\\bin\\claude.exe', spellcheck: 'false' });
        pathIn.addEventListener('change', () => setToolPath(t, pathIn.value.trim()));
        pathRow = h('div', { class: 'ai-tool-path' },
          h('span', { class: 'small quiet' }, 'Path to the tool'), pathIn,
          h('span', { class: 'small faint' }, 'Only if Navi can’t find it. A program, a .js file or an npm .cmd file.', using && t.found && t.path ? ` Navi is using ${t.path}.` : ''));
      }
      return h('div', { class: `ai-tool ${using ? 'on' : ''} ${t.found ? '' : 'missing'}` },
        h('div', { class: 'ai-tool-head' },
          h('div', {}, h('div', { class: 'h3' }, t.name), h('div', { class: 'small faint' }, `Uses ${t.plan}. No API key.`)),
          using ? chip('In use', 'accent', true) : t.found ? btn(`Use ${t.name}`, { size: 'sm', onClick: () => setProvider(t.id, models[0]) }) : chip('Not installed', 'line')),
        !t.found ? h('p', { class: 'small quiet' }, t.installHint) : null,
        modelPick ? h('div', { class: 'row', style: { gap: '10px', marginTop: '4px' } }, h('span', { class: 'small quiet' }, 'Model'), modelPick,
          t.id === 'claude-code' ? h('span', { class: 'small faint' }, 'Haiku is quickest.') : null) : null,
        pathRow);
    }));
    // Or straight to a provider: an API key, Ollama, or any OpenAI-compatible server.
    apiHost.replaceChildren(
      setRow('Or connect directly', 'An API key for Claude or OpenAI (paid by use), Ollama on this computer (free, no key), or any OpenAI-compatible server.',
        seg([{ value: 'anthropic', label: 'Claude' }, { value: 'openai', label: 'OpenAI' }, { value: 'ollama', label: 'Ollama' }, { value: 'custom', label: 'Custom' }], API.includes(p) ? p : '', (v) => setProvider(v))));
    if (API.includes(p)) {
      apiHost.append(setRow('Model', p === 'anthropic' ? 'Haiku is quick and cheap, and plenty for dictation.' : p === 'custom' ? 'Needed: the name your server knows the model by.' : null, h('div', {}, modelInput, hints)));
      if (p === 'ollama' || p === 'custom') {
        // Navi adds /chat/completions to a custom server's address (llm.rs).
        baseInput.placeholder = p === 'ollama' ? 'http://localhost:11434' : 'http://localhost:1234/v1';
        apiHost.append(setRow('Address', p === 'ollama' ? 'Where Ollama is running.' : 'The address of any OpenAI-compatible server, up to the part before /chat/completions.', baseInput));
      }
      if (p !== 'ollama') apiHost.append(setRow('API key', keyState, h('div', { class: 'row' }, keyInput, btn('Save', { size: 'sm', onClick: async () => {
        if (!keyInput.value.trim()) return;
        try { await invoke('set_secret', { name: p, value: keyInput.value.trim() }); keyInput.value = ''; toast('Key saved'); refreshKey(); } catch (e) { toast(errText(e), { error: true }); }
      } }))));
      fillHints(); refreshKey();
    }
    offHost.replaceChildren(...[
      longRow(),
      p !== 'off' ? testRow() : null,
      setRow(p === 'off' ? 'AI is off' : 'Turn AI off', 'Navi still tidies punctuation, capitals, fillers and spelling on this computer. AI is for the rewriting modes, edits like “make that shorter”, “what’s wrong with this?”, ramble and call notes, jobs on calls, screen-watch write-ups and learning your style. “Scratch that” and “change X to Y” work without it.',
        p === 'off' ? chip('Off', 'line') : btn('Turn off', { size: 'sm', variant: 'ghost', onClick: () => setProvider('off') })),
    ].filter(Boolean));
  };
  // Call notes, watch write-ups, jobs on calls and "what's wrong with this?" don't keep
  // anyone waiting, so they can use a stronger model than dictation.
  const longRow = () => {
    const p = S().ai.provider;
    if (p === 'off') return null;
    // A name that doesn't suit this provider (left from another one) is ignored when the jobs run,
    // so it shows as the default here too.
    const stored = S().ai.longModel || '';
    const cur = stored.trim() && longModelFits(p, stored) ? stored : '';
    if (p === 'claude-code') {
      const names = ['haiku', 'sonnet', 'opus'];
      // Show the model that will run: the one named, otherwise Sonnet (controller.rs make_llm_long).
      const running = names.find((m) => cur.toLowerCase().includes(m)) || 'sonnet';
      return setRow('Model for longer jobs', 'Call notes, screen watches, jobs on calls and “what’s wrong with this?”. Nobody sits waiting for these, so Sonnet is the default.',
        seg(names.map((m) => ({ value: m, label: m[0].toUpperCase() + m.slice(1) })), running, (v) => save((n) => { n.ai.longModel = v; })));
    }
    const inp = h('input', { class: 'input', style: { width: '260px' }, value: cur, placeholder: 'Same as above', list: 'model-hints' });
    inp.addEventListener('change', () => save((n) => { n.ai.longModel = inp.value.trim(); }));
    return setRow('Model for longer jobs', 'For call notes, screen watches, jobs on calls and “what’s wrong with this?”. Leave it empty to use the model above.', inp);
  };
  section('ai', 'AI', 'Optional, and off until you pick one. Used by the modes that rewrite (Email, Message, Prompt), edits like “make that shorter”, “what’s wrong with this?”, ramble and call notes, jobs on calls, screen-watch write-ups, learning your style, and Clean’s light polish if you turn it on. Only words are sent, never audio or pictures.',
    h('div', { class: 'h3', style: { margin: '2px 0 4px' } }, 'Use the AI you already pay for'),
    h('p', { class: 'small quiet', style: { marginBottom: '12px' } }, 'Navi can borrow Claude Code or Codex if they are on this computer, with no API key. Rewrites take a few seconds this way: about 3 with Claude Code and about 4 or 5 with Codex. Clean mode stays instant, because its polish skips these tools.'),
    subHost, apiHost, offHost);
  drawAi();
  invoke('detect_ai_tools').then((list) => { tools = list || []; drawAi(); }).catch(() => {});

  // ---------------------------------------------------------------- voice (ElevenLabs)
  const elKey = h('input', { class: 'input', type: 'password', style: { width: '260px' }, placeholder: 'Paste your ElevenLabs API key' });
  const elKeyState = h('span', { class: 'small faint' });
  const voiceHost = h('div', {});
  const modelHost = h('div', {});
  const elTestOut = h('span', { class: 'small faint' });
  async function loadVoices() {
    const has = await invoke('has_secret', { name: 'elevenlabs' }).catch(() => false);
    elKeyState.textContent = has ? 'Saved in Windows Credential Manager' : 'Find yours at elevenlabs.io, under Developers > API keys.';
    if (!has) { voiceHost.replaceChildren(h('span', { class: 'small faint' }, 'Save a key to see your voices')); modelHost.replaceChildren(); return; }
    voiceHost.replaceChildren(h('span', { class: 'small faint' }, 'Loading your voices…'));
    try {
      const [voices, models] = await Promise.all([invoke('eleven_voices'), invoke('eleven_models')]);
      const cur = S().speak?.voiceId || '';
      voiceHost.replaceChildren(select([{ value: '', label: 'Pick a voice' }, ...voices.map((v) => ({ value: v.voiceId, label: v.description ? `${v.name} · ${v.description}` : v.name }))], cur,
        (id) => save((n) => { n.speak.voiceId = id; n.speak.voiceName = voices.find((v) => v.voiceId === id)?.name || ''; }), 'wide-select'));
      const curModel = S().speak?.model || 'eleven_v4_turbo';
      modelHost.replaceChildren(select(models.map((m) => ({ value: m.modelId, label: m.name || m.modelId })), models.some((m) => m.modelId === curModel) ? curModel : models[0]?.modelId,
        (m) => save((n) => { n.speak.model = m; })));
    } catch (e) { voiceHost.replaceChildren(h('span', { class: 'small', style: { color: 'var(--blue-ink)' } }, errText(e))); }
  }
  section('voice', 'Voice', 'Hear what you said read back in an ElevenLabs voice, on the Speak page or from History. ElevenLabs counts every character it reads against your plan.',
    setRow('ElevenLabs API key', elKeyState, h('div', { class: 'row' }, elKey, btn('Save', { size: 'sm', onClick: async () => {
      if (!elKey.value.trim()) return;
      try { await invoke('set_secret', { name: 'elevenlabs', value: elKey.value.trim() }); elKey.value = ''; toast('Key saved'); loadVoices(); } catch (e) { toast(errText(e), { error: true }); }
    } }))),
    setRow('Voice', 'Your own voices, clones and any you added from the ElevenLabs library.', voiceHost),
    setRow('Model', 'Eleven v4 sounds the most natural. v4 Turbo answers quicker.', modelHost),
    setRow('Hear it', elTestOut, btn('Play a sample', { size: 'sm', icon: 'play', onClick: async () => {
      elTestOut.textContent = 'Asking ElevenLabs…';
      try {
        const url = await invoke('speak_preview', { text: `Hi${S().userName ? ` ${S().userName}` : ''}, this is how I sound in Navi.`, voiceId: null });
        new Audio(url).play().catch(() => {});
        elTestOut.textContent = '';
      } catch (e) { elTestOut.textContent = errText(e); }
    } })));
  loadVoices();

  // ---------------------------------------------------------------- writing
  const style = h('textarea', { class: 'textarea', placeholder: 'How you write, in a few lines. For example: Short and warm. British spelling. I sign off emails with “Cheers, Alex”.' });
  style.value = S().writing.styleNotes || '';
  style.addEventListener('input', debounce(() => save((n) => { n.writing.styleNotes = style.value; }), 600));
  const learnStyle = btn('Learn it from my writing…', { size: 'sm', icon: 'spark', onClick: async () => {
    const paths = await pickFile({ multiple: true, filters: [{ name: 'Text', extensions: ['txt', 'md', 'eml', 'docx'] }] });
    const list = Array.isArray(paths) ? paths : paths ? [paths] : [];
    if (!list.length) return;
    learnStyle.disabled = true; learnStyle.lastChild.textContent = 'Reading your writing…';
    try { style.value = await invoke('generate_style_notes', { paths: list }); await save((n) => { n.writing.styleNotes = style.value; n.writing.styleSamples = list; }); toast('Style notes written'); }
    catch (e) { toast(errText(e), { error: true }); }
    learnStyle.disabled = false; learnStyle.lastChild.textContent = 'Learn it from my writing…';
  } });
  section('writing', 'Writing', null,
    setRow('British English', 'Colour, organise, travelled. Leaves code, links and names alone.', toggle(S().writing.british, (v) => save((n) => { n.writing.british = v; }))),
    setRow('No em dashes', 'Swaps long dashes for commas and full stops.', toggle(S().writing.noEmDashes, (v) => save((n) => { n.writing.noEmDashes = v; }))),
    setRow('Remove ums and ahs', 'Drops filler words and stutters.', toggle(S().writing.removeFillers, (v) => save((n) => { n.writing.removeFillers = v; }))),
    h('div', { class: 'set-row', style: { display: 'block' } },
      h('div', { class: 'row between', style: { marginBottom: '10px' } }, h('div', {}, h('div', { class: 'h3' }, 'Your style'), h('p', { class: 'small quiet', style: { marginTop: '3px' } }, 'Every AI mode follows this, so rewrites still sound like you.')), learnStyle),
      style),
  );

  // ---------------------------------------------------------------- privacy
  const excl = h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '6px', maxWidth: '360px', justifyContent: 'flex-end' } });
  const drawExcl = () => {
    excl.innerHTML = '';
    S().context.excludedApps.forEach((a) => { const c = chip(a.replace(/\.exe$/i, ''), 'line'); c.append(h('button', { html: '&times;', title: 'Remove', onClick: () => save((n) => { n.context.excludedApps = n.context.excludedApps.filter((x) => x !== a); }).then(drawExcl) })); excl.append(c); });
    const add = h('input', { class: 'input', style: { width: '150px', height: '28px', borderRadius: '999px', fontSize: '12px' }, placeholder: 'Add app.exe' });
    add.addEventListener('keydown', (e) => { if (e.key === 'Enter' && add.value.trim()) { let v = add.value.trim(); if (!/\.exe$/i.test(v)) v += '.exe'; save((n) => { n.context.excludedApps = [...n.context.excludedApps, v]; }).then(drawExcl); } });
    excl.append(add);
  };
  drawExcl();
  section('privacy', 'Context and privacy', 'Navi reads a little of what is on screen to get names and tone right. The reading happens on this computer. When an AI mode runs, the app name, the window title and up to 1,000 characters of the text before your cursor go to your AI with your words.',
    setRow('Read the window title', 'Lets Navi use the title of the window you are in and, in a browser, the site in the address bar (its name only, to pick a mode). Off, Navi still sees which app you are in, to pick a mode, and tells your AI its name.', toggle(S().context.readApp, (v) => save((n) => { n.context.readApp = v; }))),
    setRow('Read the text near my cursor', 'The paragraph you are writing, so names come out right and replies match the thread. Password boxes are never read.', toggle(S().context.readField, (v) => save((n) => { n.context.readField = v; }))),
    setRow('Read the screen', 'Windows’ own on-device text recognition of the window you are in, so the speech engine spells names on screen right. It never goes to your AI. Off by default.', toggle(S().context.ocr, (v) => save((n) => { n.context.ocr = v; }))),
    setRow('Never read these apps', null, excl),
    setRow('Local only', 'Nothing goes to the internet except downloads you start and the Check for updates button. AI then only works with Ollama or a server on this computer; without one, Navi tidies your words on this computer.', toggle(S().privacy.localOnly, (v) => save((n) => { n.privacy.localOnly = v; }))),
    setRow('Keep recordings for', 'Text is kept until you delete it. Audio can expire.', select([{ value: '7', label: '7 days' }, { value: '30', label: '30 days' }, { value: '90', label: '90 days' }, { value: '0', label: 'Forever' }], String(S().privacy.keepAudioDays), (v) => save((n) => { n.privacy.keepAudioDays = Number(v); }))),
    setRow('Save history', 'Off means nothing is kept after it is pasted.', toggle(S().privacy.saveHistory, (v) => save((n) => { n.privacy.saveHistory = v; }))),
    setRow('Clear history', 'Every take and its recording on this computer. Your notes stay.', btn('Clear history', { size: 'sm', variant: 'danger', onClick: async () => {
      if (!(await confirmDialog('Clear all history?', 'Every take and its recording is deleted from this computer. Your words, snippets and modes stay, and so do your notes, call notes, screenshots and Markdown files.', { ok: 'Delete all', danger: true }))) return;
      await invoke('clear_history'); toast('History cleared');
    } })),
  );

  // ---------------------------------------------------------------- meeting notes
  const nt = S().notes || { detectCalls: true, systemAudio: true, autoStop: true };
  section('meetings', 'Meeting notes', 'Navi can take notes on your calls: it records both sides, writes down what was said with each line marked You or Them, and turns it into notes with the AI set up above. The audio is transcribed on this computer and never kept. Only words go to that AI: the transcript, anything you typed during the call, and the text on screenshots if those are on.',
    setRow('Offer to take notes when a call starts', 'When Teams, Zoom, Google Meet, Slack, Discord or WhatsApp starts using your microphone, a small card in the corner asks.', toggle(nt.detectCalls, (v) => save((n) => { n.notes.detectCalls = v; }))),
    setRow('Record the other side of the call', 'What your computer plays, so the other people are in the notes too. Off records only your microphone.', toggle(nt.systemAudio, (v) => save((n) => { n.notes.systemAudio = v; }))),
    setRow('Finish when the call ends', 'Navi writes the notes 20 seconds after the call app lets go of the microphone. Notes with no call app to follow finish after 20 minutes with nothing said.', toggle(nt.autoStop, (v) => save((n) => { n.notes.autoStop = v; }))),
    setRow('Jobs on calls', ['Say “', jobsWk(), ', …” on a call (“', jobsWk(), ', draft the follow-up email to Sam”) and your AI does it while you talk. The answer is kept with the notes. It only writes: nothing is sent or booked.'], toggle(nt.callJobs !== false, (v) => save((n) => { n.notes.callJobs = v; }))),
    setRow('Screenshots during calls', 'A look at the screen every 15 seconds, kept when it has changed, so the notes can say what was shown. They stay on this computer; your AI only gets the text on them.', toggle(!!nt.callScreenshots, (v) => save((n) => { n.notes.callScreenshots = v; }))),
    setRow('Send actions to', 'The actions from each call’s notes, once they’re written. Connect ClickUp or Notion under Notes and tasks.', seg([{ value: 'none', label: 'Nowhere' }, { value: 'clickup', label: 'ClickUp' }, { value: 'notion', label: 'Notion' }], nt.actionsTo || 'none', async (v) => { await save((n) => { n.notes.actionsTo = v; }); drawInt(); })),
    setRow('Your notes', 'Saved on the Notes page and as Markdown files in your notes folder (below).', btn('Open Notes', { size: 'sm', icon: 'notes', onClick: () => invoke('open_note', { id: null }) })),
    h('p', { class: 'small faint', style: { paddingTop: '14px', borderTop: '1px solid var(--line-soft)' } },
      'Tell the people on the call that Navi is taking notes. In some places, recording a call needs everyone’s consent.'),
  );

  // ---------------------------------------------------------------- integrations
  const r = S().ramble;
  const folderLabel = h('span', { class: 'small faint', style: { maxWidth: '260px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' } }, r.notesDir || 'Documents\\Navi Notes');
  const intRows = h('div', {});
  const tokenRow = (name, label, idKey, idLabel) => {
    const tok = h('input', { class: 'input', type: 'password', style: { width: '220px' }, placeholder: `${label} token` });
    const idIn = h('input', { class: 'input', style: { width: '220px' }, value: S().ramble[idKey] || '', placeholder: idLabel });
    idIn.addEventListener('change', () => save((n) => { n.ramble[idKey] = idIn.value.trim() || null; }));
    const out = h('span', { class: 'small faint' });
    invoke('has_secret', { name }).then((has) => { out.textContent = has ? 'Token saved' : 'No token yet'; }).catch(() => {});
    return h('div', {},
      setRow(`${label} token`, out, h('div', { class: 'row' }, tok, btn('Save', { size: 'sm', onClick: async () => { if (!tok.value.trim()) return; await invoke('set_secret', { name, value: tok.value.trim() }); tok.value = ''; out.textContent = 'Token saved'; toast('Saved'); } }))),
      setRow(idLabel, null, h('div', { class: 'row' }, idIn, btn('Test', { size: 'sm', onClick: async () => { out.textContent = 'Checking…'; try { out.textContent = await invoke('test_integration', { name }); } catch (e) { out.textContent = errText(e); } } }))));
  };
  // Ramble tasks and call actions share these connections, so either one asking shows them.
  const drawInt = () => {
    intRows.innerHTML = '';
    const wanted = [S().ramble.tasksTo, S().notes?.actionsTo];
    if (wanted.includes('clickup')) intRows.append(tokenRow('clickup', 'ClickUp', 'clickupListId', 'List ID'));
    if (wanted.includes('notion')) intRows.append(tokenRow('notion', 'Notion', 'notionDatabaseId', 'Database ID'));
  };
  section('integrations', 'Notes and tasks', 'Where your rambles and meeting notes go. Each is saved as a Markdown file. A ramble’s tasks can go to ClickUp, or the whole note (without the transcript) to Notion; a call’s actions can go to either.',
    setRow('Notes folder', folderLabel, btn('Change…', { size: 'sm', icon: 'folder', onClick: async () => {
      const dir = await pickFile({ directory: true, title: 'Choose a folder for your notes' });
      if (!dir) return;
      await save((n) => { n.ramble.notesDir = dir; }); folderLabel.textContent = dir;
    } })),
    setRow('Also paste the notes', 'Into the app you were using when you finished.', toggle(r.pasteNotes, (v) => save((n) => { n.ramble.pasteNotes = v; }))),
    setRow('Send tasks to', null, seg([{ value: 'none', label: 'Nowhere' }, { value: 'clickup', label: 'ClickUp' }, { value: 'notion', label: 'Notion' }], r.tasksTo, async (v) => { await save((n) => { n.ramble.tasksTo = v; }); drawInt(); })),
    intRows);
  drawInt();

  // ---------------------------------------------------------------- about
  const info = store.info || {};
  // Version and updates (update.rs): the button checks GitHub, then offers the update it found.
  const upNote = h('span', { class: 'faint' });
  const upBtn = btn('Check for updates', { size: 'sm', icon: 'download' });
  const drawUpdate = (u) => {
    const s = u || {};
    upBtn.disabled = s.state === 'checking' || s.state === 'downloading';
    if (s.state === 'available') {
      upBtn.lastChild.textContent = `Update to ${s.version}`;
      upNote.textContent = 'A new version is available. Updating restarts Navi.';
    } else if (s.state === 'downloading') {
      upBtn.lastChild.textContent = `Downloading ${Math.round((s.progress || 0) * 100)}%`;
      upNote.textContent = 'Navi restarts when it has finished.';
    } else {
      upBtn.lastChild.textContent = s.state === 'checking' ? 'Checking…' : 'Check for updates';
      upNote.textContent = s.state === 'current' ? 'You have the latest version.' : s.state === 'error' ? s.message || '' : '';
    }
  };
  upBtn.onclick = async () => {
    const now = await invoke('update_status').catch(() => null);
    if (now?.state === 'available') {
      try { await invoke('install_update'); } catch (e) { toast(errText(e), { error: true }); }
      return;
    }
    drawUpdate({ state: 'checking' });
    drawUpdate(await invoke('check_for_updates').catch((e) => ({ state: 'error', message: errText(e) })));
  };
  invoke('update_status').then(drawUpdate).catch(() => {});
  listen('update', drawUpdate).then((off) => cleanups.push(off)).catch(() => {});
  section('about', 'About Navi', null,
    setRow('Version', h('span', {}, `Navi ${info.version || '0.1.0'}. `, upNote), upBtn),
    setRow('Check for updates automatically', 'Navi looks on GitHub for a new version soon after it starts and every few hours, and tells you when one is ready. It doesn’t look while Local only is on.', toggle(S().general.checkUpdates !== false, (v) => save((n) => { n.general.checkUpdates = v; }))),
    setRow('Your data', 'Settings, history, recordings, screenshots and speech models live in this folder. Markdown notes go to your notes folder.', btn('Open folder', { size: 'sm', icon: 'external', onClick: () => invoke('open_data_folder') })),
    setRow('Set-up', 'Walk through the first-run steps again.', btn('Run set-up again', { size: 'sm', onClick: async () => { await save((n) => { n.onboarded = false; }); location.reload(); } })),
    h('p', { class: 'small faint', style: { paddingTop: '14px', borderTop: '1px solid var(--line-soft)' } },
      'Navi is free, under the MIT licence. Speech recognition by whisper.cpp (MIT). Typeface Poppins (SIL Open Font Licence). Made by ',
      h('a', { href: 'https://github.com/GoodLadTom', onClick: (e) => { e.preventDefault(); openPath('https://github.com/GoodLadTom'); } }, 'GoodLad'), '.'),
  );

  // ---------------------------------------------------------------- subnav with scroll spy
  const view = root.closest('.view');
  const mark = (cur) => subnav.querySelectorAll('button').forEach((b) => b.classList.toggle('on', b.dataset.id === cur));
  // A clicked section stays lit while the page scrolls to it (the last sections can't reach the
  // top of the page, so the spy alone would never pick them).
  let chosen = null;
  let chosenTimer = null;
  const choose = (id) => {
    chosen = id;
    mark(id);
    clearTimeout(chosenTimer);
    chosenTimer = setTimeout(() => { chosen = null; }, 900);
    sectionEls[id]?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  };
  for (const [id, label] of SECTIONS) {
    subnav.append(h('button', { dataset: { id }, onClick: (e) => { e.currentTarget.blur(); choose(id); } }, label));
  }
  const spy = () => {
    if (chosen) return;
    let cur = SECTIONS[0][0];
    for (const [id] of SECTIONS) { const el = sectionEls[id]; if (el && el.getBoundingClientRect().top < 180) cur = id; }
    // Scrolled to the bottom: the last section is the one being read.
    if (view.scrollTop + view.clientHeight >= view.scrollHeight - 4) cur = SECTIONS[SECTIONS.length - 1][0];
    mark(cur);
  };
  // Scrolling by hand takes over again from a click.
  view.addEventListener('wheel', () => { chosen = null; }, { passive: true });
  view.addEventListener('scroll', spy);
  cleanups.push(() => clearTimeout(chosenTimer));
  spy();
  if (arg && sectionEls[arg]) setTimeout(() => choose(arg), 60);

  return () => cleanups.forEach((f) => { try { typeof f === 'function' && f(); } catch {} });
}

function dots(n) {
  return h('span', { class: 'dots5' }, [1, 2, 3, 4, 5].map((i) => h('i', { class: i <= n ? 'on' : '' })));
}

// Whether a model for longer jobs can belong to the provider. Keep in step with long_model_fits in
// src-tauri/src/controller.rs, which ignores one that can't (Settings must show what really runs).
function longModelFits(provider, model) {
  const m = model.trim().toLowerCase();
  const names = ['haiku', 'sonnet', 'opus'];
  const claude = m.startsWith('claude') || names.includes(m);
  if (provider === 'claude-code') return names.some((n) => m.includes(n));
  if (provider === 'anthropic') return m.startsWith('claude');
  if (provider === 'ollama') return !claude && !m.startsWith('gpt-');
  // A custom server may serve Claude under its full names; only Claude Code's bare names are leftovers.
  if (provider === 'custom') return !names.includes(m);
  return !claude;
}
