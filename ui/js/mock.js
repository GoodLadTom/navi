// Sample data for previewing the interface in a browser. Never used inside the app.
const listeners = {};
function emit(event, payload) { (listeners[event] || []).forEach((cb) => cb(payload)); }
export function listen(event, cb) {
  (listeners[event] = listeners[event] || []).push(cb);
  return () => { listeners[event] = listeners[event].filter((f) => f !== cb); };
}

const now = Date.now();
const iso = (minsAgo) => new Date(now - minsAgo * 60000).toISOString();
// A time as a default note title gives it ("14 October, 10:30", meeting.rs default_title).
const titleWhen = (minsAgo) => {
  const d = new Date(now - minsAgo * 60000);
  return `${d.getDate()} ${d.toLocaleString('en-GB', { month: 'long' })}, ${d.toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit' })}`;
};

// The built-in modes exactly as src-tauri/src/text/modes.rs builtin_modes() ships them.
const modes = [
  { id: 'raw', name: 'Raw', icon: 'pen', kind: 'raw', description: 'Exactly what you said. Only spoken punctuation, your word list and snippets apply.', instructions: '', apps: [], useAi: false, builtin: true },
  { id: 'clean', name: 'Clean', icon: 'clean', kind: 'clean', description: 'Your words, tidied: punctuation, capitals, no fillers.', instructions: '', apps: [], useAi: false, builtin: true },
  { id: 'message', name: 'Message', icon: 'chat', kind: 'rewrite', description: 'Short and natural, ready for Slack, Teams or WhatsApp.', instructions: '', apps: ['slack.exe', 'teams.exe', 'ms-teams.exe', 'discord.exe', 'whatsapp.exe', 'telegram.exe', 'signal.exe'], useAi: true, builtin: true },
  { id: 'email', name: 'Email', icon: 'mail', kind: 'rewrite', description: 'Clear paragraphs, ready to send. Greetings and sign-offs only if you say them.', instructions: '', apps: ['outlook.exe', 'olk.exe', 'thunderbird.exe'], useAi: true, builtin: true },
  { id: 'prompt', name: 'Prompt', icon: 'terminal', kind: 'prompt', description: "A clear, direct brief for an AI tool such as Claude Code, Cursor or ChatGPT, with references to your project's files.", instructions: '', apps: ['windowsterminal.exe', 'code.exe', 'cursor.exe', 'windsurf.exe', 'bb.exe', 'claude.exe', 'codex.exe', 'chatgpt.exe', 'powershell.exe', 'pwsh.exe', 'cmd.exe'], useAi: true, builtin: true },
  { id: 'notes', name: 'Notes', icon: 'notes', kind: 'notes', description: 'Ramble freely, get tidy notes and tasks.', instructions: '', apps: [], useAi: true, builtin: true },
];

const takes = [
  { id: 't1', createdAt: iso(4), durationMs: 14200, appName: 'Outlook', windowTitle: 'RE: Slides for the Acme Studio pitch', modeId: 'email', rawText: "hi priya um just to say the slides are done and uploaded to the shared folder er I went with the warmer colours because they read better on a projector let me know if you want the other version instead cheers alex", finalText: "Hi Priya,\n\nJust to say the slides are done and uploaded to the shared folder. I went with the warmer colours because they read better on a projector. Let me know if you'd rather have the other version.\n\nCheers,\nAlex", words: 40, audioPath: 'sounds/warmup.wav', model: 'large-v3-turbo', latencyMs: 3100, status: 'pasted', kind: 'talk', meta: {} },
  { id: 't2', createdAt: iso(26), durationMs: 6100, appName: 'Slack', windowTitle: '#content', modeId: 'message', rawText: "yeah that works for me can we do thursday afternoon instead though", finalText: 'Yeah, that works for me. Can we do Thursday afternoon instead though?', words: 13, audioPath: 'sounds/warmup.wav', model: 'large-v3-turbo', latencyMs: 2800, status: 'pasted', kind: 'talk', meta: {} },
  { id: 't3', createdAt: iso(48), durationMs: 21800, appName: 'Windows Terminal', windowTitle: 'claude ~ navi', modeId: 'prompt', rawText: "so in the history page when you click a take the audio player should show the waveform and let me scrub through it at the moment it only plays from the start can you fix that and keep the styling the same", finalText: "Let me scrub through a take's audio from its waveform on the History page.\n\nContext: clicking a take opens the detail panel (@ui/js/views/history.js), where the player draws the waveform but only plays from the start.\n\nConstraints: keep the current styling.", words: 44, audioPath: 'sounds/warmup.wav', model: 'large-v3-turbo', latencyMs: 4300, status: 'pasted', kind: 'talk', meta: {} },
  { id: 't4', createdAt: iso(95), durationMs: 184000, appName: 'Notion', windowTitle: 'Navi launch', modeId: 'notes', rawText: 'okay so for the launch we need the website live first then the video ...', finalText: "Navi launch plan\n\nSummary: get the website live first, then the film, then send the spring newsletter and post to Hacker News on the same morning.", words: 412, audioPath: 'sounds/warmup.wav', model: 'large-v3-turbo', latencyMs: 11800, status: 'notes', kind: 'ramble', meta: { notes: { title: 'Navi launch plan', summary: 'Get the website live first, then the 80-second film, then send the spring newsletter and post to Hacker News on the same morning.', points: ['The film has no talking: just music and chapters.', 'Keep the download on GitHub releases for now.', 'Windows first, Mac once the core is settled.'], decisions: ['No code signing for the first release.'], tasks: [{ text: 'Finish the landing page copy', owner: 'Alex', due: 'Friday' }, { text: 'Record the screen captures for the film', owner: null, due: null }, { text: 'Ask Priya for the launch post wording', owner: 'Alex', due: null }] } } },
  { id: 't5', createdAt: iso(60 * 20), durationMs: 3200, appName: 'Outlook', windowTitle: 'RE: Slides', modeId: 'email', rawText: 'Navi, make that shorter', finalText: 'Hi Priya, the slides are in the shared folder. I went with the warmer colours. Shout if you want the other version.', words: 22, audioPath: 'sounds/warmup.wav', model: 'large-v3-turbo', latencyMs: 3300, status: 'command', kind: 'command', meta: { command: { type: 'rewrite', instruction: 'make that shorter' } } },
  { id: 't6', createdAt: iso(60 * 26), durationMs: 9400, appName: 'Chrome', windowTitle: 'Acme Studio - Templates', modeId: 'clean', rawText: 'the template library is live now and you can duplicate any of the layouts from the gallery', finalText: 'The template library is live now, and you can duplicate any of the layouts from the gallery.', words: 17, audioPath: 'sounds/warmup.wav', model: 'large-v3-turbo', latencyMs: 520, status: 'pasted', kind: 'talk', meta: {} },
];

