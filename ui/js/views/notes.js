// Notes: calls written up, Granola style. Navi records both sides of a call, writes down what was
// said with each line marked You or Them and, with an AI set up, turns it into notes built on whatever you typed while it listened
// (meeting.rs); without one the note keeps the transcript and your own notes. Along the way it can
// find ideas for Shorts (ideas.rs), do jobs you give it with "Navi, ..." (or "Hey Navi, ...",
// jobs.rs) and keep screenshots (screen.rs). Screen watches (watch.rs) end up here too.
import { invoke, listen, copyText, fileSrc } from '../bridge.js';
import { h, btn, chip, dayLabel, clock, toast, confirmDialog, errText } from '../ui.js';

export async function render(root, { arg, store }) {
  let notes = [];
  let selected = arg || null;
  let live = (await invoke('meeting_status').catch(() => null)) || { recording: false };
  let watch = (await invoke('watch_status').catch(() => null)) || { watching: false };
  let liveAt = Date.now();
  let liveView = null;
  let watchView = null;
  let editing = false;
  let mineTimer = null;
  let mineText = null;
  // What you typed during the call ({ id, text }). It wins over Navi's copy, which only comes back
  // with the next update, so going to another note and back never loses the last few words.
  let typed = null;
  const offs = [];

  const headBtn = h('span', { class: 'row', style: { gap: '8px' } });
  root.append(h('div', { class: 'page-head' },
    h('div', {}, h('div', { class: 'eyebrow' }, 'Calls, written up'), h('h1', { class: 'h1' }, 'Notes'),
      h('p', { class: 'lead' }, 'Navi listens to both sides of a call and writes down what was said, each line marked You or Them. With an AI set up in Settings > AI, it turns that into notes built around whatever you type as you go.')),
    headBtn));

  const listCard = h('div', { class: 'card h-list' });
  const detail = h('div', { class: 'card detail' });
  root.append(h('div', { class: 'history' }, listCard, detail));

  const elapsed = () => (live.elapsedMs || 0) + (live.finishing ? 0 : Date.now() - liveAt);
  const tick = setInterval(() => { if (liveView) liveView.clock.textContent = stamp(elapsed()); }, 1000);

  // While a call is being taken down its own panel has the Stop button, so the head only offers it
  // when another note is open. "Watch my screen" sits beside it.
  function drawHead() {
    const watchBtn = watch.watching
      ? btn(`Stop watching · ${watch.shots || 0}`, { variant: 'ghost', icon: 'check', title: 'Stop and write up what you did', onClick: stopWatch })
      : btn('Watch my screen', { variant: 'ghost', icon: 'play', title: 'Screenshots while you work, written up when you stop', onClick: startWatch });
    let main = null;
    if (!live.recording) main = btn('Take notes now', { variant: 'primary', icon: 'notes', onClick: start });
    else if (selected === live.id) main = null;
    else if (live.finishing) main = btn('Writing up…', { disabled: true });
    else main = btn('Stop and write up', { variant: 'primary', icon: 'check', onClick: stop });
    headBtn.replaceChildren(...[watchBtn, main].filter(Boolean));
  }

  async function start() {
    headBtn.querySelectorAll('button').forEach((b) => { b.disabled = true; });
    try {
      selected = await invoke('start_meeting', { source: null });
      live = (await invoke('meeting_status')) || { recording: false };
      liveAt = Date.now();
      await load();
    } catch (e) {
      toast(errText(e), { error: true });
      drawHead();
    }
  }

  async function stop() {
    await flushMine();
    try { await invoke('stop_meeting'); } catch (e) { toast(errText(e), { error: true }); }
  }

  async function startWatch() {
    try { await invoke('start_watch'); } catch (e) { toast(errText(e), { error: true }); }
    watch = (await invoke('watch_status').catch(() => null)) || watch;
    drawHead();
  }

  async function stopWatch() {
    try { await invoke('stop_watch'); toast('Writing up what you did'); } catch (e) { toast(errText(e), { error: true }); }
    watch = (await invoke('watch_status').catch(() => null)) || { watching: false };
    drawHead();
  }

  // What you type while Navi listens is sent a moment after you pause.
  function queueMine(text) {
    typed = { id: live.id, text };
    live.myNotes = text;
    mineText = text;
    clearTimeout(mineTimer);
    mineTimer = setTimeout(flushMine, 400);
  }
  async function flushMine() {
    clearTimeout(mineTimer);
    if (mineText == null) return;
    const text = mineText;
    mineText = null;
    await invoke('set_my_notes', { text }).catch(() => {});
  }

  // Something half typed in the open note (a title, or Markdown being edited) that redrawing it
  // would throw away.
  const typingInDetail = () => {
    const el = document.activeElement;
    return editing || (!!el && detail.contains(el) && el.matches('input, textarea') && !el.readOnly);
  };

  // `redrawDetail` false refreshes the list and the buttons only, leaving the open note as it is.
  async function load(redrawDetail = true) {
    notes = await invoke('list_notes').catch(() => []);
    if (live.recording && !notes.some((n) => n.id === live.id)) {
      notes.unshift({ id: live.id, title: live.title, app: live.app, createdAt: new Date(Date.now() - elapsed()).toISOString(), durationMs: 0, status: 'recording', notesMd: '' });
    }
    const was = selected;
    if (!selected || !notes.some((n) => n.id === selected)) selected = live.recording ? live.id : notes[0]?.id || null;
    drawHead();
    drawList();
    if (redrawDetail || selected !== was) drawDetail();
  }

  function drawList() {
    listCard.innerHTML = '';
    if (!notes.length) {
      listCard.append(h('div', { class: 'empty' },
        h('div', { class: 'h2' }, 'No notes yet'),
        h('p', {}, 'When a call starts in Teams, Zoom, Google Meet, Slack or Discord, Navi offers to take notes. Or press Take notes now.')));
      return;
    }
    let lastDay = '';
    for (const n of notes) {
      const day = dayLabel(n.createdAt);
      if (day !== lastDay) { listCard.append(h('div', { class: 'h-day eyebrow' }, day)); lastDay = day; }
      const isLive = live.recording && n.id === live.id;
      const status = isLive ? chip(live.finishing ? 'Writing up' : 'Live', 'blue', true)
        : n.status === 'done' ? null
        : isWatching(n) ? chip('Watching', 'blue', true)
        : chip('Writing up', 'accent', true);
      const ideas = isLive ? (live.ideas || []).length : n.counts?.ideas || 0;
      const meta = [n.app, isLive || !n.durationMs ? null : length(n.durationMs), ideas ? `${ideas} Short idea${ideas === 1 ? '' : 's'}` : null].filter(Boolean).join(' · ');
      const summary = summaryOf(n.notesMd);
      listCard.append(h('button', { class: `h-item ${n.id === selected ? 'on' : ''}`, onClick: () => { if (n.id === selected) return; selected = n.id; editing = false; drawList(); drawDetail(); } },
        h('div', { class: 'top' }, h('b', { class: 'n-title' }, n.title || 'Notes'), h('span', { class: 't' }, clock(n.createdAt))),
        h('div', { class: 'n-meta' }, status, meta ? h('span', {}, meta) : null),
        summary ? h('div', { class: 'pv' }, summary) : null));
    }
  }

  // The note of the screen watch going on now (it is saved as it goes).
  const isWatching = (n) => n.status === 'watching' && watch.watching && watch.id === n.id;
  const shotsSoFar = () => `${watch.shots || 0} screenshot${watch.shots === 1 ? '' : 's'} so far. Stop to have them written up.`;

  function drawDetail() {
    liveView = null;
    watchView = null;
    drawHead();
    const n = notes.find((x) => x.id === selected);
    if (!n) {
      detail.replaceChildren(h('div', { class: 'empty' }, h('p', {}, notes.length ? 'Pick a note to see it here.' : 'Your notes will appear here.')));
      return;
    }
    if (live.recording && n.id === live.id) drawLive();
    else drawNote(n.id);
  }

  // The title as an inline field, Granola style: click, type, then Enter or click away to keep it.
  function titleInput(value, save, readOnly = false) {
    const el = h('input', { class: 'n-title-in', placeholder: 'Untitled', spellcheck: 'true', title: readOnly ? null : 'Click to rename' });
    el.value = value || '';
    el.readOnly = readOnly;
    let kept = el.value;
    el.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') el.blur();
      if (e.key === 'Escape') { el.value = kept; el.blur(); }
    });
    el.addEventListener('blur', async () => {
      const t = el.value.trim();
      if (!t || t === kept) { el.value = kept; return; }
      try { await save(t); kept = t; } catch (e) { el.value = kept; toast(errText(e), { error: true }); }
    });
    return el;
  }

  // ------------------------------------------------------------ the meeting in progress
  function drawLive() {
    const clockEl = h('span', { class: 'n-clock' }, stamp(elapsed()));
    const stopBtn = live.finishing ? btn('Writing up…', { size: 'sm', disabled: true }) : btn('Stop and write up', { size: 'sm', variant: 'primary', icon: 'check', onClick: stop });
    const title = titleInput(live.title, async (t) => {
      await invoke('set_meeting_title', { title: t });
      live.title = t;
      drawList();
    }, !!live.finishing);
    const mine = h('textarea', { class: 'textarea n-mine', placeholder: 'Type anything you want to remember, in any order. Navi builds the notes around it.' });
    mine.value = typed?.id === live.id ? typed.text : live.myNotes || '';
    mine.readOnly = !!live.finishing;
    mine.addEventListener('input', () => queueMine(mine.value));
    const lines = h('div', { class: 'transcript live selectable' });
    const extras = h('div', {});
    const sides = h('div', { class: 'faint' });
    // A problem with the recording, such as a microphone unplugged mid-call, and what to do.
    const warn = h('div', { style: { color: 'var(--blue-ink)', marginTop: '2px' } });
    detail.replaceChildren(
      title,
      h('div', { class: 'n-live-head' },
        h('div', { class: 'row', style: { gap: '10px', minWidth: 0 } },
          h('span', { class: 'rec-dot' }),
          h('div', { class: 'small', style: { minWidth: 0 } },
            h('b', {}, live.finishing ? 'Finishing' : 'Taking notes'), clockEl,
            sides, warn)),
        stopBtn),
      h('div', { class: 'eyebrow n-label' }, 'Your notes'),
      mine,
      extras,
      h('div', { class: 'eyebrow n-label' }, 'Live transcript'),
      lines,
      h('p', { class: 'small faint n-consent' }, 'Let the people on the call know Navi is taking notes.'));
    liveView = { clock: clockEl, lines, extras, sides, warn };
    fillLive();
  }

  function fillLive() {
    if (!liveView) return;
    liveView.sides.textContent = [live.app || null, live.systemAudio ? 'Both sides of the call' : 'Your microphone only'].filter(Boolean).join(' · ');
    liveView.warn.textContent = live.warningDetail || live.warning || '';
    liveView.warn.style.display = live.warning ? '' : 'none';
    fillExtras();
    const box = liveView.lines;
    const all = live.lines || [];
    if (!all.length) {
      box.replaceChildren(h('p', { class: 'faint small' }, 'Listening. The first lines appear about 20 seconds in.'));
      return;
    }
    const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 30;
    box.replaceChildren(...all.map((l, i) => lineRow(l, all[i - 1])));
    if (atBottom) box.scrollTop = box.scrollHeight;
  }

  // Jobs, Short ideas and screenshots as they come in.
  function fillExtras() {
    const box = liveView?.extras;
    if (!box) return;
    const parts = [];
    const jobs = live.jobs || [];
    if (jobs.length) parts.push(h('div', { class: 'eyebrow n-label' }, 'Jobs'), jobList(jobs));
    const ideas = live.ideas || [];
    const ideasOn = !!store?.settings?.notes?.shortIdeas;
    if (ideas.length || ideasOn) {
      parts.push(h('div', { class: 'eyebrow n-label' }, 'Short ideas'));
      parts.push(ideas.length ? ideaList(ideas) : h('p', { class: 'small faint' }, 'Your AI looks through the call every few minutes. Ideas for Shorts appear here as they turn up.'));
    }
    const shots = live.shots || [];
    if (shots.length) parts.push(h('div', { class: 'eyebrow n-label' }, `Screenshots · ${shots.length}`), shotStrip(shots.slice(-6)));
    box.replaceChildren(...parts);
  }

  // ------------------------------------------------------------ a finished note
  async function drawNote(id) {
    const n = await invoke('get_note', { id }).catch(() => null);
    if (selected !== id) return;
    if (!n) { detail.replaceChildren(h('div', { class: 'empty' }, h('p', {}, 'This note has gone.'))); return; }
    const isWatch = n.app === 'Watch';
    const meta = [`${dayLabel(n.createdAt)} at ${clock(n.createdAt)}`, n.durationMs ? length(n.durationMs) : null, n.app || null].filter(Boolean).join(' · ');
    const done = n.status === 'done';
    const title = titleInput(n.title, async (t) => {
      const saved = await invoke('update_note', { id: n.id, title: t, notesMd: null });
      if (saved) Object.assign(n, saved);
      load();
    }, !done);
    const head = h('div', { class: 'n-doc-head' }, title, h('div', { class: 'small faint n-meta-line' }, meta));
    if (isWatching(n)) {
      const count = h('p', { class: 'small quiet' }, shotsSoFar());
      detail.replaceChildren(head,
        h('div', { class: 'n-wait' }, h('span', { class: 'rec-dot' }), h('div', {}, h('b', {}, 'Watching your screen'), count)),
        h('div', { class: 'row' }, btn('Stop and write up', { size: 'sm', variant: 'primary', icon: 'check', onClick: stopWatch })));
      watchView = { count };
      return;
    }
    if (!done) {
      // Written by the AI for longer jobs (Settings > AI). Without one, Navi keeps the transcript
      // and your notes (meeting.rs offline_notes), or lists a watch's screenshots by time.
      const waitNote = isWatch ? 'Your AI is writing it up. Without one, Navi lists the screenshots by time.'
        : 'Your AI is writing them up. Without one, Navi keeps the transcript and your own notes.';
      detail.replaceChildren(head, h('div', { class: 'n-wait' }, h('span', { class: 'n-spin' }),
        h('div', {}, h('b', {}, isWatch ? 'Writing up what you did' : 'Writing up the notes'), h('p', { class: 'small quiet' }, waitNote))));
      return;
    }
    const actionsTo = store?.settings?.notes?.actionsTo || 'none';
    head.append(h('div', { class: 'n-actions' }, ...[
      btn('Copy', { size: 'sm', icon: 'copy', title: 'Copy the notes', onClick: async () => { await copyText(n.notesMd); toast('Copied'); } }),
      isWatch ? null : rewriteButton(n),
      !isWatch && n.transcript.length && !n.ideas.length ? ideasButton(n, 'Find Short ideas') : null,
      !isWatch && actionsTo !== 'none' && hasActions(n.notesMd) ? sendButton(n, actionsTo) : null,
      btn('', { size: 'sm', variant: 'ghost', icon: 'trash', title: 'Delete this note', onClick: async () => {
        // A watch's Markdown copy links to the screenshots in Navi's data folder, which go too.
        const gone = isWatch ? 'It is removed from Navi, with its screenshots. The Markdown copy in your notes folder stays, but without its pictures.'
          : 'It is removed from Navi, with its screenshots. The Markdown copy in your notes folder stays.';
        if (!(await confirmDialog('Delete this note?', gone, { ok: 'Delete', danger: true }))) return;
        try { await invoke('delete_note', { id: n.id }); toast('Deleted'); selected = null; load(); } catch (e) { toast(errText(e), { error: true }); }
      } }),
    ].filter(Boolean)));

    const tabs = h('div', { class: 'tabs', style: { marginTop: '16px' } });
    const body = h('div', {});
    const views = [['notes', isWatch ? 'What happened' : 'Notes']];
    if (n.ideas.length) views.push(['ideas', 'Short ideas', n.ideas.length]);
    if (n.jobs.length) views.push(['jobs', 'Jobs', n.jobs.length]);
    // A watch shows its pictures in the write-up itself; one whose notes don't have them gets the tab.
    if (n.shots.length && (!isWatch || !/!\[[^\]]*\]\(/.test(n.notesMd))) views.push(['shots', 'Screenshots', n.shots.length]);
    if (!isWatch) views.push(['mine', 'Your notes'], ['transcript', 'Transcript', n.transcript.length]);
    const show = (which) => {
      tabs.querySelectorAll('button').forEach((b) => b.classList.toggle('on', b.dataset.k === which));
      if (which === 'notes') body.replaceChildren(notesBody(n));
      if (which === 'ideas') body.replaceChildren(ideasBody(n));
      if (which === 'jobs') body.replaceChildren(jobList(n.jobs));
      if (which === 'shots') body.replaceChildren(shotGrid(n.shots));
      if (which === 'mine') body.replaceChildren(n.myNotes.trim() ? h('div', { class: 'text-body' }, n.myNotes) : h('p', { class: 'faint' }, n.app === 'Ramble' ? 'A ramble is all talk: nothing typed alongside it.' : 'You didn’t type anything during this call.'));
      if (which === 'transcript') body.replaceChildren(transcriptView(n.transcript));
    };
    views.forEach(([k, label, count]) => tabs.append(h('button', { dataset: { k }, onClick: (e) => { show(k); e.currentTarget.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'smooth' }); } }, label, count != null ? h('span', { class: 'count' }, String(count)) : null)));
    tabs.classList.add('n-tabs');
    const edge = () => tabs.classList.toggle('more', tabs.scrollLeft + tabs.clientWidth < tabs.scrollWidth - 4);
    tabs.addEventListener('scroll', edge, { passive: true });
    requestAnimationFrame(edge);
    // Everything is read here in Navi; the Markdown copy is only a backup in the notes folder.
    const file = n.filePath ? h('p', { class: 'small faint n-file' }, 'A Markdown copy is kept in your notes folder. ',
      h('button', { onClick: () => invoke('reveal_note_file', { id: n.id }).catch((e) => toast(errText(e), { error: true })) }, 'Show it')) : null;
    detail.replaceChildren(...[head, tabs, body, file].filter(Boolean));
    show('notes');
  }

  // The notes, with Edit to change the Markdown (its first heading is the title).
  function notesBody(n) {
    const wrap = h('div', {});
    const draw = () => {
      if (!editing) {
        wrap.replaceChildren(h('div', { class: 'n-tools' }, btn('Edit', { size: 'sm', variant: 'ghost', icon: 'pen', onClick: () => { editing = true; draw(); } })), markdown(n.notesMd, n.title));
        return;
      }
      const ta = h('textarea', { class: 'textarea n-edit', spellcheck: 'true' });
      ta.value = n.notesMd;
      wrap.replaceChildren(ta, h('div', { class: 'row', style: { gap: '8px', marginTop: '10px', justifyContent: 'flex-end' } },
        btn('Cancel', { size: 'sm', variant: 'ghost', onClick: () => { editing = false; draw(); } }),
        btn('Save', { size: 'sm', variant: 'primary', onClick: async () => {
          try {
            const saved = await invoke('update_note', { id: n.id, title: null, notesMd: ta.value });
            editing = false;
            toast('Saved');
            if (saved) Object.assign(n, saved);
            load();
          } catch (e) { toast(errText(e), { error: true }); }
        } })));
      setTimeout(() => ta.focus(), 30);
    };
    draw();
    return wrap;
  }

  function ideasBody(n) {
    return h('div', {},
      h('div', { class: 'n-tools' },
        btn('Copy all', { size: 'sm', variant: 'ghost', icon: 'copy', onClick: async () => { await copyText(n.ideas.map(ideaText).join('\n\n')); toast('Copied'); } }),
        ideasButton(n, 'Look again')),
      ideaList(n.ideas));
  }

  function rewriteButton(n) {
    const b = btn('Write again', { size: 'sm', icon: 'rerun', title: 'Write the notes again with your AI', disabled: !n.transcript.length });
    b.onclick = async () => {
      b.disabled = true;
      b.lastChild.textContent = 'Writing…';
      try {
        await invoke('rewrite_note', { id: n.id });
        toast('Written again');
      } catch (e) {
        toast(errText(e), { error: true });
        load();
      }
    };
    return b;
  }

  // Reads the whole call again for ideas (also on notes taken before Short ideas were on).
  function ideasButton(n, label) {
    const b = btn(label, { size: 'sm', variant: label === 'Look again' ? 'ghost' : '', icon: 'spark', title: 'Have your AI look through the whole call for Shorts' });
    b.onclick = async () => {
      b.disabled = true;
      b.lastChild.textContent = 'Looking…';
      try {
        const saved = await invoke('find_short_ideas', { id: n.id });
        toast(`${saved?.ideas?.length || 0} Short ideas`);
      } catch (e) {
        toast(errText(e), { error: true });
        b.disabled = false;
        b.lastChild.textContent = label;
      }
    };
    return b;
  }

  function sendButton(n, to) {
    const name = to === 'notion' ? 'Notion' : 'ClickUp';
    const b = btn(`Send actions to ${name}`, { size: 'sm', icon: 'external' });
    b.onclick = async () => {
      b.disabled = true;
      try { toast(await invoke('send_note_actions', { id: n.id })); } catch (e) { toast(errText(e), { error: true }); }
      b.disabled = false;
    };
    return b;
  }

  offs.push(await listen('meeting', (m) => {
    const was = live;
    live = m || { recording: false };
    liveAt = Date.now();
    if (typed && typed.id === live.id) live.myNotes = typed.text;
    if (was.recording !== live.recording || was.id !== live.id || was.finishing !== live.finishing) {
      // The call ended by itself: send what you were typing before the panel is redrawn.
      flushMine();
      // The call's own panel is redrawn; another note you are typing in is left alone.
      const callOpen = !selected || selected === was.id || selected === live.id;
      load(callOpen || !typingInDetail());
    } else if (liveView) {
      fillLive();
      if (was.title !== live.title || (was.ideas || []).length !== (live.ideas || []).length) drawList();
    }
  }));
  offs.push(await listen('note', (n) => {
    // News of another note only refreshes the list, and the open note is never redrawn over
    // something half typed in it.
    load(n?.id === selected && !typingInDetail());
  }));
  offs.push(await listen('watch', (w) => {
    watch = w || { watching: false };
    drawHead();
    if (watchView) watchView.count.textContent = shotsSoFar();
  }));

  await load();
  return () => {
    clearInterval(tick);
    flushMine();
    offs.forEach((off) => { try { off?.(); } catch {} });
  };
}

