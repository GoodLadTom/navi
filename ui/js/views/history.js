import { invoke, listen, fileSrc, copyText } from '../bridge.js';
import { h, ico, btn, chip, select, relTime, dayLabel, clock, fmtDuration, appInitial, toast, confirmDialog, debounce, errText } from '../ui.js';
import { icon } from '../icons.js';

export async function render(root, { arg, store, go }) {
  let modes = await invoke('list_modes').catch(() => []);
  let apps = await invoke('app_names').catch(() => []);
  const modeName = (id) => modes.find((m) => m.id === id)?.name || id;
  const q = { search: '', kind: '', app: '', mode: '', limit: 200, offset: 0 };
  let takes = [];
  let selected = arg || null;
  let audio = null;

  // What is kept follows Settings > Context and privacy: recordings go after 30 days by default
  // (lib.rs purge_old_audio) and the text stays; with Save history off nothing is kept.
  const privacy = store?.settings?.privacy || {};
  const keepDays = privacy.keepAudioDays ?? 30;
  const kept = privacy.saveHistory === false
    ? 'Save history is off in Settings > Context and privacy, so new takes are not kept.'
    : keepDays
      ? `Everything you have said, kept on this computer. Recordings are deleted after ${keepDays} days and the text stays (Settings > Context and privacy).`
      : 'Everything you have said, kept on this computer with its recording.';
  root.append(h('div', { class: 'page-head' },
    h('div', {}, h('div', { class: 'eyebrow' }, 'Every take, kept'), h('h1', { class: 'h1' }, 'History'),
      h('p', { class: 'lead' }, `${kept} Copy a take again, or re-run its recording in a different mode.`))));

  const searchInput = h('input', { class: 'input', placeholder: 'Search what you said' });
  const appOptions = () => [{ value: '', label: 'All apps' }, ...apps.map((a) => ({ value: a, label: a }))];
  const modeOptions = () => [{ value: '', label: 'All modes' }, ...modes.map((m) => ({ value: m.id, label: m.name }))];
  const appSelect = select(appOptions(), '', (v) => { q.app = v; load(); });
  const modeSelect = select(modeOptions(), '', (v) => { q.mode = v; load(); });
  const toolbar = h('div', { class: 'toolbar' },
    h('div', { class: 'search' }, h('span', { html: icon('search', 16) }), searchInput),
    kindSeg(),
    // The two filters wrap together, never one on its own.
    h('div', { class: 'filters' }, appSelect, modeSelect));
  root.append(toolbar);

  // A take from an app new to History, or a mode made since the page opened, joins the filters.
  // The filter you picked stays picked.
  async function refreshFilters() {
    [apps, modes] = await Promise.all([invoke('app_names').catch(() => apps), invoke('list_modes').catch(() => modes)]);
    const fill = (sel, options, value) => {
      if (value && !options.some((o) => o.value === value)) options.push({ value, label: value });
      sel.replaceChildren(...options.map((o) => h('option', { value: o.value }, o.label)));
      sel.value = value;
    };
    fill(appSelect, appOptions(), q.app);
    fill(modeSelect, modeOptions(), q.mode);
  }

  const listCard = h('div', { class: 'card h-list' });
  const detail = h('div', { class: 'card detail' });
  root.append(h('div', { class: 'history' }, listCard, detail));

  searchInput.addEventListener('input', debounce(() => { q.search = searchInput.value.trim(); load(); }, 220));

  // Kinds as they are saved (types.rs Take.kind). Hands-free takes, and every take the wake word
  // starts, are "handsFree", so they get their own filter rather than hiding under Talk.
  function kindSeg() {
    const wrap = h('div', { class: 'seg' });
    [['', 'All'], ['talk', 'Talk'], ['handsFree', 'Hands-free'], ['ramble', 'Rambles'], ['command', 'Commands']].forEach(([v, l]) => {
      const b = h('button', { class: v === '' ? 'on' : '' }, l);
      b.onclick = () => { wrap.querySelectorAll('button').forEach((x) => x.classList.remove('on')); b.classList.add('on'); q.kind = v; load(); };
      wrap.append(b);
    });
    return wrap;
  }

  // `keepDetail`: a new take arrived, so leave the take on show (and anything playing) alone
  // unless it has gone.
  async function load(keepDetail = false) {
    const query = { ...q, search: q.search || null, kind: q.kind || null, app: q.app || null, mode: q.mode || null };
    takes = await invoke('list_takes', { query }).catch(() => []);
    const before = selected;
    if (!selected || !takes.find((t) => t.id === selected)) selected = takes[0]?.id || null;
    drawList();
    if (!keepDetail || selected !== before) drawDetail();
  }

  function drawList() {
    listCard.innerHTML = '';
    if (!takes.length) {
      listCard.append(h('div', { class: 'empty' }, h('div', { class: 'h2' }, q.search ? 'Nothing matches' : 'No takes yet'), h('p', {}, q.search ? 'Try fewer words.' : 'Your dictations will collect here.')));
      return;
    }
    let lastDay = '';
    for (const t of takes) {
      const day = dayLabel(t.createdAt);
      if (day !== lastDay) { listCard.append(h('div', { class: 'h-day eyebrow' }, day)); lastDay = day; }
      const item = h('button', { class: `h-item ${t.id === selected ? 'on' : ''}`, onClick: () => { selected = t.id; drawList(); drawDetail(); } },
        h('div', { class: 'top' }, h('b', {}, t.appName || 'Unknown app'), h('span', {}, t.kind === 'ramble' ? 'Notes' : t.kind === 'command' ? 'Command' : modeName(t.modeId)), h('span', { class: 't' }, clock(t.createdAt))),
        // A take that failed to transcribe still has its recording, ready to re-run: say why.
        h('div', { class: 'pv' }, t.finalText || t.rawText || (t.status === 'error' ? `Failed: ${t.meta?.error || 'not transcribed'}` : '(nothing heard)')));
      listCard.append(item);
    }
  }

  function drawDetail() {
    if (audio) { audio.pause(); audio = null; }
    detail.innerHTML = '';
    const t = takes.find((x) => x.id === selected);
    if (!t) { detail.append(h('div', { class: 'empty' }, h('p', {}, 'Pick a take to see it here.'))); return; }

    // Delete sits up by the name, so the action buttons below never leave it alone on a line.
    detail.append(h('div', { class: 'd-head' },
      h('div', { class: 'd-who' },
        h('span', { class: 'app-av' }, appInitial(t.appName)),
        h('div', { style: { flex: '1', minWidth: '0' } },
          h('div', { class: 'h2' }, t.appName || 'Unknown app'),
          h('div', { class: 'small faint' }, `${dayLabel(t.createdAt)} at ${clock(t.createdAt)}${t.windowTitle ? ' · ' + t.windowTitle : ''}`)),
        btn('', { size: 'sm', variant: 'ghost', icon: 'trash', title: 'Delete this take', onClick: async () => {
          if (!(await confirmDialog('Delete this take?', 'The text and its recording will be removed from this computer.', { ok: 'Delete', danger: true }))) return;
          await invoke('delete_take', { id: t.id }); toast('Deleted'); selected = null; load();
        } })),
      h('div', { class: 'row', style: { gap: '6px' } },
        btn('Copy', { size: 'sm', icon: 'copy', onClick: async () => { await copyText(t.finalText); toast('Copied'); } }),
        hearButton(t),
        rerunButton(t))));

    if (t.audioPath) detail.append(player(t.audioPath));

    // Text, with tabs for the raw transcript and what changed.
    const tabs = h('div', { class: 'tabs', style: { marginTop: '16px', marginBottom: '14px' } });
    const body = h('div', {});
    const views = t.kind === 'ramble' && t.meta?.notes ? [['notes', 'Notes'], ['final', 'Text'], ['raw', 'Transcript']] : [['final', 'Final'], ['raw', 'What you said'], ['diff', 'Changes']];
    const show = (which) => {
      tabs.querySelectorAll('button').forEach((b) => b.classList.toggle('on', b.dataset.k === which));
      body.innerHTML = '';
      if (which === 'final') body.append(h('div', { class: 'text-body' }, t.finalText || ''));
      if (which === 'raw') body.append(h('div', { class: 'text-body quiet' }, t.rawText || ''));
      if (which === 'diff') body.append(diffView(t.rawText || '', t.finalText || ''));
      if (which === 'notes') body.append(notesView(t.meta.notes));
    };
    views.forEach(([k, l]) => tabs.append(h('button', { dataset: { k }, onClick: () => show(k) }, l)));
    detail.append(tabs, body);
    show(views[0][0]);

    const statusLabel = { pasted: 'Pasted', typed: 'Typed', clipboard: 'On the clipboard', notes: 'Saved as notes', command: 'Voice edit', spoken: 'Read aloud', cancelled: 'Cancelled', error: 'Failed', rerun: 'Re-run from History' };
    detail.append(h('div', { class: 'kv' },
      h('div', {}, h('span', { class: 'eyebrow' }, 'Mode'), h('b', {}, t.kind === 'ramble' ? 'Notes' : modeName(t.modeId))),
      h('div', {}, h('span', { class: 'eyebrow' }, 'Length'), h('b', {}, `${fmtDuration(t.durationMs)} · ${t.words} words`)),
      h('div', {}, h('span', { class: 'eyebrow' }, 'Turnaround'), h('b', {}, `${(t.latencyMs / 1000).toFixed(1)}s`)),
      h('div', {}, h('span', { class: 'eyebrow' }, 'Result'), h('b', {}, statusLabel[t.status] || t.status))));
  }

  // The take read aloud in your ElevenLabs voice (Settings > Voice).
  function hearButton(t) {
    const b = btn('Hear it', { size: 'sm', icon: 'volume', title: 'Hear this take in your ElevenLabs voice' });
    b.onclick = async () => {
      b.disabled = true; b.lastChild.textContent = 'Asking…';
      try {
        const url = await invoke('speak_preview', { text: t.finalText, voiceId: null });
        audio?.pause();
        audio = new Audio(url);
        audio.play().catch(() => {});
        // Navi sends ElevenLabs 5,000 characters at most (speak.rs MAX_CHARS and fit), counted as
        // Rust counts them.
        if (Array.from((t.finalText || '').trim()).length > 5000) toast('Playing the first part only. Navi reads up to 5,000 characters.');
      } catch (e) {
        toast(errText(e), { error: true });
      }
      b.disabled = false; b.lastChild.textContent = 'Hear it';
    };
    return b;
  }

  function rerunButton(t) {
    const wrap = h('span', { class: 'row', style: { gap: '6px' } });
    const b = btn('Re-run', { size: 'sm', icon: 'rerun', disabled: !t.audioPath, title: t.audioPath ? 'Transcribe this recording again' : 'No recording kept for this take' });
    b.onclick = async () => {
      const pick = select(modes.filter((m) => m.kind !== 'notes').map((m) => ({ value: m.id, label: m.name })), t.modeId, null);
      pick.style.height = '30px'; pick.style.width = '130px'; pick.style.borderRadius = '999px';
      const go2 = btn('Run', { size: 'sm', variant: 'primary', onClick: async () => {
        go2.disabled = true; go2.textContent = 'Running…';
        try { const nt = await invoke('rerun_take', { id: t.id, modeId: pick.value }); toast('Re-run done'); selected = nt?.id || t.id; load(); }
        catch (e) { toast(errText(e), { error: true }); go2.disabled = false; go2.textContent = 'Run'; }
      } });
      wrap.replaceChildren(pick, go2);
    };
    wrap.append(b);
    return wrap;
  }

  function player(path) {
    const playBtn = h('button', { class: 'play', html: icon('play', 15) });
    const canvas = h('canvas', {});
    const time = h('span', { class: 'time' }, '0:00');
    const wrap = h('div', { class: 'player' }, playBtn, canvas, time);
    const a = new Audio(fileSrc(path));
    audio = a;
    let peaks = null;
    const fmt = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
    const draw = () => {
      const dpr = window.devicePixelRatio || 1;
      const w = canvas.clientWidth, hh = canvas.clientHeight;
      if (!w) return;
      canvas.width = w * dpr; canvas.height = hh * dpr;
      const ctx = canvas.getContext('2d');
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, hh);
      const n = Math.floor(w / 3.5);
      const pos = a.duration ? a.currentTime / a.duration : 0;
      // Played bars in the accent, the rest in a quiet ink; nothing is coloured before it plays.
      const css = getComputedStyle(canvas);
      const played = css.getPropertyValue('--accent').trim() || '#9B80E0';
      const rest = withAlpha(css.getPropertyValue('--ink').trim() || '#241F33', 0.28);
      for (let i = 0; i < n; i++) {
        const v = peaks ? peaks[Math.floor(i / n * peaks.length)] : 0.15;
        const bh = Math.max(2.5, v * (hh - 4));
        ctx.fillStyle = (i + 1) / n <= pos ? played : rest;
        ctx.fillRect(i * 3.5, (hh - bh) / 2, 2, bh);
      }
    };
    fetch(fileSrc(path)).then((r) => r.arrayBuffer()).then((buf) => new AudioContext().decodeAudioData(buf)).then((ab) => {
      const data = ab.getChannelData(0);
      const buckets = 160, size = Math.floor(data.length / buckets);
      peaks = Array.from({ length: buckets }, (_, b) => { let m = 0; for (let i = b * size; i < (b + 1) * size; i += 16) m = Math.max(m, Math.abs(data[i] || 0)); return m; });
      const max = Math.max(0.05, ...peaks);
      peaks = peaks.map((p) => Math.pow(p / max, 0.7));
      draw();
    }).catch(() => draw());
    a.addEventListener('timeupdate', () => { time.textContent = `${fmt(a.currentTime)} / ${fmt(a.duration || 0)}`; draw(); });
    a.addEventListener('loadedmetadata', () => { time.textContent = `0:00 / ${fmt(a.duration)}`; draw(); });
    a.addEventListener('ended', () => { playBtn.innerHTML = icon('play', 15); });
    playBtn.onclick = () => { if (a.paused) { a.play(); playBtn.innerHTML = icon('pause', 15); } else { a.pause(); playBtn.innerHTML = icon('play', 15); } };
    const seek = (e) => { const r = canvas.getBoundingClientRect(); if (a.duration) a.currentTime = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)) * a.duration; draw(); };
    canvas.addEventListener('pointerdown', (e) => { seek(e); canvas.setPointerCapture(e.pointerId); canvas.onpointermove = seek; });
    canvas.addEventListener('pointerup', () => { canvas.onpointermove = null; });
    requestAnimationFrame(draw);
    return wrap;
  }

  await load();
  // Dictating elsewhere while this page is open adds the new take to the list, keeping your
  // search, filters, the take you picked and its playback.
  const offTake = await listen('take', () => { load(true); refreshFilters(); });
  // A mode renamed, made or removed elsewhere (a pack update): new names in the list and filter.
  const offModes = await listen('modes', async () => { await refreshFilters(); drawList(); });
  return () => { offTake(); offModes(); if (audio) audio.pause(); };
}

