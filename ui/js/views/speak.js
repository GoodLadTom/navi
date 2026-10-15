// Speak: type or dictate a sentence or a paragraph, then hear it in your ElevenLabs voice.
import { invoke, listen, saveFile } from '../bridge.js';
import { h, ico, btn, select, toast, errText } from '../ui.js';

// Navi sends ElevenLabs this many characters at most (MAX_CHARS in speak.rs) and cuts the rest.
const MAX_CHARS = 5000;
const num = (n) => n.toLocaleString('en-GB');
// Characters as speak.rs counts them (an emoji is one, not two).
const chars = (text) => Array.from(text).length;
const tooLongNote = (text) => `Navi reads up to ${num(MAX_CHARS)} characters and this is ${num(chars(text))}.`;

export async function render(root, { store, go }) {
  const cleanups = [];
  let audio = null;

  root.append(h('div', { class: 'page-head' },
    h('div', {}, h('div', { class: 'eyebrow' }, 'Your words, out loud'), h('h1', { class: 'h1' }, 'Speak'),
      h('p', { class: 'lead' }, 'Click in the box, hold your shortcut and talk, or type. Then play it back in your ElevenLabs voice.'))));

  const area = h('textarea', { class: 'practice speak-area', placeholder: 'Say or type a sentence or a paragraph…' });
  const voiceHost = h('div', { class: 'row', style: { gap: '8px' } });
  const status = h('p', { class: 'small faint speak-status' });
  const play = btn('Play', { variant: 'primary', icon: 'play' });
  const stop = btn('Stop', { variant: 'ghost', icon: 'pause' });
  const download = btn('Download', { variant: 'ghost', icon: 'download', title: 'Save this clip as a WAV file' });
  const last = btn('Use my last take', { variant: 'ghost', icon: 'history' });
  let voiceChoice = null;
  let voices = [];

  play.onclick = async () => {
    const text = area.value.trim();
    if (!text) { status.textContent = 'Say or type something first.'; return; }
    play.disabled = true; status.textContent = 'Asking ElevenLabs…';
    try {
      const url = await invoke('speak_preview', { text, voiceId: voiceChoice });
      audio?.pause();
      audio = new Audio(url);
      audio.addEventListener('ended', () => { status.textContent = ''; });
      await audio.play();
      status.textContent = chars(text) > MAX_CHARS ? `Playing the first part only. ${tooLongNote(text)}` : `Playing ${num(chars(text))} characters.`;
    } catch (e) {
      status.textContent = errText(e);
    }
    play.disabled = false;
  };
  stop.onclick = () => { audio?.pause(); status.textContent = ''; };
  // Saves the clip as a WAV file. Straight after Play it is the very clip you heard, and
  // ElevenLabs isn't asked again; otherwise Navi makes it first.
  download.onclick = async () => {
    const text = area.value.trim();
    if (!text) { status.textContent = 'Say or type something first.'; return; }
    const voiceName = voices.find((v) => v.voiceId === voiceChoice)?.name || null;
    const suggested = await invoke('suggest_clip_path', { text, voiceName }).catch(() => null);
    const path = await saveFile({ title: 'Save the voice clip', defaultPath: suggested || 'Navi voice clip.wav', filters: [{ name: 'WAV audio', extensions: ['wav'] }] });
    if (!path) return;
    download.disabled = true;
    status.textContent = 'Saving the clip…';
    try {
      const where = await invoke('save_speech', { text, voiceId: voiceChoice, path });
      const file = String(where).split(/[\\/]/).pop();
      const part = chars(text) > MAX_CHARS ? `It has the first part only. ${tooLongNote(text)} ` : '';
      status.replaceChildren(`Saved ${file}. ${part}`, h('button', { onClick: () => invoke('reveal_file', { path: where }).catch((e) => toast(errText(e), { error: true })) }, 'Show it'));
    } catch (e) {
      status.textContent = errText(e);
    }
    download.disabled = false;
  };
  last.onclick = async () => {
    const t = (await invoke('list_takes', { query: { limit: 1, offset: 0 } }).catch(() => []))[0];
    if (!t?.finalText) { toast('No takes yet'); return; }
    area.value = t.finalText; area.focus();
  };

  root.append(h('div', { class: 'card speak-card' }, area,
    h('div', { class: 'row between', style: { marginTop: '14px', flexWrap: 'wrap', gap: '10px' } },
      h('div', { class: 'row', style: { gap: '8px' } }, play, stop, download, last), voiceHost),
    status));

  async function loadVoices() {
    const has = await invoke('has_secret', { name: 'elevenlabs' }).catch(() => false);
    if (!has) {
      voiceHost.replaceChildren(h('span', { class: 'small quiet' }, 'Connect ElevenLabs to choose a voice.'),
        btn('Connect', { size: 'sm', icon: 'link', onClick: () => go('settings', 'voice') }));
      play.disabled = true;
      download.disabled = true;
      return;
    }
    try {
      voices = await invoke('eleven_voices');
      const cur = store.settings.speak?.voiceId || voices[0]?.voiceId || '';
      voiceChoice = cur || null;
      voiceHost.replaceChildren(h('span', { class: 'small quiet' }, 'Voice'),
        select(voices.map((v) => ({ value: v.voiceId, label: v.name })), cur, (id) => { voiceChoice = id; }));
      play.disabled = false;
      download.disabled = false;
    } catch (e) {
      voiceHost.replaceChildren(h('span', { class: 'small', style: { color: 'var(--blue-ink)' } }, errText(e)));
    }
  }
  loadVoices();
  // A take dictated into the box lands there by itself; nothing else to do.
  cleanups.push(await listen('settings-changed', () => loadVoices()));
  setTimeout(() => area.focus(), 80);
  return () => { audio?.pause(); cleanups.forEach((f) => { try { f(); } catch {} }); };
}