// ------------------------------------------------------------ pieces

function transcriptView(lines) {
  if (!lines?.length) return h('p', { class: 'faint' }, 'Nothing was heard.');
  return h('div', { class: 'transcript selectable' }, lines.map((l, i) => lineRow(l, lines[i - 1])));
}

// One line of a transcript. A line from the same person soon after the last one leaves out the name.
function lineRow(l, prev) {
  const same = prev && prev.who === l.who && l.atMs - prev.atMs < 30000;
  return h('div', { class: `t-line ${l.who === 'you' ? 'you' : 'them'}${same ? ' cont' : ''}` },
    h('span', { class: 't-time' }, stamp(l.atMs)),
    h('span', { class: 't-who' }, same ? '' : l.who === 'you' ? 'You' : 'Them'),
    h('span', { class: 't-text' }, l.text));
}

// An idea for a Short: what it teaches, the line it opens on, and the exact lines to cut.
function ideaList(ideas) {
  return h('div', { class: 'ideas' }, ideas.map((idea, i) => h('div', { class: 'idea' },
    h('div', { class: 'idea-top' },
      h('span', { class: 'idea-n' }, String(i + 1)),
      h('b', {}, idea.title),
      h('button', { class: 'idea-copy', title: 'Copy this idea', onClick: async () => { await copyText(ideaText(idea)); toast('Copied'); } }, 'Copy')),
    idea.lesson ? h('p', { class: 'idea-lesson' }, idea.lesson) : null,
    idea.hook ? h('p', { class: 'idea-hook' }, h('span', {}, 'Opens on'), ` “${idea.hook}”`) : null,
    h('div', { class: 'idea-lines selectable' }, idea.quotes.map((q) => h('div', { class: 'idea-line' }, h('span', { class: 't-time' }, stamp(q.atMs)), h('span', {}, `“${q.text}”`)))),
    idea.why ? h('p', { class: 'idea-why small quiet' }, idea.why) : null,
    idea.seconds || idea.flags?.length ? h('div', { class: 'idea-flags' }, idea.seconds ? h('span', { class: 'idea-len' }, `About ${idea.seconds} s`) : null, (idea.flags || []).map((f) => chip(f, 'blue'))) : null)));
}

