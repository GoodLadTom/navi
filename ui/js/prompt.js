// The card in the corner of the screen: offers to take notes when a call starts, shows the time
// while Navi listens (with how many Short ideas and jobs so far), says when the notes are saved,
// and shows the answer to "Navi, what's wrong with this?". meeting.rs decides what it shows and
// sizes the window to fit.
import { invoke, listen, isTauri, copyText } from './bridge.js';
import { h } from './ui.js';
import { icon } from './icons.js';
import { applyTheme } from './theme.js';

const card = document.getElementById('card');
let tick = null;
let autoHide = null;
let startedAt = Date.now();
// The "Taking notes" card's second line, kept so meeting updates can add ideas and jobs to it.
let liveSub = null;
let liveBase = '';

const dismiss = () => invoke('dismiss_meeting_prompt').catch(() => {});
const ic = (name, size = 16) => h('span', { html: icon(name, size, 1.9) });
const closeBtn = (title) => h('button', { class: 'x', title, onClick: dismiss, html: icon('x', 14, 2) });

function clockText() {
  const s = Math.max(0, Math.floor((Date.now() - startedAt) / 1000));
  const mm = String(Math.floor(s / 60) % 60).padStart(2, '0');
  const ss = String(s % 60).padStart(2, '0');
  return s >= 3600 ? `${Math.floor(s / 3600)}:${mm}:${ss}` : `${mm}:${ss}`;
}

function render(s) {
  clearInterval(tick);
  clearTimeout(autoHide);
  card.className = `card ${s?.state || 'hidden'}`;
  switch (s?.state) {
    case 'ask': {
      const sub = h('span', {}, `${s.app} is using your mic.`);
      const take = h('button', { class: 'btn primary' }, ic('notes', 15), 'Take notes');
      take.onclick = async () => {
        take.disabled = true;
        try {
          await invoke('start_meeting', { source: s.app });
        } catch (e) {
          sub.textContent = String(e);
          sub.classList.add('err');
          take.disabled = false;
        }
      };
      card.replaceChildren(
        h('span', { class: 'ic' }, ic('notes', 17)),
        h('div', { class: 'txt' }, h('b', {}, 'Take notes on this call?'), sub,
          h('div', { class: 'actions' }, take, h('button', { class: 'btn ghost', onClick: dismiss }, 'Not now'))),
        closeBtn('Not now'));
      // Ignored: it goes away by itself.
      autoHide = setTimeout(dismiss, 45000);
      break;
    }
    case 'recording': {
      startedAt = Date.now() - (s.elapsedMs || 0);
      const clock = h('span', { class: 'clock' }, clockText());
      liveBase = s.systemAudio ? 'Both sides of the call' : 'Your microphone only';
      // A card shown again mid-call (after an answer is closed) has its warning straight away.
      liveSub = h('span', {});
      showLive(s);
      card.replaceChildren(
        h('span', { class: 'rec' }),
        h('div', { class: 'txt open', title: 'Open in Navi', onClick: () => invoke('open_note', { id: null }) },
          h('b', {}, 'Taking notes', clock),
          liveSub),
        h('button', { class: 'btn', onClick: () => invoke('stop_meeting').catch(() => {}) }, 'Stop'),
        closeBtn('Hide (Navi keeps taking notes)'));
      tick = setInterval(() => { clock.textContent = clockText(); }, 1000);
      break;
    }
    case 'writing':
      card.replaceChildren(
        h('span', { class: 'spin' }),
        h('div', { class: 'txt' }, h('b', {}, 'Writing your notes'), h('span', {}, 'Navi will say when they’re ready.')));
      break;
    case 'done':
      card.replaceChildren(
        h('span', { class: 'ok' }, ic('check', 15)),
        h('div', { class: 'txt' }, h('b', {}, 'Notes saved'), h('span', {}, s.title || ''), s.detail ? h('span', { class: 'detail', title: s.detail }, s.detail) : null),
        h('button', { class: 'btn', onClick: () => { invoke('open_note', { id: s.id }); dismiss(); } }, 'Open'),
        closeBtn('Close'));
      autoHide = setTimeout(dismiss, 20000);
      break;
    case 'thinking':
      card.replaceChildren(
        h('span', { class: 'spin' }),
        // The AI for longer jobs answers this (Sonnet through Claude Code by default), which can
        // take a while (controller.rs CRITIQUE_DEADLINE), so no promise of seconds.
        h('div', { class: 'txt' }, h('b', {}, s.title || 'Thinking'), h('span', {}, 'Asking your AI. The answer appears here.')),
        closeBtn('Close'));
      break;
    case 'answer': {
      const copy = h('button', { class: 'btn' }, ic('copy', 14), 'Copy');
      copy.onclick = async () => { await copyText(s.body || ''); copy.lastChild.textContent = 'Copied'; setTimeout(() => { copy.lastChild.textContent = 'Copy'; }, 1600); };
      card.replaceChildren(
        h('div', { class: 'a-head' }, h('span', { class: 'ic small' }, ic('spark', 15)), h('b', {}, s.title || 'Answer'), closeBtn('Close')),
        h('div', { class: 'a-body selectable' }, answerBody(s.body || '')),
        h('div', { class: 'a-foot' }, copy, h('button', { class: 'btn ghost', onClick: dismiss }, 'Done')));
      break;
    }
    default:
      card.replaceChildren();
  }
}

