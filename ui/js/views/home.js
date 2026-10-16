import { invoke } from '../bridge.js';
import { h, ico, btn, chip, keycaps, keyLabel, chordLabel, countUp, fmtNum, relTime, appInitial, toast, errText } from '../ui.js';
import { BubbleView } from '../bubble.js';
import { refreshSuggestions } from '../app.js';

// Each result keeps to what was said: the AI is told never to add facts (text/modes.rs).
const DEMO = [
  { app: 'Outlook', mode: 'Email', said: 'hi priya just to say the slides are done and in the shared folder', text: 'Hi Priya, just to say the slides are done and in the shared folder.' },
  { app: 'Slack', mode: 'Message', said: 'can we do thursday afternoon instead', text: 'Can we do Thursday afternoon instead?' },
  { app: 'Terminal', mode: 'Prompt', said: 'fix the history player so i can scrub through a take from its waveform', text: 'Goal: let me scrub through a take from its waveform in @ui/js/views/history.js.' },
];

export async function render(root, { store, go }) {
  const s = store.settings;
  const now = new Date();
  const hour = now.getHours();
  const greet = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening';
  const name = s.userName ? `, ${s.userName}` : '';

  root.append(h('div', { class: 'page-head' },
    h('div', {},
      h('div', { class: 'eyebrow' }, now.toLocaleDateString('en-GB', { weekday: 'long', day: 'numeric', month: 'long' })),
      h('h1', { class: 'h1' }, `${greet}${name}.`))));

  // ------------------------------------------------------------- hero
  const keysWrap = h('div', { class: 'hero-keys' });
  const caps = s.hotkeys.talk.map(keyLabel).map((label) => h('span', { class: `kbd lg ${label.length > 3 ? 'wide' : ''}` }, label));
  caps.forEach((c, i) => { if (i) keysWrap.append(h('span', { class: 'plus' }, '+')); keysWrap.append(c); });

  const typed = h('span', {});
  const demoApp = h('div', { class: 'demo-app' }, h('div', { class: 'bar' }, h('i'), h('i'), h('i')), h('div', { class: 'typed' }, typed, h('span', { class: 'caret' })));
  const bubbleHost = h('div', { class: 'demo-bubble' });
  const hero = h('div', { class: 'card hero' },
    h('div', { class: 'hero-left' },
      h('div', { class: 'eyebrow' }, 'Talk anywhere'),
      keysWrap,
      h('h2', { class: 'hero-title' }, `Hold ${chordLabel(s.hotkeys.talk)} and speak.`),
      h('p', { class: 'sub', style: { marginTop: '8px' } }, 'Let go, and your words appear wherever you were typing, tidied and spelt the British way.'),
      h('div', { class: 'tips' },
        s.hotkeys.doubleTapHandsFree ? chip('Double-tap for hands-free', 'line') : null,
        chip(`Add ${keyLabel(s.hotkeys.rambleKey)} to ramble`, 'line'),
        chip('“Navi, scratch that” to undo', 'line'))),
    h('div', { class: 'hero-right' }, demoApp, bubbleHost));
  root.append(hero);

  const bubble = new BubbleView(bubbleHost);
  bubble.setFirefly(s.general?.showFirefly !== false);
  let alive = true;
  runDemo({ bubble, caps, typed, isAlive: () => alive });

  // ------------------------------------------------------------- stats
  const stats = await invoke('get_stats').catch(() => null);
  const st = stats || { wordsToday: 0, wordsWeek: 0, minutesSaved: 0, avgWpm: 0, streakDays: 0, daily: [] };
  const statCard = (label, iconName, value, unit, foot, extra) => {
    const v = h('span', {}, '0');
    const card = h('div', { class: 'card stat' },
      h('div', { class: 'label' }, h('span', { class: 'eyebrow' }, label), h('span', { class: 'faint' }, ico(iconName, 16))),
      h('div', { class: 'value num' }, v, unit ? h('small', {}, unit) : null),
      foot ? h('div', { class: 'foot' }, foot) : null,
      extra || null);
    requestAnimationFrame(() => countUp(v, value));
    return card;
  };
  const hoursSaved = st.minutesSaved >= 90;
  // Typing time only (40 wpm, as history.rs TYPING_WPM). Time saved also takes off the speaking time.
  const typeMin = st.wordsToday / 40;
  const todayFoot = !st.wordsToday ? 'Nothing yet today' : typeMin < 1 ? 'Under a minute to type at 40 wpm' : `${fmtNum(typeMin)} min to type at 40 wpm`;
  root.append(h('div', { class: 'grid cols-4', style: { marginTop: '14px' } },
    statCard('Today', 'words', st.wordsToday, 'words', todayFoot),
    statCard('This week', 'history', st.wordsWeek, 'words', null, sparkline(st.daily)),
    statCard('Time saved', 'clock', hoursSaved ? st.minutesSaved / 60 : st.minutesSaved, hoursSaved ? 'hours' : 'min', 'vs typing at 40 wpm'),
    statCard('Pace', 'speed', st.avgWpm, 'wpm', st.streakDays ? `${st.streakDays}-day streak` : 'Speak most days for a streak'),
  ));

  // ------------------------------------------------------------- recent + side column
  const takes = await invoke('list_takes', { query: { limit: 6, offset: 0 } }).catch(() => []);
  const modes = await invoke('list_modes').catch(() => []);
  const modeName = (id) => modes.find((m) => m.id === id)?.name || id;

  const list = h('div', { class: 'take-list' });
  if (!takes.length) {
    list.append(h('div', { class: 'empty' }, h('div', { class: 'h2' }, 'Your takes will appear here'), h('p', {}, `Click into any app, hold ${chordLabel(s.hotkeys.talk)} and say something.`)));
  }
  for (const t of takes) {
    list.append(h('button', { class: 'take-row', onClick: () => go('history', t.id) },
      h('span', { class: 'app-av' }, appInitial(t.appName)),
      h('div', {},
        h('div', { class: 'meta' }, h('b', {}, t.appName || 'Unknown app'), chip(t.kind === 'ramble' ? 'Notes' : t.kind === 'command' ? 'Command' : modeName(t.modeId), t.kind === 'command' ? 'line' : 'accent')),
        // A take that failed to transcribe has no text, but History can re-run it: say why.
        h('div', { class: 'text' }, t.finalText || t.rawText || (t.status === 'error' ? `Failed: ${t.meta?.error || 'not transcribed'}` : ''))),
      h('span', { class: 'when' }, relTime(t.createdAt))));
  }

  const learned = await invoke('list_suggestions').catch(() => []);
  const learnCard = h('div', { class: 'card' },
    h('div', { class: 'card-head' }, h('div', {}, h('div', { class: 'eyebrow' }, 'Navi noticed'), h('div', { class: 'h2', style: { marginTop: '4px' } }, 'Words you corrected')), btn('', { variant: 'ghost', size: 'sm', icon: 'right', onClick: () => go('dictionary', 'learned'), title: 'Open' })));
  const learnBody = h('div', {});
  learnCard.append(learnBody);
  const drawLearned = (items) => {
    learnBody.innerHTML = '';
    if (!items.length) {
      // Learning needs its switch and "Read the text near my cursor" (controller.rs). A fix where
      // every misheard word is everyday English is only ever suggested (learn.rs can_auto_add).
      const n = Math.max(1, s.learning?.autoAddAfter || 2);
      const times = n === 1 ? 'once' : n === 2 ? 'twice' : `${n} times`;
      const hint = !s.learning?.enabled || s.context?.readField === false
        ? 'Navi can learn the words you fix after it pastes. Turn on “Learn from my corrections” in Dictionary, and “Read the text near my cursor” in Settings > Context and privacy.'
        : `When you fix a word after Navi pastes it, it shows up here. Fix a name or term ${times} and Navi adds it to your words. Everyday words wait here for your yes.`;
      learnBody.append(h('p', { class: 'small quiet' }, hint));
      return;
    }
    for (const sgg of items.slice(0, 3)) {
      learnBody.append(h('div', { class: 'learn-item' },
        h('div', { class: 'pair' }, h('s', {}, sgg.from), h('span', { class: 'arrow' }, '→'), h('b', {}, sgg.to), h('span', { class: 'faint small' }, `  ${sgg.count}×`)),
        btn('', { size: 'sm', icon: 'check', title: 'Add to my words', onClick: async () => { try { await invoke('accept_suggestion', { id: sgg.id }); toast(`Added ${sgg.to} to your words`); drawLearned(items.filter((x) => x.id !== sgg.id)); refreshSuggestions(); } catch (e) { toast(errText(e), { error: true }); } } }),
        btn('', { size: 'sm', variant: 'ghost', icon: 'x', title: 'Ignore', onClick: async () => { await invoke('dismiss_suggestion', { id: sgg.id }); drawLearned(items.filter((x) => x.id !== sgg.id)); refreshSuggestions(); } })));
    }
  };
  drawLearned(learned);

  const practice = h('textarea', { class: 'practice', placeholder: `Click here, hold ${chordLabel(s.hotkeys.talk)} and say: “Testing, one, two, three.”` });
  const practiceCard = h('div', { class: 'card' },
    h('div', { class: 'eyebrow' }, 'Try it here'),
    h('div', { class: 'h2', style: { margin: '4px 0 12px' } }, 'A safe place to practise'),
    practice);

  root.append(h('div', { class: 'grid', style: { gridTemplateColumns: '1.35fr 1fr', marginTop: '14px', alignItems: 'start' } },
    h('div', { class: 'card' },
      h('div', { class: 'card-head' }, h('div', {}, h('div', { class: 'eyebrow' }, 'Recent'), h('div', { class: 'h2', style: { marginTop: '4px' } }, 'Your last takes')), btn('History', { variant: 'ghost', size: 'sm', icon: 'right', onClick: () => go('history') })),
      list),
    h('div', { class: 'stack', style: { gap: '14px' } }, learnCard, practiceCard)));

  return () => { alive = false; bubble.hide(true); };
}

