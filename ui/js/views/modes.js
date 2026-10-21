import { isMac, os } from '../platform.js';
import { invoke, pickFile, listen } from '../bridge.js';
import { h, ico, btn, chip, toggle, select, seg, setRow, field, toast, confirmDialog, errText, keycaps, appName } from '../ui.js';

const ICONS = ['clean', 'pen', 'mail', 'chat', 'terminal', 'notes', 'spark', 'wand'];
const KIND_LABEL = { raw: 'As said', clean: 'Tidy', rewrite: 'Rewrite', prompt: 'Agent prompt', notes: 'Notes' };
// What a mode does, under its name.
const kindLabel = (m) => KIND_LABEL[m.kind] || m.kind;

// True when no AI can run (controller.rs make_llm gives None): none chosen in Settings > AI, or
// Local only blocking one that runs off this computer. A missing key isn't known here.
const noAi = (s) => {
  const provider = s.ai?.provider || 'off';
  return provider === 'off' || (!!s.privacy?.localOnly && ['claude-code', 'codex', 'anthropic', 'openai'].includes(provider));
};
// Whether takes in this mode go through the AI: rambles (Notes) whenever there is one, Raw never,
// Clean's light polish not on the slower subscription tools (pipeline.rs), the rest when switched on.
const usesAi = (m, s) => {
  if (noAi(s)) return false;
  if (m.kind === 'notes') return true;
  if (m.kind === 'clean' && ['claude-code', 'codex'].includes(s.ai?.provider)) return false;
  return !!m.useAi && m.kind !== 'raw';
};
const listNames = (names) => (names.length > 1 ? `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}` : names[0] || '');