function ideaText(idea) {
  return [
    idea.title + (idea.seconds ? ` (about ${idea.seconds} s)` : ''),
    idea.lesson,
    idea.hook ? `Opens on: “${idea.hook}”` : '',
    ...idea.quotes.map((q) => `[${stamp(q.atMs)}] “${q.text}”`),
    idea.why ? `Why: ${idea.why}` : '',
    ...(idea.flags || []).map((f) => `Check: ${f}`),
  ].filter(Boolean).join('\n');
}

// Jobs given with "Navi, ..." (or "Hey Navi, ...") on the call, and what the AI made of them.
function jobList(jobs) {
  return h('div', { class: 'jobs' }, jobs.map((job) => {
    const state = job.status === 'working'
      ? h('span', { class: 'job-state' }, h('span', { class: 'n-spin small' }), 'Working on it')
      : job.status === 'failed' ? chip('Couldn’t do it', 'blue') : null;
    return h('div', { class: 'job' },
      h('div', { class: 'job-top' },
        h('span', { class: 't-time' }, stamp(job.atMs)),
        h('b', {}, `“${job.ask}”`),
        state,
        job.status === 'done' ? h('button', { class: 'idea-copy', title: 'Copy the answer', onClick: async () => { await copyText(job.resultMd); toast('Copied'); } }, 'Copy') : null),
      job.status === 'done' ? h('div', { class: 'job-body' }, markdown(job.resultMd)) : null,
      job.status === 'failed' && job.error ? h('p', { class: 'small quiet' }, job.error) : null);
  }));
}