// "#241F33" and 0.28 → "rgba(36, 31, 51, 0.28)" (a canvas takes plain colours only).
function withAlpha(hex, alpha) {
  const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!m) return `rgba(36, 31, 51, ${alpha})`;
  return `rgba(${parseInt(m[1], 16)}, ${parseInt(m[2], 16)}, ${parseInt(m[3], 16)}, ${alpha})`;
}

function diffView(a, b) {
  const wa = a.split(/(\s+)/), wb = b.split(/(\s+)/);
  // Word-level LCS, small inputs only.
  const A = wa.filter((x) => x.trim()), B = wb.filter((x) => x.trim());
  if (A.length * B.length > 250000) return h('div', { class: 'text-body' }, b);
  const dp = Array.from({ length: A.length + 1 }, () => new Uint16Array(B.length + 1));
  for (let i = A.length - 1; i >= 0; i--) for (let j = B.length - 1; j >= 0; j--) dp[i][j] = norm(A[i]) === norm(B[j]) ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const out = h('div', { class: 'text-body diff' });
  let i = 0, j = 0;
  while (i < A.length || j < B.length) {
    if (i < A.length && j < B.length && norm(A[i]) === norm(B[j])) { out.append(B[j] + ' '); i++; j++; }
    else if (j < B.length && (i >= A.length || dp[i][j + 1] >= dp[i + 1][j])) { out.append(h('ins', {}, B[j]), ' '); j++; }
    else { out.append(h('del', {}, A[i]), ' '); i++; }
  }
  return out;
}
const norm = (w) => w.toLowerCase().replace(/[^\p{L}\p{N}']/gu, '');

function notesView(n) {
  const wrap = h('div', { class: 'notes selectable' });
  wrap.append(h('div', { class: 'h2' }, n.title || 'Notes'));
  if (n.summary) wrap.append(h('p', { class: 'quiet', style: { marginTop: '6px' } }, n.summary));
  if (n.points?.length) { wrap.append(h('h4', {}, 'Key points')); wrap.append(h('ul', {}, n.points.map((p) => h('li', {}, p)))); }
  if (n.decisions?.length) { wrap.append(h('h4', {}, 'Decisions')); wrap.append(h('ul', {}, n.decisions.map((p) => h('li', {}, p)))); }
  if (n.tasks?.length) {
    wrap.append(h('h4', {}, 'Tasks'));
    n.tasks.forEach((tk) => wrap.append(h('div', { class: 'task' }, h('span', { class: 'box' }), h('div', {}, tk.text, (tk.owner || tk.due) ? h('span', { class: 'faint small' }, `  ${[tk.owner, tk.due].filter(Boolean).join(' · ')}`) : null))));
  }
  return wrap;
}