export async function render(root, { store, arg, go }) {
  let modes = await invoke('list_modes').catch(() => []);
  const s = store.settings;
  const cleanups = [];

  root.append(h('div', { class: 'page-head' },
    h('div', {}, h('div', { class: 'eyebrow' }, 'How Navi writes'), h('h1', { class: 'h1' }, 'Modes'),
      h('p', { class: 'lead' }, 'A mode decides what happens to your words before they land. Pick one to use now, or leave it on Auto and Navi picks from the app you are in.'))));

  // ------------------------------------------------------------- the mode in use
  const nowRow = h('div', { class: 'mode-now' });
  const autoNote = h('div', {});
  root.append(h('div', { class: 'card', style: { marginBottom: '14px' } },
    h('div', { class: 'card-head' }, h('div', {}, h('div', { class: 'h2' }, 'Using now'), h('p', { class: 'small quiet', style: { marginTop: '3px' } }, 'Click a mode to switch. Mode keys switch from anywhere.')),
      btn('Mode keys', { size: 'sm', variant: 'ghost', icon: 'keyboard', onClick: () => go('settings', 'shortcuts') })),
    nowRow, autoNote));

  const keyFor = (id) => store.settings.hotkeys.modeKeys?.find((k) => k.mode === id && k.keys?.length)?.keys;
  const choose = async (id) => {
    await invoke('set_active_mode', { id });
    store.set({ settings: await invoke('get_settings') });
    drawNow(); draw();
  };

  function drawNow() {
    const active = store.settings.modes.active || '';
    const choices = [{ id: '', name: 'Auto', icon: 'spark' }, ...modes.filter((m) => m.kind !== 'notes')];
    nowRow.replaceChildren(...choices.map((m) => {
      const key = keyFor(m.id || 'auto');
      return h('button', { class: `mode-pill ${active === m.id ? 'on' : ''}`, onClick: () => choose(m.id || 'auto') },
        ico(m.icon || 'spark', 15), m.name, key ? keycaps(key) : null);
    }));
    autoNote.replaceChildren(active ? h('p', { class: 'small faint', style: { marginTop: '12px' } }, 'Every take uses this mode until you switch back to Auto.')
      : h('div', { style: { marginTop: '6px' } },
        setRow('Pick the mode from the app', 'Outlook gets Email, Slack gets Message, your terminal gets Prompt. Turn off to always use the default.',
          toggle(store.settings.modes.autoByApp, (v) => store.saveSettings((n) => { n.modes.autoByApp = v; }))),
        setRow('Default mode', 'Used when no app rule matches.',
          select(modes.filter((m) => m.kind !== 'notes').map((m) => ({ value: m.id, label: m.name })), store.settings.modes.defaultMode, (v) => store.saveSettings((n) => { n.modes.defaultMode = v; }).then(draw)))));
  }

  const aiNote = h('div', {});
  const grid = h('div', { class: 'grid cols-3' });
  const editorHost = h('div', {});
  root.append(aiNote, grid, editorHost);

  // Without an AI the rewrite modes quietly give back the offline tidy (pipeline.rs), so say so here.
  function drawAiNote() {
    const st = store.settings;
    const needs = modes.filter((m) => m.useAi && (m.kind === 'rewrite' || m.kind === 'prompt')).map((m) => m.name);
    if (!noAi(st) || !needs.length) { aiNote.replaceChildren(); return; }
    const blocked = (st.ai?.provider || 'off') !== 'off';
    const why = blocked ? 'Local only is on, so your AI is blocked' : 'No AI is set up';
    const fix = blocked ? 'Turn Local only off, or use Ollama or a server on this computer.'
      : 'Claude Code or Codex use the plan you already pay for, with no API key.';
    aiNote.replaceChildren(h('div', { class: 'card', style: { marginBottom: '14px' } },
      h('div', { class: 'card-head' },
        h('div', {}, h('div', { class: 'h2' }, why),
          h('p', { class: 'small quiet', style: { marginTop: '3px' } }, `${listNames(needs)} tidy your words offline instead of rewriting them, and rambles get rough notes. ${fix}`)),
        btn('Set up AI', { size: 'sm', variant: 'ghost', icon: 'right', onClick: () => go('settings', 'ai') }))));
  }

  const draw = () => {
    drawAiNote();
    grid.innerHTML = '';
    const active = store.settings.modes.active || '';
    for (const m of modes) {
      const isDefault = m.id === store.settings.modes.defaultMode;
      const inUse = m.id === active;
      const key = keyFor(m.id);
      const card = h('div', { class: `card mode-card ${inUse ? 'on' : ''}`, role: 'button', tabindex: '0', onClick: (e) => { if (e.target.closest('.mode-edit')) return; if (m.kind !== 'notes') choose(m.id); } },
        h('span', { class: 'ico' }, ico(m.icon || 'spark', 20)),
        h('span', { class: 'default' }, inUse ? chip('In use', 'accent', true) : isDefault ? chip('Default', 'ink') : null,
          btn('', { variant: 'ghost', size: 'sm', icon: 'pen', title: `Edit ${m.name}`, onClick: () => openEditor(m) })),
        h('div', {}, h('div', { class: 'h2' }, m.name), h('div', { class: 'small faint' }, kindLabel(m) + (usesAi(m, store.settings) ? ' · AI' : ''))),
        h('div', { class: 'desc' }, m.description),
        h('div', { class: 'apps' }, key ? keycaps(key) : null, ...(m.apps || []).slice(0, 3).map((a) => chip(appName(a), 'line')), m.apps?.length > 3 ? chip(`+${m.apps.length - 3}`, 'line') : null,
          m.kind === 'notes' ? chip('Used by ramble', 'line') : null));
      card.querySelector('.default .btn')?.classList.add('mode-edit');
      card.addEventListener('keydown', (e) => { if (e.key === 'Enter' && m.kind !== 'notes') choose(m.id); });
      grid.append(card);
    }
    grid.append(h('button', { class: 'card mode-card add', onClick: () => openEditor(null) }, ico('plus', 22), h('div', { class: 'h3' }, 'New mode'), h('div', { class: 'small faint' }, 'Your own instructions')));
  };

  // A mode key pressed while this page is open moves the highlight too.
  cleanups.push(await listen('settings-changed', async () => { store.set({ settings: await invoke('get_settings') }); drawNow(); draw(); }));
  drawNow();

  function openEditor(mode) {
    const m = mode ? structuredClone(mode) : { id: '', name: 'My mode', icon: 'spark', description: '', kind: 'rewrite', instructions: '', apps: [], useAi: true, builtin: false };
    editorHost.innerHTML = '';
    const name = h('input', { class: 'input', value: m.name });
    const desc = h('input', { class: 'input', value: m.description, placeholder: 'One line about what this mode does' });
    const instr = h('textarea', { class: 'textarea', style: { minHeight: '150px' }, placeholder: 'For example: Write it as a friendly LinkedIn post. Short paragraphs, no hashtags, no emojis.' });
    instr.value = m.instructions || '';
    const appsWrap = h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '6px' } });
    const drawApps = () => {
      appsWrap.innerHTML = '';
      m.apps.forEach((a, i) => { const c = chip(appName(a), 'line'); c.append(h('button', { title: 'Remove', onClick: () => { m.apps.splice(i, 1); drawApps(); }, html: '&times;' })); appsWrap.append(c); });
    };
    drawApps();
    const appInput = h('input', { class: 'input', placeholder: os('Add an app, e.g. notion.exe', 'Add an app, e.g. notion') });
    appInput.addEventListener('keydown', (e) => { if (e.key === 'Enter' && appInput.value.trim()) { let v = appInput.value.trim().toLowerCase(); if (!isMac && !v.endsWith('.exe')) v += '.exe'; m.apps.push(v); appInput.value = ''; drawApps(); } });

    const iconPick = h('div', { class: 'row', style: { gap: '6px', flexWrap: 'wrap' } });
    ICONS.forEach((ic) => {
      const b = h('button', { class: `btn icon sm ${m.icon === ic ? 'primary' : ''}`, onClick: () => { m.icon = ic; iconPick.querySelectorAll('button').forEach((x) => x.classList.remove('primary')); b.classList.add('primary'); } }, ico(ic, 15));
      iconPick.append(b);
    });

    const previewOut = h('div', { class: 'text-body quiet', style: { minHeight: '60px', fontSize: '13px' } }, 'Runs this mode on your last take so you can see the result before saving.');
    const testBtn = btn('Test on my last take', { size: 'sm', icon: 'play', onClick: async () => {
      // The last dictation, held or hands-free (rambles and voice edits are left out).
      const recent = await invoke('list_takes', { query: { limit: 20, offset: 0 } }).catch(() => []);
      const last = (recent || []).find((t) => (t.kind === 'talk' || t.kind === 'handsFree') && t.rawText);
      if (!last) { previewOut.textContent = 'Say something first, then test.'; return; }
      testBtn.disabled = true; previewOut.textContent = 'Running…';
      try { previewOut.textContent = await invoke('preview_mode', { mode: collect(), text: last.rawText }); }
      catch (e) { previewOut.textContent = errText(e); }
      testBtn.disabled = false;
    } });

    const useAi = toggle(m.useAi, (v) => { m.useAi = v; });
    const collect = () => ({ ...m, name: name.value.trim() || 'My mode', description: desc.value.trim(), instructions: instr.value.trim() });

    const saveBtn = btn('Save mode', { variant: 'primary', onClick: async () => {
      try { await invoke('save_mode', { mode: collect() }); toast('Mode saved'); modes = await invoke('list_modes'); draw(); editorHost.innerHTML = ''; }
      catch (e) { toast(errText(e), { error: true }); }
    } });
    const removeBtn = mode ? btn(mode.builtin ? 'Reset to default' : 'Delete', { variant: 'danger', onClick: async () => {
      if (!(await confirmDialog(mode.builtin ? 'Reset this mode?' : 'Delete this mode?', mode.builtin ? 'Its instructions and app list go back to how Navi shipped.' : 'This cannot be undone.', { ok: mode.builtin ? 'Reset' : 'Delete', danger: !mode.builtin }))) return;
      await invoke('delete_mode', { id: mode.id }); modes = await invoke('list_modes'); draw(); editorHost.innerHTML = ''; toast(mode.builtin ? 'Reset' : 'Deleted');
    } }) : null;
    const makeDefault = mode && mode.kind !== 'notes' ? btn('Make default', { variant: 'ghost', onClick: async () => { await store.saveSettings((n) => { n.modes.defaultMode = mode.id; }); draw(); toast(`${mode.name} is now the default`); } }) : null;

    const left = h('div', { class: 'card' },
      h('div', { class: 'card-head' }, h('div', { class: 'h2' }, mode ? `Edit ${mode.name}` : 'New mode'), btn('', { variant: 'ghost', size: 'sm', icon: 'x', onClick: () => { editorHost.innerHTML = ''; } })),
      h('div', { class: 'grid mode-fields' }, field('Name', name), field('Description', desc)),
      h('div', { style: { marginTop: '14px' } }, field('Instructions for the AI', instr)),
      h('div', { style: { marginTop: '14px' } }, field('Icon', iconPick)),
      h('div', { class: 'divider' }),
      setRow('Use AI for this mode', aiHelp(m), m.kind === 'notes' ? null : useAi),
      h('div', { class: 'stack', style: { marginTop: '6px' } }, h('div', { class: 'h3' }, 'Use automatically in'), appsWrap, appInput),
      h('div', { class: 'row', style: { marginTop: '20px', gap: '8px' } }, saveBtn, makeDefault, h('span', { style: { flex: 1 } }), removeBtn));

    const right = h('div', { class: 'stack', style: { gap: '14px' } },
      h('div', { class: 'card' }, h('div', { class: 'eyebrow' }, 'Preview'), h('div', { style: { margin: '10px 0 12px' } }, previewOut), testBtn),
      m.kind === 'prompt' ? projectsCard(store) : null);

    editorHost.append(h('div', { class: 'editor', style: { marginTop: '14px' } }, left, right));
    editorHost.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  draw();
  if (arg) { const m = modes.find((x) => x.id === arg); if (m) openEditor(m); }
  return () => cleanups.forEach((f) => { try { f(); } catch {} });
}