function shotStrip(shots) {
  return h('div', { class: 'shot-strip' }, shots.map((s) => h('img', { src: fileSrc(s.path), alt: s.window || 'Screenshot', title: `${stamp(s.atMs)} ${s.window || ''}`, loading: 'lazy', onClick: () => viewShot(s.path, shotLabel(s)) })));
}

function shotGrid(shots) {
  return h('div', { class: 'shot-grid' }, shots.map((s) => h('figure', {},
    h('img', { src: fileSrc(s.path), alt: s.caption || s.window || 'Screenshot', loading: 'lazy', title: 'See it full size', onClick: () => viewShot(s.path, shotLabel(s)) }),
    h('figcaption', {}, h('span', { class: 't-time' }, stamp(s.atMs)), s.caption || [s.app, s.window].filter(Boolean).join(' · ')))));
}

function shotLabel(s) {
  return `${stamp(s.atMs)}  ${s.caption || [s.app, s.window].filter(Boolean).join(' · ')}`;
}

// A screenshot full size inside Navi. Esc, the cross or a click outside closes it. (Clicking used
// to ask Windows to open the file, which Navi isn't allowed to do, so nothing happened.)
function viewShot(path, caption) {
  const onKey = (e) => { if (e.key === 'Escape') { e.stopPropagation(); close(); } };
  const close = () => { box.remove(); document.removeEventListener('keydown', onKey, true); };
  const box = h('div', { class: 'shot-view', role: 'dialog', 'aria-label': caption || 'Screenshot', onClick: (e) => { if (e.target === box) close(); } },
    h('figure', {},
      h('img', { src: fileSrc(path), alt: caption || 'Screenshot' }),
      h('figcaption', {},
        h('span', {}, caption || ''),
        h('div', { class: 'row', style: { gap: '6px', flex: 'none' } },
          btn('Show in folder', { size: 'sm', icon: 'folder', onClick: () => invoke('reveal_file', { path }).catch((e) => toast(errText(e), { error: true })) }),
          btn('', { size: 'sm', variant: 'ghost', icon: 'x', title: 'Close (Esc)', onClick: close })))));
  document.body.append(box);
  document.addEventListener('keydown', onKey, true);
}

