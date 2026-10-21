// First run: welcome, name and sound, microphone, shortcut, speech engine, a first take.
import { os } from '../platform.js';
import { invoke, listen } from '../bridge.js';
import { h, ico, btn, chip, select, chordLabel, errText } from '../ui.js';
import { shortcutPicker } from '../shortcut.js';
import { soundPicker, playCue } from '../soundpicker.js';

export function run({ store }) {
  return new Promise((resolve) => {
    const s = () => store.settings;
    // The dotted backdrop moves the window; the title bar's buttons stay on top of it.
    const overlay = h('div', { class: 'onb', 'data-tauri-drag-region': true });
    const card = h('div', { class: 'onb-card' });
    overlay.append(card);
    document.body.append(overlay);
    const unsubs = [];
    let step = 0;

    const steps = [welcome, voice, mic, shortcut, engine, tryIt, done];

    function frame(content, { next = 'Continue', onNext, back = true, nextDisabled = false } = {}) {
      card.innerHTML = '';
      const dots = h('div', { class: 'onb-steps' }, steps.map((_, i) => h('i', { class: i <= step ? 'on' : '' })));
      const nextBtn = btn(next, { variant: 'primary', icon: step === steps.length - 1 ? 'check' : 'right', iconRight: true, disabled: nextDisabled, onClick: async () => { if (onNext && (await onNext()) === false) return; go(step + 1); } });
      card.append(dots, content, h('div', { class: 'foot' }, back && step > 0 ? btn('Back', { variant: 'ghost', icon: 'left', onClick: () => go(step - 1) }) : h('span'), nextBtn));
      card.style.animation = 'none'; void card.offsetWidth; card.style.animation = '';
      return nextBtn;
    }

    function go(i) {
      unsubs.splice(0).forEach((f) => { try { f(); } catch {} });
      if (i >= steps.length) return finish();
      step = Math.max(0, i);
      steps[step]();
    }

    function welcome() {
      frame(h('div', {},
        // Navi's logo on its tile (app.css .logo-tile), which reads on light and dark alike.
        h('span', { class: 'welcome-mark logo-tile', 'aria-hidden': 'true' }),
        h('div', { class: 'eyebrow' }, 'Welcome'),
        h('h1', {}, 'Meet Navi.'),
        h('p', { class: 'lead' }, 'Hold a shortcut, talk, and let go. Your words appear wherever you were typing, tidied up and spelt the British way. It runs on this computer, and AI only comes in if you turn it on.'),
        h('div', { class: 'row', style: { gap: '8px', marginTop: '20px', flexWrap: 'wrap' } }, chip('Private by default', 'line'), chip('Works in almost every app', 'line'), chip('Learns your words', 'line'))),
        { next: 'Get started', back: false });
      setTimeout(() => playCue(store, s().sounds.set || 'hush', 'start'), 450);
    }

    function voice() {
      const name = h('input', { class: 'input', value: s().userName || '', placeholder: 'Your first name', style: { maxWidth: '280px' } });
      const picker = soundPicker({ store, compact: true });
      unsubs.push(() => picker.cleanup());
      frame(h('div', {},
        h('div', { class: 'eyebrow' }, 'Say hello'),
        h('h1', {}, 'What should Navi call you?'),
        name,
        h('div', { class: 'h3', style: { marginTop: '26px' } }, 'And how should Navi sound?'),
        h('p', { class: 'small quiet', style: { marginTop: '3px', marginBottom: '10px' } }, 'A soft sound tells you when Navi starts and stops listening. You will hear it a lot, so pick one you like. You can change it any time.'),
        picker.el), { onNext: () => store.saveSettings((n) => { n.userName = name.value.trim(); }) });
      setTimeout(() => name.focus(), 80);
    }

    async function mic() {
      const devices = await invoke('list_devices').catch(() => []);
      // Listing microphones can take a moment; if Continue was pressed meanwhile, don't draw over the next step.
      if (steps[step] !== mic) return;
      const meter = h('div', { class: 'meter', style: { marginTop: '18px' } }, h('i'));
      const pick = select([{ value: '', label: os('Windows default', 'System default') }, ...devices.map((d) => ({ value: d.name, label: d.name + (d.isDefault ? ' (default)' : '') }))], s().mic.device || '', async (v) => {
        await store.saveSettings((n) => { n.mic.device = v || null; });
        await invoke('mic_test_stop').catch(() => {});
        // Only reopen the mic if this step is still showing (a quick Continue must not leave it on).
        if (steps[step] === mic) await invoke('mic_test_start', { device: v || null }).catch(() => {});
      });
      let heard = false;
      const status = h('p', { class: 'small faint', style: { marginTop: '10px' } }, 'Say something, and the bar should move.');
      const nextBtn = frame(h('div', {},
        h('div', { class: 'eyebrow' }, 'Microphone'),
        h('h1', {}, 'Let’s check Navi can hear you.'),
        h('p', { class: 'lead' }, 'Pick your microphone and say a few words.'),
        h('div', { style: { marginTop: '20px', maxWidth: '420px' } }, pick, meter, status)), { onNext: () => invoke('mic_test_stop').catch(() => {}) });
      unsubs.push(() => invoke('mic_test_stop').catch(() => {}));
      try { await invoke('mic_test_start', { device: s().mic.device }); }
      catch (e) { status.textContent = errText(e); status.style.color = 'var(--blue-ink)'; }
      if (steps[step] !== mic) { invoke('mic_test_stop').catch(() => {}); return; }
      const off = await listen('level', (v) => {
        meter.firstChild.style.width = `${Math.round(v * 100)}%`;
        if (v > 0.25 && !heard) { heard = true; status.textContent = 'Perfect, Navi can hear you.'; status.style.color = 'var(--accent-ink)'; }
      });
      if (steps[step] !== mic) off(); else unsubs.push(off);
    }

    function shortcut() {
      const picker = shortcutPicker({ store, mode: 'auto' });
      unsubs.push(() => picker.cleanup());
      frame(h('div', {},
        h('div', { class: 'eyebrow' }, 'Your shortcut'),
        h('h1', {}, 'Hold to talk. Let go to finish.'),
        h('p', { class: 'lead' }, os('Pick the key you’ll hold while you speak. One key is easiest. It works in almost every app: one running as administrator gets your words on the clipboard to paste instead.', 'Pick the key you’ll hold while you speak. One key is easiest. If you pick Fn, set System Settings > Keyboard > “Press 🌐 key to” to “Do Nothing”, so the emoji picker doesn’t open too.')),
        h('div', { style: { marginTop: '22px' } }, picker.el)));
    }

    async function engine() {
      const body = h('div', {});
      const nextBtn = frame(h('div', {},
        h('div', { class: 'eyebrow' }, 'Speech engine'),
        h('h1', {}, 'Getting the ears ready.'),
        h('p', { class: 'lead' }, 'Navi uses Whisper, running on this computer, so what you say never leaves it to be transcribed.'),
        body), { nextDisabled: true });
      body.append(h('p', { class: 'small faint', style: { marginTop: '18px' } }, 'Looking at this computer…'));
      const setup = await invoke('detect_setup').catch(() => ({}));
      body.innerHTML = '';
      const ready = () => { nextBtn.disabled = false; };
      if (setup.modelFound && setup.serverFound) {
        body.append(h('div', { class: 'found' }, ico('check', 20, 2.2), h('div', {},
          h('div', { class: 'h3' }, 'Found Whisper already on this computer'),
          h('div', { class: 'small' }, `${setup.gpu ? `It will run on your ${setup.gpu}.` : 'It will run on the processor.'}`))));
        // The model id is enough: Navi finds its file in the same places it was just found. A
        // saved path would outlive choosing another model in Settings, so an old one is cleared,
        // unless it is the very file just found (a model kept somewhere else on purpose).
        await store.saveSettings((n) => {
          n.engine.model = setup.modelId || n.engine.model;
          if ((n.engine.modelPath || '').trim() !== (setup.modelPath || '').trim()) n.engine.modelPath = null;
          if (setup.serverPath) n.engine.serverPath = setup.serverPath;
        });
        ready();
        return;
      }
      const prog = h('div', { class: 'progress', style: { marginTop: '14px' } }, h('i'));
      const label = h('p', { class: 'small faint', style: { marginTop: '8px' } }, '');
      const gpuNote = setup.gpu ? `Your ${setup.gpu} will make it fast.` : 'No NVIDIA graphics card found, so Navi will use a lighter, English-only model on the processor. Settings > Speech engine has models for other languages.';
      const modelId = setup.gpu ? 'large-v3-turbo' : 'base.en';
      body.append(h('div', { class: 'card flat', style: { marginTop: '18px', background: 'var(--well)', border: 0 } },
        h('div', { class: 'h3' }, setup.gpu ? 'Whisper Large v3 Turbo · 1.6 GB' : 'Whisper Base (English) · 148 MB'),
        h('p', { class: 'small quiet', style: { marginTop: '3px' } }, gpuNote),
        prog, label));
      unsubs.push(await listen('download', (p) => {
        if (p.total) prog.firstChild.style.width = `${Math.round(p.received / p.total * 100)}%`;
        // A late "done" must not blank what the step says next.
        const text = p.state === 'downloading' ? `${Math.round(p.received / 1e6)} of ${Math.round(p.total / 1e6)} MB` : p.state === 'extracting' ? 'Unpacking…' : p.state === 'verifying' ? 'Checking the download…' : '';
        if (text) label.textContent = text;
      }));
      try {
        if (!setup.serverFound) { label.textContent = 'Downloading the engine…'; await invoke('download_engine'); }
        label.textContent = 'Downloading the model…';
        await invoke('download_model', { id: modelId });
        // The model just downloaded, not a path saved by an earlier set-up.
        await store.saveSettings((n) => { n.engine.model = modelId; n.engine.modelPath = null; });
      } catch (e) {
        label.textContent = errText(e); label.style.color = 'var(--blue-ink)';
        body.append(btn('Try again', { size: 'sm', icon: 'rerun', onClick: () => go(step) }));
        return;
      }
      prog.firstChild.style.width = '100%';
      await startEngine();

      // Only says Done once the engine is really running; otherwise the first take would fail.
      async function startEngine() {
        label.textContent = 'Starting the speech engine…'; label.style.color = '';
        const st = await invoke('restart_engine').catch((e) => ({ state: 'error', message: errText(e) }));
        if (st?.state === 'ready') { label.textContent = 'Done.'; ready(); return; }
        label.textContent = st?.message || 'The speech engine needs attention.'; label.style.color = 'var(--blue-ink)';
        const again = btn('Try again', { size: 'sm', icon: 'rerun', onClick: () => { again.remove(); startEngine(); } });
        body.append(again);
        // Set-up is never a dead end: the engine can be sorted out later in Settings.
        ready();
      }
    }

    async function tryIt() {
      const keys = chordLabel(s().hotkeys.talk);
      const area = h('textarea', { class: 'practice', style: { marginTop: '20px' }, placeholder: 'Click here first…' });
      const status = h('p', { class: 'try-status' }, `Click in the box, hold ${keys}, say “This is my first note, and it’s working”, then let go.`);
      frame(h('div', {},
        h('div', { class: 'eyebrow' }, 'Your first take'),
        h('h1', {}, 'Go on, say something.'),
        area, status), { next: 'Continue' });
      setTimeout(() => area.focus(), 100);
      const say = (text, tone = '') => { status.textContent = text; status.className = `try-status ${tone}`; };
      let heard = false;
      const nudge = setTimeout(() => { if (!heard) say(`Nothing yet? Go back two steps, to Your shortcut, and check Navi can hear ${keys}, or pick a one-key shortcut.`, 'warn'); }, 15000);
      unsubs.push(() => clearTimeout(nudge));
      unsubs.push(await listen('shortcut', (state) => { if (state === 'down') { heard = true; say('Navi heard your shortcut. Keep holding it while you talk…', 'on'); } }));
      unsubs.push(await listen('pill', (p) => {
        if (p?.state === 'listening') say('Listening. Let go when you’ve finished.', 'on');
        else if (p?.state === 'processing') say('Writing it down…', 'on');
        else if (p?.state === 'error') say(`${p.message}. Try again: hold ${keys} and talk.`, 'warn');
      }));
      unsubs.push(await listen('take', (t) => {
        if (t?.finalText) say(`Lovely. That took ${(t.latencyMs / 1000).toFixed(1)} seconds.`, 'done');
      }));
    }

    function done() {
      // The tips name the keys set now: "Run set-up again" can come after they were changed.
      const hk = s().hotkeys;
      // Learning needs its switch and "Read the text near my cursor" (controller.rs). A fix where
      // every misheard word is everyday English is only ever suggested (learn.rs can_auto_add).
      const learns = s().learning?.enabled !== false && s().context?.readField !== false;
      const n = Math.max(1, s().learning?.autoAddAfter || 2);
      const times = n === 1 ? 'once' : n === 2 ? 'twice' : `${n} times`;
      // Voice commands quote the words set now (Settings > Voice commands), "Navi" by default.
      const vc = s().voiceCommands;
      const said = (action) => (vc?.commands || []).find((c) => c.action === action)?.phrases?.find((p) => String(p).trim());
      const wake = said('wake') || 'Navi';
      frame(h('div', {},
        // The logo on its tile, as on the welcome step (markSrc() went with the old image).
        h('span', { class: 'welcome-mark logo-tile', 'aria-hidden': 'true', style: { width: '64px', height: '64px' } }),
        h('div', { class: 'eyebrow' }, 'All set'),
        h('h1', {}, 'You’re ready.'),
        h('p', { class: 'lead' }, 'A few things worth knowing:'),
        h('div', { class: 'stack', style: { marginTop: '14px', gap: '10px' } },
          hk.doubleTapHandsFree !== false ? tip('Double-tap', 'your shortcut for hands-free.') : null,
          tip(`Hold ${chordLabel(hk.talk)}, tap ${chordLabel([hk.rambleKey || 'Space'])}`, 'to ramble. Press Enter when you’re done and Navi turns it into notes.'),
          tip('“Navi, scratch that”', 'undoes the last thing you said.'),
          vc?.enabled
            ? tip(`“${wake}, ${said('watch_on') || 'watch my screen'}”`, 'and other voice commands. Change the words in Settings > Voice commands.')
            : tip(`Say “${wake}”`, 'to talk hands-free once you turn on Voice commands in Settings.'),
          learns ? tip(`Fix a name ${times}`, 'and Navi adds it to your words. Everyday words wait on the Dictionary page for your yes.') : null)),
        { next: 'Start using Navi' });
    }

    function tip(k, t) { return h('div', { class: 'row', style: { gap: '10px' } }, h('span', { class: 'chip accent' }, k), h('span', { class: 'quiet' }, t)); }

    async function finish() {
      await invoke('finish_onboarding').catch(() => {});
      await store.saveSettings((n) => { n.onboarded = true; });
      overlay.style.transition = 'opacity .3s'; overlay.style.opacity = '0';
      setTimeout(() => { overlay.remove(); resolve(); }, 300);
    }

    go(0);
  });
}