function sparkline(daily = []) {
  const w = 180, hgt = 30;
  const max = Math.max(1, ...daily.map((d) => d.words));
  const pts = daily.map((d, i) => [i * (w / Math.max(1, daily.length - 1)), hgt - 3 - (d.words / max) * (hgt - 6)]);
  if (!pts.length) return null;
  const line = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(' ');
  const area = `${line} L${w} ${hgt} L0 ${hgt} Z`;
  const last = pts[pts.length - 1];
  const svg = `<svg class="spark" width="100%" height="${hgt}" viewBox="0 0 ${w} ${hgt}" preserveAspectRatio="none">
    <path d="${area}" fill="var(--accent-tint)"/>
    <path d="${line}" fill="none" stroke="var(--accent)" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke"/>
    <circle cx="${last[0]}" cy="${last[1]}" r="2.6" fill="var(--accent)"/></svg>`;
  return h('div', { html: svg });
}

async function runDemo({ bubble, caps, typed, isAlive }) {
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  let i = 0;
  await wait(700);
  while (isAlive()) {
    const d = DEMO[i % DEMO.length];
    typed.textContent = '';
    caps.forEach((c) => c.classList.add('down'));
    bubble.set({ state: 'listening', kind: 'talk', modeName: d.mode, appLabel: d.app });
    const t0 = performance.now();
    while (performance.now() - t0 < 2300 && isAlive()) {
      const t = (performance.now() - t0) / 1000;
      bubble.level(Math.max(0, Math.sin(t * 7) * 0.35 + Math.sin(t * 2.9) * 0.3 + 0.25 + Math.random() * 0.2));
      await wait(33);
    }
    caps.forEach((c) => c.classList.remove('down'));
    if (!isAlive()) break;
    bubble.set({ state: 'processing', modeName: d.mode });
    await wait(700);
    bubble.set({ state: 'done', preview: d.text, outcome: 'pasted' });
    for (let k = 1; k <= d.text.length && isAlive(); k += 2) { typed.textContent = d.text.slice(0, k); await wait(12); }
    typed.textContent = d.text;
    await wait(2600);
    i++;
  }
}