// A made-up screenshot for the preview: a plain drawn window with a title and a few bars.
function sampleShot(title, tint) {
  const svg = `<svg xmlns='http://www.w3.org/2000/svg' width='800' height='450' viewBox='0 0 800 450'>
<rect width='800' height='450' fill='#F4F2F8'/><rect x='0' y='0' width='800' height='38' fill='#E6E0EF'/>
<circle cx='22' cy='19' r='6' fill='#D3CBE0'/><circle cx='42' cy='19' r='6' fill='#D3CBE0'/>
<text x='70' y='25' font-family='Segoe UI, sans-serif' font-size='15' fill='#3A3150'>${title}</text>
<rect x='40' y='76' width='300' height='22' rx='6' fill='${tint}'/>
<rect x='40' y='120' width='720' height='12' rx='6' fill='#DAD3E6'/><rect x='40' y='146' width='640' height='12' rx='6' fill='#DAD3E6'/>
<rect x='40' y='172' width='680' height='12' rx='6' fill='#DAD3E6'/>
<rect x='40' y='220' width='220' height='170' rx='12' fill='#FFFFFF'/><rect x='290' y='220' width='220' height='170' rx='12' fill='#FFFFFF'/><rect x='540' y='220' width='220' height='170' rx='12' fill='#FFFFFF'/>
<rect x='64' y='250' width='120' height='16' rx='6' fill='${tint}'/><rect x='314' y='250' width='120' height='16' rx='6' fill='${tint}'/><rect x='564' y='250' width='120' height='16' rx='6' fill='${tint}'/>
</svg>`;
  return 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
}

// Meeting notes (sample calls).
const sampleNotes = [
  {
    id: 'n1', createdAt: iso(70), title: 'Weekly marketing plan', app: 'Microsoft Teams', durationMs: 31 * 60000 + 12000, status: 'done', filePath: 'C:\\Users\\you\\Documents\\Navi Notes\\2026-10-14 1402 Weekly marketing plan.md',
    myNotes: `header image: warmer version?
launch date
who sends the spring newsletter`,
    notesMd: `# Weekly marketing plan
## Summary
A run through this week’s newsletter and the Navi launch. The spring newsletter goes out on Thursday, and the launch moves to the week after so the film can be finished first.
## Key points
- The warmer header image reads better at small sizes, so it stays.
- Two short clips are cut and need captions before Wednesday.
- The launch film needs another pass on the opening ten seconds.
## Decisions
- The spring newsletter goes out on Thursday morning.
- The Navi launch moves back one week.
## Actions
- [ ] Alex: captions on both clips by Wednesday.
- [ ] Them: send the final wording for the launch post.
- [ ] Alex: recut the first ten seconds of the launch film.`,
    jobs: [
      { id: 'j1', atMs: 108000, ask: 'draft a short post saying the launch is moving back a week', status: 'done', error: '', resultMd: `**The Navi launch is moving back a week**

We want the launch film to be properly finished before anyone sees it, so Navi now launches on [date]. Everything else stays the same.

Gaps: the new date.` },
    ],
    shots: [
      { atMs: 12000, path: sampleShot('Newsletter plan - Notion', '#C9B6F5'), app: 'Notion', window: 'Newsletter plan', text: '', caption: '' },
      { atMs: 90000, path: sampleShot('Launch plan - Google Docs', '#8DB8E8'), app: 'Chrome', window: 'Launch plan - Google Docs', text: '', caption: '' },
    ],
    transcript: [
      { atMs: 4000, who: 'them', text: 'Can you hear me all right?' },
      { atMs: 6500, who: 'you', text: 'Yes, loud and clear. Shall we start with the newsletter?' },
      { atMs: 11000, who: 'them', text: 'Go for it. Where are we with the spring one?' },
      { atMs: 15500, who: 'you', text: 'It is written. I went with the warmer header image because it reads better small.' },
      { atMs: 22000, who: 'them', text: 'Agreed, keep that one. Can it go out on Thursday?' },
      { atMs: 26000, who: 'you', text: 'Thursday morning works.' },
      { atMs: 94000, who: 'them', text: 'And the launch, are we still on for next week?' },
      { atMs: 99000, who: 'you', text: 'I would rather move it back a week so the film is properly finished.' },
      { atMs: 105000, who: 'them', text: 'Fine by me. I will send the wording for the launch post.' },
    ],
  },
  {
    id: 'n2', createdAt: iso(60 * 27), title: `Zoom call, ${titleWhen(60 * 27)}`, app: 'Zoom', durationMs: 18 * 60000, status: 'done', filePath: null,
    myNotes: '',
    notesMd: `# Zoom call, ${titleWhen(60 * 27)}

Navi kept the full transcript. To have it written up with a summary, decisions and actions, turn on an AI in Settings > AI and press Write again.
`,
    transcript: [
      { atMs: 3000, who: 'them', text: 'Morning! Thanks for making the time.' },
      { atMs: 6000, who: 'you', text: 'No problem at all. Tell me about the site.' },
    ],
  },
];
sampleNotes.push({
  id: 'w1', createdAt: iso(60 * 5), title: 'Set up the new pricing page', app: 'Watch', durationMs: 14 * 60000, status: 'done', filePath: null, myNotes: '', transcript: [], jobs: [],
  shots: [
    { atMs: 0, path: sampleShot('Pricing - Notion', '#C9B6F5'), app: 'Notion', window: 'Pricing', text: '', caption: 'The three plans drafted in Notion' },
    { atMs: 310000, path: sampleShot('Products - Stripe', '#A9A2E6'), app: 'Chrome', window: 'Products - Stripe', text: '', caption: 'The Pro plan created in Stripe at £19 a month' },
  ],
  notesMd: `# Set up the new pricing page
## Summary
You drafted three plans in Notion, created the Pro plan in Stripe and checked the live page.
## What happened
1. The three plans drafted in Notion (00:00)

![The three plans drafted in Notion](<${sampleShot('Pricing - Notion', '#C9B6F5')}>)

2. The Pro plan created in Stripe at £19 a month (05:10)

![The Pro plan created in Stripe at £19 a month](<${sampleShot('Products - Stripe', '#A9A2E6')}>)`,
});
// A ramble the AI couldn't write up: rough notes made on the computer, saying why (controller.rs
// process_ramble, text/ramble.rs rough_notes_notice). Write again turns them into proper notes.
const rambleSaid = [
  'Okay so for the launch we need the website live first.',
  'Then the film, which still needs another pass on the opening.',
  'I need to email Priya the launch post wording by Friday.',
  'We decided to keep the download on GitHub releases for now.',
];
sampleNotes.push({
  id: 'r1', createdAt: iso(60 * 3), title: 'For the launch we need the website live', app: 'Ramble', durationMs: 3 * 60000 + 20000, status: 'done', filePath: null, myNotes: '', jobs: [], shots: [],
  transcript: rambleSaid.map((text, i) => ({ atMs: i * 50000, who: 'you', text })),
  notesMd: `# For the launch we need the website live

The AI couldn't write these notes: Claude Code took too long. These are rough notes Navi made on your computer. Press Write again on the Notes page to try once more.

## Summary

For the launch we need the website live first. Then the film, which still needs another pass on the opening.

## Key points

- I need to email Priya the launch post wording by Friday.
- We decided to keep the download on GitHub releases for now.

## Decisions

- We decided to keep the download on GitHub releases for now.

## Tasks

- [ ] Email Priya the launch post wording by Friday (due by Friday)`,
});
// What Write again gives the ramble above, as the ramble prompt would (meeting.rs rewrite_ramble).
const rambleRewritten = { title: 'Navi launch order', notesMd: `# Navi launch order

## Summary

Get the website live first, then the film once its opening has had another pass. The download stays on GitHub releases for now.

## Key points

- The website goes live before anything else.
- The film's opening needs another pass.

## Decisions

- Keep the download on GitHub releases for now.

## Tasks

- [ ] Email Priya the launch post wording (due by Friday)` };
// Newest first, the order store/notes.rs lists notes in, so each day heading shows once.
sampleNotes.sort((a, b) => (a.createdAt < b.createdAt ? 1 : -1));
const mockLive = { session: null, timer: null, script: [
  ['them', 'Right, shall we get going?'], ['you', 'Yes. First thing, the menu for the new site.'],
  ['them', 'We want the specials on the home page, changed every Friday.'], ['you', 'That is easy. Who will update them?'],
  ['them', 'Probably me. Can it be done from a phone?'], ['you', 'Yes, I will set that up and show you.'],
] };
let mockUpdate = { state: 'current', current: '0.1.0', version: '', notes: '', progress: 0, message: '' };