// What the "Use AI for this mode" switch really does for this kind of mode (pipeline.rs; rambles
// are written up in controller.rs process_ramble, which never looks at the switch).
function aiHelp(m) {
  if (m.kind === 'raw') return 'This mode never uses AI: your words go in as said.';
  if (m.kind === 'notes') return 'Rambles are written up by your AI whenever one is set up in Settings > AI, and get rough notes on this computer when not.';
  if (m.kind === 'clean') return 'Off keeps everything on this computer and uses Navi’s own tidy-up rules. On adds a light AI polish, except through Claude Code or Codex, which are too slow for it.';
  return 'Off keeps everything on this computer and uses Navi’s own tidy-up rules.';
}

function projectsCard(store) {
  const card = h('div', { class: 'card' });
  const draw = () => {
    const a = store.settings.agent;
    card.innerHTML = '';
    card.append(h('div', { class: 'eyebrow' }, 'Projects'),
      h('p', { class: 'small quiet', style: { margin: '6px 0 12px' } }, 'Prompt mode looks at the files in your project, so “the header component” becomes @src/components/Header.tsx. It matches the project from the window title, or uses the active one.'));
    if (!a.projects.length) card.append(h('p', { class: 'small faint' }, 'No projects yet.'));
    for (const p of a.projects) {
      card.append(h('div', { class: 'row between', style: { padding: '8px 0', borderTop: '1px solid var(--line-soft)' } },
        h('div', { style: { minWidth: 0 } }, h('div', { class: 'h3' }, p.name, a.activeProject === p.id ? h('span', { class: 'chip accent', style: { marginLeft: '8px' } }, 'Active') : null), h('div', { class: 'small faint', style: { overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' } }, p.path)),
        h('div', { class: 'row', style: { gap: '4px' } },
          a.activeProject !== p.id ? btn('Use', { size: 'sm', variant: 'ghost', onClick: async () => { await store.saveSettings((n) => { n.agent.activeProject = p.id; }); draw(); } }) : null,
          btn('', { size: 'sm', variant: 'ghost', icon: 'x', title: 'Remove', onClick: async () => { await store.saveSettings((n) => { n.agent.projects = n.agent.projects.filter((x) => x.id !== p.id); if (n.agent.activeProject === p.id) n.agent.activeProject = null; }); draw(); } }))));
    }
    card.append(h('div', { class: 'row proj-foot', style: { marginTop: '12px', gap: '8px' } },
      btn('Add a project folder', { size: 'sm', icon: 'folder', onClick: async () => {
        const path = await pickFile({ directory: true, title: 'Choose a project folder' });
        if (!path) return;
        try { const p = await invoke('add_project', { path }); await store.saveSettings((n) => { n.agent.projects.push(p); if (!n.agent.activeProject) n.agent.activeProject = p.id; }); draw(); }
        catch (e) { toast(errText(e), { error: true }); }
      } }),
      seg([{ value: 'at', label: '@path' }, { value: 'plain', label: 'path' }], a.mentionStyle, (v) => store.saveSettings((n) => { n.agent.mentionStyle = v; }))));
  };
  draw();
  return card;
}