// Bullets and short paragraphs, with **bold** and `code`. Built from elements, never HTML.
function answerBody(md) {
  const out = [];
  let list = null;
  for (const raw of md.split('\n')) {
    const t = raw.trim();
    if (!t) { list = null; continue; }
    const bullet = /^[-*•]\s+(.*)$/.exec(t) || /^\d+[.)]\s+(.*)$/.exec(t);
    if (bullet) {
      if (!list) { list = h('ul', {}); out.push(list); }
      list.append(h('li', {}, inline(bullet[1])));
    } else {
      list = null;
      out.push(h('p', {}, inline(t.replace(/^#+\s+/, ''))));
    }
  }
  return out;
}

function inline(text) {
  const parts = [];
  const re = /(\*\*[^*]+\*\*|`[^`]+`)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    parts.push(m[0].startsWith('**') ? h('strong', {}, m[0].slice(2, -2)) : h('code', {}, m[0].slice(1, -1)));
    last = m.index + m[0].length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

// "Both sides of the call", then "2 Short ideas · working on a job" once there is news. A problem
// with the recording ("Your microphone stopped") comes before either.
function liveLine(m) {
  // The speakers can stop being recorded mid-call (no output device left).
  if (typeof m?.systemAudio === 'boolean') liveBase = m.systemAudio ? 'Both sides of the call' : 'Your microphone only';
  if (m?.warning) return m.warning;
  const bits = [];
  const ideas = (m?.ideas || []).length;
  if (ideas) bits.push(`${ideas} Short idea${ideas === 1 ? '' : 's'}`);
  const working = (m?.jobs || []).filter((j) => j.status === 'working').length;
  if (working) bits.push(working === 1 ? 'working on a job' : `working on ${working} jobs`);
  return bits.length ? bits.join(' · ') : liveBase;
}

// Fills the "Taking notes" card's second line. A warning may take two lines (meeting.rs makes the
// card taller for one), and hovering it gives the whole sentence on what to do.
function showLive(m) {
  if (!liveSub) return;
  liveSub.textContent = liveLine(m);
  liveSub.classList.toggle('warn', !!m?.warning);
  if (m?.warning) liveSub.title = m.warningDetail || m.warning;
  else liveSub.removeAttribute('title');
}

const followTheme = () => invoke('get_settings').then((s) => applyTheme(s?.general?.theme, s?.general?.colour)).catch(() => {});
followTheme();
listen('settings-changed', followTheme);

listen('prompt', render);
// Keeps the clock true to the meeting's own time, and the second line up to date.
listen('meeting', (m) => {
  if (m?.recording && !m.finishing) startedAt = Date.now() - (m.elapsedMs || 0);
  if (liveSub && card.classList.contains('recording') && m?.recording) showLive(m);
});

if (isTauri) {
  invoke('prompt_state').then(render).catch(() => {});
} else {
  // Browser preview: prompt.html?state=ask|recording|warning|writing|done|thinking|answer
  document.body.classList.add('preview');
  const which = new URLSearchParams(location.search).get('state') || 'ask';
  render({
    ask: { state: 'ask', app: 'Microsoft Teams' },
    recording: { state: 'recording', app: 'Microsoft Teams', systemAudio: true, elapsedMs: 754000 },
    warning: { state: 'recording', app: 'Microsoft Teams', systemAudio: true, elapsedMs: 754000, warning: 'Speech engine not ready, holding audio', warningDetail: 'The speech engine isn’t running yet. Navi keeps the audio and writes it down once it is.' },
    writing: { state: 'writing' },
    done: { state: 'done', id: 'n1', title: 'Weekly marketing plan', detail: '3 Short ideas · 4 actions to ClickUp' },
    thinking: { state: 'thinking', title: 'Looking for problems' },
    answer: { state: 'answer', title: 'What\u2019s wrong with it', body: '- **The opening buries the point.** "I wanted to reach out" says nothing; lead with the 15% saving.\n- **"Best in class" is unsupported.** Name the result or cut it.\n- **No ask.** It ends without saying what you want Sam to do next.\n- **Two typos:** "recieve" and "it\u2019s" for "its" in the last line.' },
  }[which]);
  if (which === 'recording') liveSub.textContent = liveLine({ ideas: [{}, {}], jobs: [{ status: 'working' }] });
}