// The same headings, at any level, as meeting.rs actions_from (which sends the actions).
function hasActions(md) {
  return /^[ \t]*#+[ \t]*\**(actions|action items|action points|next steps|to do|to-do)\**[ \t]*$/im.test(String(md || ''));
}

function stamp(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const mm = String(Math.floor(s / 60) % 60).padStart(2, '0');
  const ss = String(s % 60).padStart(2, '0');
  return s >= 3600 ? `${Math.floor(s / 3600)}:${mm}:${ss}` : `${mm}:${ss}`;
}

function length(ms) {
  const m = Math.round(ms / 60000);
  if (ms < 60000) return `${Math.max(1, Math.round(ms / 1000))} s`;
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')} min`;
}

// The summary's first lines, for the list.
function summaryOf(md) {
  const lines = String(md || '').split('\n');
  const at = lines.findIndex((l) => /^##\s+summary/i.test(l.trim()));
  if (at < 0) return '';
  const out = [];
  for (const raw of lines.slice(at + 1)) {
    const t = raw.trim();
    if (t.startsWith('#')) break;
    if (t) out.push(t.replace(/^[-*+]\s+/, '').replace(/[*_`]/g, ''));
    if (out.join(' ').length > 180) break;
  }
  return out.join(' ');
}

// A quote: set in from a rule on the left, in the quieter ink.
const QUOTE_STYLE = { margin: '8px 0', padding: '2px 0 2px 12px', borderLeft: '3px solid var(--rule-strong)', color: 'var(--ink-quiet)' };

// A small, safe Markdown renderer for notes: headings, bullets, numbered lists, tasks, quotes,
// pictures (screenshots Navi keeps), bold, italics and inline code. It builds elements and never
// inserts HTML. The first heading is left out when it is the title already shown above.
function markdown(md, title) {
  const wrap = h('div', { class: 'notes md selectable' });
  let list = null;
  let para = [];
  let quote = [];
  let titleSkipped = false;
  const flushPara = () => { if (para.length) { wrap.append(h('p', {}, inline(para.join(' ')))); para = []; } };
  const flushQuote = () => { if (quote.length) { wrap.append(h('blockquote', { class: 'md-quote', style: QUOTE_STYLE }, inline(quote.join(' ')))); quote = []; } };
  const flush = () => { flushPara(); flushQuote(); };
  for (const raw of String(md || '').split('\n')) {
    const t = raw.trim();
    let m;
    // "> " lines are a quote (older notes said why they weren't written up that way), shown
    // without the marker. Lines in a row make one quote; a bare ">" ends it.
    if ((m = /^>\s?(.*)$/.exec(t))) {
      flushPara();
      list = null;
      if (m[1].trim()) quote.push(m[1].trim());
      else flushQuote();
      continue;
    }
    flushQuote();
    if (!t) { flush(); list = null; continue; }
    if ((m = /^!\[([^\]]*)\]\((?:<([^>]+)>|([^)\s]+))\)$/.exec(t))) {
      flush(); list = null;
      const path = m[2] || m[3];
      wrap.append(h('figure', { class: 'md-shot' }, h('img', { src: fileSrc(path), alt: m[1], loading: 'lazy', title: 'See it full size', onClick: () => viewShot(path, m[1]) })));
      continue;
    }
    if ((m = /^(#{1,6})\s+(.*)$/.exec(t))) {
      flush(); list = null;
      const text = m[2].replace(/^\*\*(.*)\*\*$/, '$1').trim();
      if (m[1].length === 1 && !titleSkipped && (!title || text === title.trim())) { titleSkipped = true; continue; }
      wrap.append(h(m[1].length <= 2 ? 'h4' : 'h5', {}, inline(text)));
      continue;
    }
    if ((m = /^[-*+]\s+\[( |x|X)\]\s+(.*)$/.exec(t))) {
      flush(); list = null;
      wrap.append(h('div', { class: 'task' }, h('span', { class: `box${m[1] === ' ' ? '' : ' on'}` }), h('div', {}, inline(m[2]))));
      continue;
    }
    if ((m = /^[-*+•]\s+(.*)$/.exec(t))) {
      flush();
      if (!list || list.tagName !== 'UL') { list = h('ul', {}); wrap.append(list); }
      list.append(h('li', { class: /^\s{2,}/.test(raw) ? 'sub' : '' }, inline(m[1])));
      continue;
    }
    if ((m = /^(\d+)[.)]\s+(.*)$/.exec(t))) {
      flush();
      if (!list || list.tagName !== 'OL') { list = h('ol', { start: m[1] === '1' ? null : m[1] }); wrap.append(list); }
      list.append(h('li', {}, inline(m[2])));
      continue;
    }
    list = null;
    para.push(t);
  }
  flush();
  return wrap;
}

function inline(text) {
  const out = [];
  const re = /(\*\*[^*]+\*\*|`[^`]+`|(?<![\w*])\*[^*\s][^*]*\*(?![\w*])|(?<!\w)_[^_\s][^_]*_(?!\w))/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const tok = m[0];
    if (tok.startsWith('**')) out.push(h('strong', {}, tok.slice(2, -2)));
    else if (tok.startsWith('`')) out.push(h('code', {}, tok.slice(1, -1)));
    else out.push(h('em', {}, tok.slice(1, -1)));
    last = m.index + tok.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}