function meetingStatus() {
  const s = mockLive.session;
  if (!s) return { recording: false, finishing: false, id: '', title: '', app: '', elapsedMs: 0, lines: [], myNotes: '', systemAudio: false, jobs: [], shots: [], warning: '', warningDetail: '' };
  return { recording: true, finishing: s.finishing, id: s.id, title: s.title, app: s.app, elapsedMs: Date.now() - s.startedAt, lines: s.lines.slice(), myNotes: s.myNotes, systemAudio: true, jobs: s.jobs.slice(), shots: s.shots.slice(), warning: '', warningDetail: '' };
}
let mockWatch = { watching: false, id: '', elapsedMs: 0, shots: 0 };
// How many demo takes the bubble preview has run (simulate_take).
let simulated = 0;

// Settings > Voice commands as Navi ships them. Keep in step with default_voice_commands in
// src-tauri/src/types.rs.
const DEFAULT_VOICE_COMMANDS = [
  { action: 'wake', phrases: ['Navi', 'Hey Navi'] },
  { action: 'scratch_that', phrases: ['scratch that'] },
  { action: 'watch_on', phrases: ['watch my screen'] },
  { action: 'watch_off', phrases: ['stop watching'] },
  { action: 'notes_on', phrases: ['take notes'] },
  { action: 'notes_off', phrases: ['stop taking notes'] },
  { action: 'cancel', phrases: ["don't worry"] },
  { action: 'finish', phrases: ["that's all"] },
  { action: 'colour', phrases: ['turn', 'change to', 'switch to', 'make it'] },
];
const VOICE_ACTIONS = DEFAULT_VOICE_COMMANDS.map((c) => c.action);
const VOICE_LABELS = { wake: 'Wake word', scratch_that: 'Scratch that', watch_on: 'Watch my screen', watch_off: 'Stop watching', notes_on: 'Take notes', notes_off: 'Stop taking notes', cancel: 'Cancel', finish: 'Finish', colour: 'Change colour' };
// What "Navi, turn ..." listens for after the phrase. Keep in step with COLOURS in src-tauri/src/colours.rs.
const COLOUR_WORDS = ['lilac', 'purple', 'lavender', 'gold', 'golden', 'yellow', 'teal', 'green', 'sky', 'blue', 'rose', 'pink', 'graphite', 'grey', 'gray'];
// What Whisper writes for silence and noise, so no phrase may be one of them (types.rs BLOCKED).
const VOICE_BLOCKED = ['thank you', 'thanks', 'thank you very much', 'thank you so much', 'thanks for watching', 'thank you for watching', 'you', 'bye', 'bye bye', 'ok', 'okay', 'yes', 'no', 'so', 'um', 'uh', 'hmm', 'the', 'a', 'i'];
// types.rs phrase_key: "Don't worry!" -> "dont worry".
const phraseKey = (s) => String(s ?? '').toLowerCase().replace(/['’‘]/g, '').replace(/[^\p{L}\p{N}]+/gu, ' ').trim();
// types.rs clean_phrase: trimmed, no punctuation at either end, single spaces. "Hey, Navi" keeps its comma.
const cleanPhrase = (p) => String(p ?? '').trim().replace(/^[^\p{L}\p{N}'’]+|[^\p{L}\p{N}'’]+$/gu, '').replace(/\s+/g, ' ');
const keyWords = (key) => (key ? key.split(' ').length : 0);
// types.rs PHRASE_FOLDS and fold_key: two words the listener hears as one ("do not" is "don't").
const PHRASE_FOLDS = [['do', 'not', 'dont'], ['that', 'is', 'thats'], ['it', 'is', 'its'], ['let', 'us', 'lets'], ['i', 'am', 'im'], ['what', 'is', 'whats'], ['all', 'right', 'alright'], ['can', 'not', 'cant'], ['will', 'not', 'wont'], ['o', 'k', 'ok']];
function foldKey(key) {
  const words = key.split(' ').filter(Boolean);
  const out = [];
  for (let i = 0; i < words.length; i++) {
    const fold = PHRASE_FOLDS.find(([a, b]) => words[i] === a && words[i + 1] === b);
    if (fold) { out.push(fold[2]); i++; } else out.push(words[i]);
  }
  return out.join(' ');
}
// types.rs after_words: what follows the first `n` words of `text`, a word being a run of letters,
// digits and apostrophes with at least one letter or digit in it. Empty when the words run out.
function afterWords(text, n) {
  let count = 0;
  let inWord = false;
  let hasAlnum = false;
  let i = 0;
  for (const c of text) {
    const alnum = /[\p{L}\p{N}]/u.test(c);
    if (alnum || /['’‘]/.test(c)) {
      if (!inWord) { inWord = true; hasAlnum = false; }
      hasAlnum ||= alnum;
    } else if (inWord) {
      inWord = false;
      if (hasAlnum && ++count === n) return text.slice(i);
    }
    i += c.length;
  }
  return '';
}

// Tidies and checks Settings > Voice commands before a save, throwing the message Rust would.
// `before`: the settings before this save, so a phrase used twice names the command that had it.
// Keep in step with VoiceCommandSettings::normalised_from in src-tauri/src/types.rs.
function normaliseVoiceCommands(vc, before = null) {
  const src = vc || {};
  const lists = new Map();
  const unknown = [];
  for (const c of src.commands || []) {
    if (!c || !VOICE_ACTIONS.includes(c.action)) { if (c) unknown.push(c); continue; }
    if (lists.has(c.action)) lists.get(c.action).push(...(c.phrases || []));
    else lists.set(c.action, [...(c.phrases || [])]);
  }
  // 1. complete(): a missing action gets its starting phrases; one left empty on purpose stays off.
  for (const c of DEFAULT_VOICE_COMMANDS) if (!lists.has(c.action)) lists.set(c.action, [...c.phrases]);
  // 2. Cleaned, empty ones dropped.
  for (const [a, list] of lists) lists.set(a, list.map(cleanPhrase).filter((p) => phraseKey(p)));
  // 3. A command typed with the wake word in front loses it: "Navi, take notes" is saved as "take notes".
  const wakeKeys = [...new Set([...lists.get('wake').map(phraseKey), 'navi', 'hey navi', 'hi navi', 'ok navi', 'okay navi'])]
    .sort((x, y) => y.length - x.length);
  for (const a of VOICE_ACTIONS) {
    if (a === 'wake') continue;
    lists.set(a, lists.get(a).map((p) => {
      const key = phraseKey(p);
      const wake = wakeKeys.find((w) => key === w || key.startsWith(`${w} `));
      if (!wake) return p;
      if (key === wake) throw `“${p}” is the wake word. Type what you say after it.`;
      return cleanPhrase(afterWords(p, keyWords(wake)));
    }));
  }
  // 3b. "turn gold" typed into Change colour is saved as "turn": the colour is said after it.
  lists.set('colour', lists.get('colour').map((p) => {
    const words = phraseKey(p).split(' ');
    if (!COLOUR_WORDS.includes(words[words.length - 1])) return p;
    if (words.length === 1) throw `“${p}” is a colour. Type what you say before it, such as “turn”.`;
    return cleanPhrase(p.slice(0, p.length - afterWords(p, words.length - 1).length));
  }));
  // 4. Each phrase on its own.
  for (const a of VOICE_ACTIONS) {
    for (const p of lists.get(a)) {
      const key = phraseKey(p);
      if ([...p].length > 40 || keyWords(key) > 6) throw `“${p}” is too long for a voice command. Keep it to six words or fewer.`;
      if (VOICE_BLOCKED.includes(key)) throw `Navi can’t listen for “${p}”: Whisper often hears it in silence. Pick another phrase.`;
      if ((a === 'cancel' || a === 'finish') && keyWords(key) === 1 && [...key].length < 6) throw `“${p}” is too short to listen for while you’re dictating. Use two words or more, such as “that’s all”.`;
    }
  }
  // 5 and 6. No repeats within an action, and at most eight phrases.
  for (const a of VOICE_ACTIONS) {
    const seen = new Set();
    const list = lists.get(a).filter((p) => { const k = phraseKey(p); if (seen.has(k)) return false; seen.add(k); return true; });
    if (list.length > 8) throw 'Up to eight phrases for each command.';
    lists.set(a, list);
  }
  // 7. A phrase belongs to one action, compared folded. Named: the command that had it before
  // this save, else the earlier one.
  const had = (action, k) => (before?.commands || []).some((c) => c && c.action === action && (c.phrases || []).some((q) => foldKey(phraseKey(cleanPhrase(q))) === k));
  const owner = new Map();
  for (const a of VOICE_ACTIONS) {
    for (const p of lists.get(a)) {
      const k = foldKey(phraseKey(p));
      if (owner.has(k)) {
        const [earlier, earlierP] = owner.get(k);
        if (had(a, k) && !had(earlier, k)) throw `“${earlierP}” is already used for ${VOICE_LABELS[a]}.`;
        throw `“${p}” is already used for ${VOICE_LABELS[earlier]}.`;
      }
      owner.set(k, [a, p]);
    }
  }
  // 8. Something has to wake Navi.
  if (!lists.get('wake').length) throw 'Keep at least one wake word.';
  // 9. In Settings order, with actions from a newer Navi kept as they were, after them.
  return { enabled: !!src.enabled, commands: [...VOICE_ACTIONS.map((action) => ({ action, phrases: lists.get(action) })), ...unknown] };
}

const daily = Array.from({ length: 14 }, (_, i) => ({ date: new Date(now - (13 - i) * 86400000).toISOString().slice(0, 10), words: [320, 610, 180, 0, 890, 1240, 760, 430, 980, 1120, 640, 1510, 1380, 1730][i] }));

const state = {
  settings: {
    onboarded: true, userName: 'Alex',
    hotkeys: { talk: ['Ctrl', 'Win'], rambleKey: 'Space', doubleTapHandsFree: true, cancelKey: 'Escape', modeKeys: [{ mode: 'email', keys: ['F4'] }] },
    sounds: { enabled: true, set: 'hush', volume: 0.7, custom: {}, whileTalking: 'pause' },
    mic: { device: null, trimCue: true },
    engine: { model: 'large-v3-turbo', language: 'en', useGpu: true, serverPath: null, modelPath: null, vad: true },
    // Claude Code on the Claude plan the sample user already has: no API key (text/cli_ai.rs).
    ai: { provider: 'claude-code', model: 'haiku', baseUrl: null, longModel: '' },
    writing: { british: true, noEmDashes: true, removeFillers: true, styleNotes: 'Short, warm, direct. British spelling. No em dashes. Sign off with "Cheers, Alex" on email.' },
    context: { readApp: true, readField: true, ocr: false, excludedApps: ['1Password.exe', 'KeePass.exe', 'Bitwarden.exe'] },
    modes: { defaultMode: 'clean', autoByApp: true, active: '' },
    learning: { enabled: true, autoAddAfter: 2, checkAfterSecs: 25 },
    agent: { projects: [{ id: 'p1', name: 'navi', path: 'C:\\Users\\you\\Projects\\Navi' }], activeProject: 'p1', mentionStyle: 'at' },
    ramble: { notesDir: null, pasteNotes: false, tasksTo: 'clickup', clickupListId: '901204', notionDatabaseId: null },
    privacy: { localOnly: false, keepAudioDays: 30, saveHistory: true },
    general: { launchAtLogin: true, startMinimised: true, pasteMethod: 'paste', restoreClipboard: true, showPill: true, showFirefly: true, closeToTray: true, trayTipShown: false, checkUpdates: true, theme: 'light', colour: 'lilac' },
    speak: { voiceId: 'v2', model: 'eleven_v4_turbo' },
    notes: { detectCalls: true, systemAudio: true, autoStop: true, callJobs: true, callScreenshots: false, actionsTo: 'clickup' },
    voiceCommands: { enabled: true, commands: structuredClone(DEFAULT_VOICE_COMMANDS) },
    watch: { everySecs: 10 },
  },
  takes,
  notes: sampleNotes,
  words: [
    { id: 'w1', term: 'Supabase', soundsLike: ['super base', 'supa base'], note: '', source: 'manual', createdAt: iso(9000), uses: 41 },
    { id: 'w2', term: 'Quikly', soundsLike: ['quickly', 'quick ly'], note: '', source: 'learned', createdAt: iso(4000), uses: 17 },
    { id: 'w3', term: 'Figma', soundsLike: ['fig ma'], note: 'The design tool', source: 'ai-tools', createdAt: iso(3000), uses: 22 },
    { id: 'w4', term: 'HyperFrames', soundsLike: ['hyper frames'], note: '', source: 'manual', createdAt: iso(2000), uses: 6 },
    { id: 'w5', term: 'ElevenLabs', soundsLike: ['eleven labs'], note: '', source: 'ai-tools', createdAt: iso(1000), uses: 4 },
  ],
  snippets: [
    { id: 's1', trigger: 'my calendar link', expansion: 'https://cal.example/alex/30min', source: 'manual' },
    { id: 's2', trigger: 'sign off', expansion: 'Cheers,\nAlex', source: 'manual' },
  ],
  suggestions: [
    { id: 'g1', from: 'joe', to: 'Jo', count: 1, appName: 'Slack', lastSeen: iso(30) },
    { id: 'g2', from: 'next js', to: 'Next.js', count: 1, appName: 'Windows Terminal', lastSeen: iso(80) },
  ],
  packs: [
    { id: 'ai-tools', name: 'AI tools starter', author: 'GoodLad', description: 'The names of the AI tools people talk about every day, spelt the way they should be.', version: '1.0', words: 14, snippets: 0, modes: 0, sourceUrl: null, installedAt: iso(5000) },
  ],
  stats: { wordsToday: 1730, wordsWeek: 7920, wordsTotal: 48210, takesTotal: 1268, minutesSaved: 912, avgWpm: 148, streakDays: 12, daily },
  engine: { state: 'ready', message: 'Ready', model: 'large-v3-turbo', gpu: 'NVIDIA GeForce RTX 4060 (8 GB)', device: 'cuda' },
  models: [
    { id: 'large-v3-turbo', name: 'Whisper Large v3 Turbo', file: 'ggml-large-v3-turbo.bin', url: '', sizeMb: 1624, languages: '99 languages', speed: 4, accuracy: 5, recommended: true, installed: true, path: 'C:\\Users\\you\\.local\\share\\whisper\\ggml-large-v3-turbo.bin' },
    { id: 'large-v3-turbo-q5_0', name: 'Whisper Large v3 Turbo (compact)', file: 'ggml-large-v3-turbo-q5_0.bin', url: '', sizeMb: 574, languages: '99 languages', speed: 4, accuracy: 4, recommended: false, installed: false, path: null },
    { id: 'small.en', name: 'Whisper Small (English)', file: 'ggml-small.en.bin', url: '', sizeMb: 488, languages: 'English', speed: 5, accuracy: 3, recommended: false, installed: false, path: null },
    { id: 'base.en', name: 'Whisper Base (English)', file: 'ggml-base.en.bin', url: '', sizeMb: 148, languages: 'English', speed: 5, accuracy: 2, recommended: false, installed: false, path: null },
    // As models.rs: the silence model is listed too (Settings shows it only under Trim silences).
    { id: 'vad', name: 'Silence trimming (Silero VAD)', file: 'ggml-silero-v5.1.2.bin', url: '', sizeMb: 1, languages: 'Any language', speed: 5, accuracy: 0, recommended: false, installed: true, path: 'C:\\Users\\you\\AppData\\Roaming\\com.goodlad.navi\\models\\ggml-silero-v5.1.2.bin' },
  ],
  devices: [{ name: 'Microphone (USB microphone)', isDefault: true }, { name: 'Laptop microphone array', isDefault: false }],
};
// Any preview page can open in a colour and a mode: index.html?colour=gold&theme=dark, and the same
// for pill.html and prompt.html, whose sample settings are their own.
{
  const q = new URLSearchParams(location.search);
  if (q.get('colour')) state.settings.general.colour = q.get('colour');
  if (q.get('theme')) state.settings.general.theme = q.get('theme');
  // Settings > AI with Ollama: index.html?ollama=on opens with it chosen and running, ?ollama=off
  // with it chosen but not running, as llm.rs ollama_status reports them.
  if (q.get('ollama')) { state.settings.ai = { provider: 'ollama', model: 'llama3.2', baseUrl: null, longModel: '' }; state.ollamaRunning = q.get('ollama') !== 'off'; }
}
// What Ollama has pulled in the preview (sizes as Ollama lists them).
const OLLAMA_MODELS = [{ name: 'llama3.2', size: 2019393189 }, { name: 'qwen3:8b', size: 5225388164 }, { name: 'gemma3', size: 3338801804 }];
const ollamaAddress = () => (state.settings.ai.provider === 'ollama' && state.settings.ai.baseUrl) || 'http://127.0.0.1:11434';

let levelTimer = null;
function startLevels() {
  stopLevels();
  let t = 0;
  levelTimer = setInterval(() => {
    t += 0.05;
    const speech = Math.max(0, Math.sin(t * 2.3) * 0.5 + Math.sin(t * 5.1) * 0.3 + Math.random() * 0.35);
    emit('level', Math.min(1, speech));
  }, 33);
}
function stopLevels() { clearInterval(levelTimer); levelTimer = null; }

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// The shortcut tester in a browser: keys pressed in the preview window stand in for the hook.
const CODE_NAMES = { ControlLeft: 'LCtrl', ControlRight: 'RCtrl', AltLeft: 'LAlt', AltRight: 'RAlt', ShiftLeft: 'LShift', ShiftRight: 'RShift', MetaLeft: 'LWin', MetaRight: 'RWin', Escape: 'Escape', Space: 'Space', CapsLock: 'CapsLock', Backslash: 'Backslash' };
const codeName = (code) => CODE_NAMES[code] || code.replace(/^Key/, '').replace(/^Digit/, '');
const SIDE_OF = { Ctrl: ['LCtrl', 'RCtrl'], Shift: ['LShift', 'RShift'], Alt: ['LAlt', 'RAlt'], Win: ['LWin', 'RWin'] };
let monitor = null;
function startMonitor() {
  if (monitor) return;
  const held = new Set();
  let active = false;
  const chordHeld = () => state.settings.hotkeys.talk.every((k) => held.has(k) || (SIDE_OF[k] || []).some((s) => held.has(s)));
  const onDown = (e) => {
    const name = codeName(e.code);
    if (held.has(name)) return;
    held.add(name);
    emit('key-seen', { name, label: name, up: false, injected: false });
    if (!active && chordHeld()) { active = true; emit('shortcut', 'down'); }
  };
  const onUp = (e) => {
    const name = codeName(e.code);
    held.delete(name);
    emit('key-seen', { name, label: name, up: true, injected: false });
    if (active && !chordHeld()) { active = false; emit('shortcut', 'up'); }
  };
  window.addEventListener('keydown', onDown);
  window.addEventListener('keyup', onUp);
  monitor = () => { window.removeEventListener('keydown', onDown); window.removeEventListener('keyup', onUp); };
}
function stopMonitor() { monitor?.(); monitor = null; }

function checkShortcut(keys) {
  const LABELS = { RCtrl: 'Right Ctrl', LCtrl: 'Left Ctrl', RAlt: 'Right Alt', LAlt: 'Left Alt', LShift: 'Left Shift', RShift: 'Right Shift', LWin: 'Left Win', RWin: 'Right Win', CapsLock: 'Caps Lock', Backslash: '\\', Escape: 'Esc' };
  const label = keys.map((k) => LABELS[k] || k).join(' + ');
  if (keys.length !== 1) return { label, error: null, warning: null };
  const [k] = keys;
  if (keys.length === 2 && keys[0] === 'Ctrl' && keys[1] === 'V') return { label, error: 'Ctrl + V is Paste in most apps, so Navi would take it over. Pick something else.', warning: null };
  if (['Ctrl', 'Shift', 'Alt', 'Win'].includes(k)) return { label, error: `${k} on its own is part of too many shortcuts. Pick one side, such as Right Ctrl, or add another key.`, warning: null };
  if (k === 'Escape') return { label, error: 'Esc cancels a take, so it can\'t start one too. Pick another key.', warning: null };
  if (/^[A-Z0-9]$/.test(k) || ['Space', 'Enter', 'Tab', 'Backspace', 'Delete'].includes(k)) return { label, error: `${label} on its own would stop you typing it. Pick a key you never type with, such as Right Ctrl or Caps Lock.`, warning: null };
  if (['Backslash', 'Backquote', 'Slash', 'Quote', 'Semicolon', 'Comma', 'Period', 'Minus', 'Equal', 'BracketLeft', 'BracketRight'].includes(k)) return { label, error: null, warning: `You won't be able to type ${label} while Navi is running.` };
  if (k === 'LCtrl') return { label, error: null, warning: 'Left Ctrl is part of everyday shortcuts like Ctrl + C, so Navi may start listening when you don\'t mean it to. Right Ctrl is a safer pick.' };
  if (k === 'CapsLock') return { label, error: null, warning: 'Caps Lock won\'t switch capitals on by itself any more. Shift + Caps Lock still does.' };
  return { label, error: null, warning: null };
}

const handlers = {
  app_info: () => ({ version: '0.1.0', chordLabel: state.settings.hotkeys.talk.join(' + '), dataDir: 'C:\\Users\\you\\AppData\\Roaming\\com.goodlad.navi', engine: state.engine, onboarded: state.settings.onboarded }),
  get_settings: () => structuredClone(state.settings),
  // As commands.rs: voice commands are tidied and checked first, and the tidied settings come back.
  // Phrases already saved that fail today's checks never stop something else (the theme) saving.
  save_settings: ({ settings }) => {
    let voiceCommands;
    try { voiceCommands = normaliseVoiceCommands(settings.voiceCommands, state.settings.voiceCommands); } catch (e) {
      if (JSON.stringify(settings.voiceCommands) !== JSON.stringify(state.settings.voiceCommands)) throw e;
      voiceCommands = settings.voiceCommands;
    }
    const next = { ...settings, voiceCommands };
    const was = state.settings.general;
    state.settings = next;
    // As commands.rs: a new theme, colour or firefly setting goes to every window at once.
    if (was.theme !== next.general.theme || was.colour !== next.general.colour || was.showFirefly !== next.general.showFirefly) setTimeout(() => emit('settings-changed', null), 0);
    return structuredClone(next);
  },
  voice_command_defaults: () => ({ enabled: false, commands: structuredClone(DEFAULT_VOICE_COMMANDS) }),
  get_stats: () => state.stats,
  list_takes: ({ query }) => {
    let list = state.takes;
    if (query?.search) { const s = query.search.toLowerCase(); list = list.filter((t) => (t.finalText + t.rawText + t.appName).toLowerCase().includes(s)); }
    if (query?.kind) list = list.filter((t) => t.kind === query.kind);
    if (query?.app) list = list.filter((t) => t.appName === query.app);
    if (query?.mode) list = list.filter((t) => t.modeId === query.mode);
    return list;
  },
  get_take: ({ id }) => state.takes.find((t) => t.id === id) || null,
  delete_take: ({ id }) => { state.takes = state.takes.filter((t) => t.id !== id); },
  clear_history: () => { state.takes = []; },
  // As commands.rs: a failed take that is re-run stops showing as Failed.
  rerun_take: async ({ id }) => { await wait(900); const t = state.takes.find((x) => x.id === id); if (t?.status === 'error') { t.status = 'rerun'; delete t.meta.error; } return t; },
  app_names: () => [...new Set(state.takes.map((t) => t.appName))],
  list_modes: () => modes,
  save_mode: ({ mode }) => { const i = modes.findIndex((m) => m.id === mode.id); if (i >= 0) modes[i] = mode; else modes.push({ ...mode, id: mode.id || mode.name.toLowerCase().replace(/\W+/g, '-') }); emit('modes', null); return mode; },
  // As lib.rs modes_changed: a deleted mode's key, and the active or default mode if it was this one, go too.
  delete_mode: ({ id }) => {
    const i = modes.findIndex((m) => m.id === id);
    if (i < 0 || modes[i].builtin) return;
    modes.splice(i, 1);
    const s = state.settings;
    s.hotkeys.modeKeys = (s.hotkeys.modeKeys || []).filter((k) => k.mode !== id);
    if (s.modes.active === id) s.modes.active = '';
    if (s.modes.defaultMode === id) s.modes.defaultMode = 'clean';
    emit('settings-changed', null);
    emit('modes', null);
  },
  preview_mode: async ({ text }) => { await wait(700); return text.replace(/\bum\b|\ber\b/g, '').replace(/\s+/g, ' ').trim(); },
  set_active_mode: ({ id }) => {
    const choosable = modes.filter((m) => m.kind !== 'notes').map((m) => m.id);
    const cur = state.settings.modes.active || '';
    let next = id === 'auto' ? '' : id;
    if (id === 'next') { const i = choosable.indexOf(cur); next = i < 0 ? choosable[0] : (choosable[i + 1] || ''); }
    state.settings.modes.active = next;
    const name = modes.find((m) => m.id === next)?.name || 'Auto';
    // As controller.rs switch_mode: with "Pick the mode from the app" off, Auto is the default mode.
    const fallback = modes.find((m) => m.id === state.settings.modes.defaultMode && m.kind !== 'notes')?.name || 'Clean';
    const auto = state.settings.modes.autoByApp ? 'Mode: Auto, picked from the app' : `Mode: Auto (your default, ${fallback})`;
    emit('pill', { state: 'notice', message: next ? `Mode: ${name}` : auto });
    setTimeout(() => emit('pill', { state: 'hidden' }), 1500);
    emit('settings-changed', null);
  },
  check_mode_key: ({ keys }) => ({ label: keys.join(' + '), labels: keys, error: null, warning: null }),
  list_words: () => state.words,
  save_word: ({ word }) => { if (!word.id) { word.id = 'w' + Math.random().toString(36).slice(2, 7); word.createdAt = new Date().toISOString(); word.uses = 0; state.words.unshift(word); } else { const i = state.words.findIndex((w) => w.id === word.id); state.words[i] = word; } return word; },
  delete_word: ({ id }) => { state.words = state.words.filter((w) => w.id !== id); },
  list_snippets: () => state.snippets,
  save_snippet: ({ snippet }) => { if (!snippet.id) { snippet.id = 's' + Math.random().toString(36).slice(2, 7); state.snippets.unshift(snippet); } else { const i = state.snippets.findIndex((s) => s.id === snippet.id); state.snippets[i] = snippet; } return snippet; },
  delete_snippet: ({ id }) => { state.snippets = state.snippets.filter((s) => s.id !== id); },
  list_suggestions: () => state.suggestions,
  accept_suggestion: ({ id }) => { const s = state.suggestions.find((x) => x.id === id); state.suggestions = state.suggestions.filter((x) => x.id !== id); const w = { id: 'w' + id, term: s.to, soundsLike: [s.from], note: '', source: 'learned', createdAt: new Date().toISOString(), uses: 0 }; state.words.unshift(w); return w; },
  dismiss_suggestion: ({ id }) => { state.suggestions = state.suggestions.filter((x) => x.id !== id); },
  list_packs: () => state.packs,
  export_pack: async () => { await wait(300); return { id: 'mine', name: 'My words' }; },
  import_pack_file: async () => { await wait(300); return state.packs[0]; },
  install_pack_url: async () => { await wait(600); return state.packs[0]; },
  install_pack_json: async () => { await wait(300); return state.packs[0]; },
  remove_pack: ({ id }) => { state.packs = state.packs.filter((p) => p.id !== id); emit('modes', null); },
  list_devices: () => state.devices,
  mic_test_start: () => { startLevels(); },
  mic_test_stop: () => { stopLevels(); },
  capture_shortcut: async () => { await wait(1600); return ['Backslash']; },
  check_shortcut: ({ keys }) => ({ ...checkShortcut(keys), labels: keys.map((k) => ({ RCtrl: 'Right Ctrl', RAlt: 'Right Alt', CapsLock: 'Caps Lock', Backslash: '\\', Oem8: '`' }[k] || k)) }),
  cancel_shortcut_capture: () => null,
  detect_ai_tools: () => [
    { id: 'claude-code', name: 'Claude Code', plan: 'your Claude plan', found: true, path: 'C:\\Users\\you\\.local\\bin\\claude.exe', models: ['haiku', 'sonnet', 'opus'], installHint: 'Install Claude Code from claude.com/claude-code, then run claude once in a terminal to sign in.' },
    { id: 'codex', name: 'Codex', plan: 'your ChatGPT plan', found: true, path: 'codex.js', models: ['gpt-6-luna', 'gpt-5.6-luna', 'gpt-6.1-sol'], installHint: 'Install Codex with: npm install -g @openai/codex, then run codex once in a terminal to sign in.' },
  ],
  list_sound_sets: async () => {
    try { return await (await fetch('sounds/sets/index.json')).json(); } catch { return { default: 'hush', sets: [] }; }
  },
  key_labels: ({ keys }) => keys.map((k) => ({ RCtrl: 'Right Ctrl', RAlt: 'Right Alt', CapsLock: 'Caps Lock', Backslash: '\\', Oem8: '`', Escape: 'Esc' }[k] || k)),
  key_monitor: ({ on }) => { if (on) startMonitor(); else stopMonitor(); },
  other_dictation_apps: () => ['Superwhisper'],
  shortcut_status: () => ({ running: true, label: state.settings.hotkeys.talk.join(' + '), health: { quietSecs: 0, reinstalls: 0, busyMisses: 0 } }),
  hide_main: () => null,
  list_models: () => state.models,
  download_model: async ({ id }) => {
    const total = state.models.find((m) => m.id === id).sizeMb * 1e6;
    for (let i = 1; i <= 20; i++) { await wait(120); emit('download', { id, received: total * i / 20, total, state: i === 20 ? 'done' : 'downloading', message: '' }); }
    state.models.find((m) => m.id === id).installed = true;
  },
  download_engine: async () => null,
  engine_status: () => state.engine,
  restart_engine: async () => { await wait(800); return state.engine; },
  detect_setup: () => ({ gpu: 'NVIDIA GeForce RTX 4060 (8 GB)', serverFound: true, modelFound: 'C:\\Users\\you\\.local\\share\\whisper\\ggml-large-v3-turbo.bin', modelId: 'large-v3-turbo' }),
  play_cue: () => null,
  set_secret: () => null,
  has_secret: ({ name }) => name === 'elevenlabs',
  eleven_voices: async () => { await wait(300); return [
    { voiceId: 'v1', name: 'Alice', category: 'premade', description: 'british, female, middle-aged, confident' },
    { voiceId: 'v2', name: 'George', category: 'premade', description: 'british, male, warm' },
    { voiceId: 'v3', name: 'My voice (clone)', category: 'cloned', description: '' },
  ]; },
  eleven_models: async () => [{ modelId: 'eleven_v4_turbo', name: 'Eleven v4 Turbo' }, { modelId: 'eleven_multilingual_v2', name: 'Eleven Multilingual v2' }],
  speak_preview: async () => { await wait(500); return 'sounds/sets/felt/done.wav'; },
  suggest_clip_path: ({ text, voiceName }) => `C:\\Users\\you\\Downloads\\${(voiceName || 'Navi voice clip').split(/[\u2013\u2014,(-]/)[0].trim()} - ${String(text || '').split(/\s+/).slice(0, 7).join(' ')}.wav`,
  save_speech: async ({ path }) => { await wait(400); return path; },
  reveal_file: () => null,
  open_link: () => null,
  test_ai: async () => {
    // As llm.rs describe: Ollama not running says so, with the address nothing answered at.
    if (state.settings.ai.provider === 'ollama' && state.ollamaRunning === false) { await wait(1000); throw `Ollama isn't installed or isn't running. Nothing answered at ${ollamaAddress()}.`; }
    await wait(700); return 'ready';
  },
  // As llm.rs ollama_status (GET /api/tags): running with what it has pulled, or why not.
  ollama_status: async () => {
    const address = ollamaAddress();
    if (state.settings.privacy.localOnly && !/^https?:\/\/(localhost|127\.|\[::1\])/i.test(address)) return { address, running: false, models: [], message: 'Local only is on, so Navi won\'t ask Ollama at an address off this computer.' };
    if (state.ollamaRunning === false) { await wait(1000); return { address, running: false, models: [], message: 'Ollama isn\'t installed or isn\'t running.' }; }
    await wait(150);
    return { address, running: true, models: structuredClone(OLLAMA_MODELS), message: '' };
  },
  test_integration: async ({ name }) => {
    // As commands.rs test_integration_blocked: Local only keeps the Test buttons offline too.
    if (state.settings.privacy.localOnly) throw `Local only is on, so Navi won't contact ${name === 'clickup' ? 'ClickUp' : 'Notion'}. Turn it off in Settings > Context and privacy.`;
    await wait(600); return name === 'clickup' ? 'Connected as Alex' : 'Connected to Navi notes'; },
  generate_style_notes: async () => { await wait(1200); return 'Short, warm, direct. British spelling. Uses "cheers" to sign off. Prefers plain words over jargon.'; },
  add_project: ({ path }) => ({ id: 'p' + Math.random().toString(36).slice(2, 6), name: path.split(/[\\/]/).pop(), path }),
  finish_onboarding: () => { state.settings.onboarded = true; },
  copy_text: () => null,
  open_data_folder: () => null,
  quit_app: () => null,
  meeting_status: () => meetingStatus(),
  start_meeting: async ({ source }) => {
    if (mockLive.session) throw 'Navi is already taking notes.';
    await wait(400);
    const id = 'live' + Date.now();
    mockLive.session = { id, title: 'Notes, 14 October, 15:02', app: source || '', startedAt: Date.now(), lines: [], myNotes: '', finishing: false, jobs: [], shots: [] };
    let i = 0;
    mockLive.timer = setInterval(() => {
      const s = mockLive.session;
      if (!s || s.finishing || i >= mockLive.script.length) return;
      const [who, text] = mockLive.script[i++];
      s.lines.push({ atMs: Date.now() - s.startedAt, who, text });
      if (i === 4) s.jobs.push({ id: 'lj1', atMs: Date.now() - s.startedAt, ask: 'list what the client needs from me for the menu', status: 'working', resultMd: '', error: '' });
      if (i === 5) s.jobs[0] = { ...s.jobs[0], status: 'done', resultMd: '- The Friday specials, each week\n- Photos of the dishes\n- Who updates the site from a phone' };
      emit('meeting', meetingStatus());
    }, 2500);
    emit('meeting', meetingStatus());
    return id;
  },
  stop_meeting: async () => {
    const s = mockLive.session;
    if (!s) throw "Navi isn't taking notes.";
    s.finishing = true;
    emit('meeting', meetingStatus());
    await wait(1200);
    clearInterval(mockLive.timer);
    const note = { id: s.id, createdAt: new Date(s.startedAt).toISOString(), title: 'Menu and specials for the new site', app: s.app, durationMs: Date.now() - s.startedAt, status: 'done', filePath: null, myNotes: s.myNotes, transcript: s.lines, jobs: s.jobs, shots: s.shots,
      notesMd: `# Menu and specials for the new site
## Summary
The specials go on the home page and change every Friday, updated from a phone.
## Actions
- [ ] You: set up phone editing for the specials and show them how.` };
    mockLive.session = null;
    state.notes.unshift(note);
    emit('meeting', meetingStatus());
    emit('note', note);
  },
  set_my_notes: ({ text }) => { if (mockLive.session) mockLive.session.myNotes = text; },
  set_meeting_title: ({ title }) => { if (mockLive.session) { mockLive.session.title = title; emit('meeting', meetingStatus()); } },
  list_notes: () => state.notes.map((n) => ({ ...n, transcript: [], jobs: [], shots: [], counts: { jobs: (n.jobs || []).length, shots: (n.shots || []).length } })),
  get_note: ({ id }) => { const n = state.notes.find((x) => x.id === id); return n ? structuredClone({ jobs: [], shots: [], ...n }) : null; },
  send_note_actions: async () => { await wait(700); return '3 actions to ClickUp'; },
  watch_status: () => ({ ...mockWatch }),
  start_watch: () => { mockWatch = { watching: true, id: 'mw', elapsedMs: 0, shots: 0 }; emit('watch', { ...mockWatch }); },
  stop_watch: () => { mockWatch = { watching: false, id: '', elapsedMs: 0, shots: 0 }; emit('watch', { ...mockWatch }); },
  update_note: ({ id, title, notesMd }) => {
    const n = state.notes.find((x) => x.id === id);
    if (notesMd != null) { n.notesMd = notesMd; const m = /^#\s+(.+)$/m.exec(notesMd); if (m) n.title = m[1].trim(); }
    if (title) { n.title = title; n.notesMd = /^#\s+.+$/m.test(n.notesMd) ? n.notesMd.replace(/^#\s+.+$/m, `# ${title}`) : `# ${title}\n\n${n.notesMd}`; }
    return structuredClone(n);
  },
  rewrite_note: async ({ id }) => {
    await wait(1500);
    const n = state.notes.find((x) => x.id === id);
    if (n.id === 'r1') Object.assign(n, rambleRewritten);
    emit('note', n);
    return structuredClone(n);
  },
  delete_note: ({ id }) => { state.notes = state.notes.filter((n) => n.id !== id); },
  prompt_state: () => ({ state: 'hidden' }),
  update_status: () => ({ ...mockUpdate }),
  check_for_updates: async () => {
    await wait(700);
    mockUpdate = { state: 'available', current: '0.1.0', version: '0.1.1', notes: 'Firefly icon, blue accents and updates inside Navi.', progress: 0, message: '' };
    emit('update', { ...mockUpdate });
    return { ...mockUpdate };
  },
  install_update: async () => {
    for (let i = 0; i <= 10; i++) { mockUpdate = { ...mockUpdate, state: 'downloading', progress: i / 10 }; emit('update', { ...mockUpdate }); await wait(220); }
  },
  dismiss_meeting_prompt: () => null,
  open_note: () => null,
  reveal_note_file: () => null,
  simulate_take: async () => {
    // Every third time, a ramble the AI couldn't write up: the bubble says rough notes and why.
    if (++simulated % 3 === 0) {
      emit('pill', { state: 'listening', kind: 'ramble', modeName: 'Notes', appLabel: 'Notion' }); startLevels();
      await wait(1200); emit('ramble-partial', 'so the website goes live first and then the film');
      await wait(1800); stopLevels();
      emit('pill', { state: 'processing', modeName: 'Notes' });
      await wait(1200);
      emit('pill', { state: 'notice', message: "Saved rough notes: the AI couldn't write them (Claude Code took too long) · 1 task" });
      await wait(5000); emit('pill', { state: 'hidden' });
      return;
    }
    emit('pill', { state: 'listening', kind: 'talk', modeName: 'Email', appLabel: 'Outlook' }); startLevels();
    await wait(2600); stopLevels();
    emit('pill', { state: 'processing', modeName: 'Email' });
    await wait(900);
    emit('pill', { state: 'done', preview: 'Hi Priya, just to say…', outcome: 'pasted' });
    await wait(900); emit('pill', { state: 'hidden' });
  },
};

export async function invoke(cmd, args) {
  const h = handlers[cmd];
  if (!h) { console.warn('mock: no handler for', cmd); return null; }
  await wait(40);
  return h(args || {});
}
