import { invoke } from '../bridge.js';
import { h, btn, chip, toggle, setRow, toast, relTime, errText } from '../ui.js';
import { refreshSuggestions } from '../app.js';

export async function render(root, { store, arg }) {
  let tab = arg === 'snippets' ? 'snippets' : arg === 'learned' ? 'learned' : 'words';
  let words = [], snippets = [], suggestions = [];
  const packs = await invoke('list_packs').catch(() => []);
  const packName = (src) => packs.find((p) => p.id === src)?.name || src;

  root.append(h('div', { class: 'page-head' },
    h('div', {}, h('div', { class: 'eyebrow' }, 'Your language'), h('h1', { class: 'h1' }, 'Dictionary'),
      h('p', { class: 'lead' }, 'Names, jargon and shortcuts Navi should always get right. Your words also steer the speech engine, so it hears them correctly in the first place.'))));

  const tabs = h('div', { class: 'tabs' });
  const body = h('div', {});
  root.append(tabs, body);

  async function load() {
    [words, snippets, suggestions] = await Promise.all([
      invoke('list_words').catch(() => []), invoke('list_snippets').catch(() => []), invoke('list_suggestions').catch(() => []),
    ]);
    drawTabs(); draw();
  }

  function drawTabs() {
    tabs.innerHTML = '';
    [['words', 'Words', words.length], ['snippets', 'Snippets', snippets.length], ['learned', 'Learned', suggestions.length]].forEach(([k, l, n]) => {
      tabs.append(h('button', { class: tab === k ? 'on' : '', onClick: () => { tab = k; drawTabs(); draw(); } }, l, h('span', { class: 'count' }, String(n))));
    });
  }

  function draw() {
    body.innerHTML = '';
    if (tab === 'words') drawWords();
    if (tab === 'snippets') drawSnippets();
    if (tab === 'learned') drawLearned();
  }

  function drawWords() {
    const term = h('input', { class: 'input', placeholder: 'The right spelling, e.g. Quikly' });
    const alts = h('input', { class: 'input', placeholder: 'What it gets wrong, comma separated (optional): quickly, quick ly' });
    const add = async () => {
      if (!term.value.trim()) return term.focus();
      try {
        await invoke('save_word', { word: { id: '', term: term.value.trim(), soundsLike: alts.value.split(',').map((x) => x.trim()).filter(Boolean), note: '', source: 'manual', createdAt: '', uses: 0 } });
        term.value = ''; alts.value = ''; toast('Added'); load();
      } catch (e) { toast(errText(e), { error: true }); }
    };
    [term, alts].forEach((i) => i.addEventListener('keydown', (e) => { if (e.key === 'Enter') add(); }));
    body.append(h('div', { class: 'card', style: { marginBottom: '14px' } }, h('div', { class: 'add-bar' }, term, alts, btn('Add word', { variant: 'primary', icon: 'plus', onClick: add }))));

    const list = h('div', { class: 'card' });
    if (!words.length) list.append(h('div', { class: 'empty' }, h('div', { class: 'h2' }, 'No words yet'), h('p', {}, 'Add the names of people, products and places you say often.')));
    for (const w of words) {
      const src = w.source === 'manual' ? null : w.source === 'learned' ? chip('Learned', 'accent', true) : chip(packName(w.source), 'line');
      list.append(h('div', { class: 'word-row' },
        h('div', { class: 'term selectable' }, w.term),
        h('div', { class: 'alts' }, w.soundsLike.map((a) => chip(a, 'line')), src),
        h('span', { class: 'small faint', title: 'Times used' }, w.uses ? `${w.uses}×` : ''),
        btn('', { size: 'sm', variant: 'ghost', icon: 'trash', title: 'Remove', onClick: async () => { await invoke('delete_word', { id: w.id }); load(); } })));
    }
    body.append(list);
  }

  function drawSnippets() {
    const trig = h('input', { class: 'input', placeholder: 'Say this, e.g. my calendar link' });
    const exp = h('textarea', { class: 'textarea', style: { minHeight: '42px', height: '42px' }, placeholder: 'Get this: https://cal.example/you' });
    const add = async () => {
      if (!trig.value.trim() || !exp.value.trim()) return;
      try { await invoke('save_snippet', { snippet: { id: '', trigger: trig.value.trim(), expansion: exp.value, source: 'manual' } }); trig.value = ''; exp.value = ''; toast('Snippet added'); load(); }
      catch (e) { toast(errText(e), { error: true }); }
    };
    body.append(h('div', { class: 'card', style: { marginBottom: '14px' } },
      h('p', { class: 'small quiet', style: { marginBottom: '12px' } }, 'Say a short phrase, get a longer block of text: links, addresses, sign-offs, standard replies.'),
      h('div', { class: 'add-bar' }, trig, exp, btn('Add snippet', { variant: 'primary', icon: 'plus', onClick: add }))));
    const list = h('div', { class: 'card' });
    if (!snippets.length) list.append(h('div', { class: 'empty' }, h('p', {}, 'No snippets yet.')));
    for (const s of snippets) {
      list.append(h('div', { class: 'snip' },
        h('div', {}, h('div', { class: 'trig' }, `“${s.trigger}”`), s.source !== 'manual' ? h('div', { style: { marginTop: '6px' } }, chip(packName(s.source), 'line')) : null),
        h('div', { class: 'exp selectable' }, s.expansion),
        btn('', { size: 'sm', variant: 'ghost', icon: 'trash', title: 'Remove', onClick: async () => { await invoke('delete_snippet', { id: s.id }); load(); } })));
    }
    body.append(list);
  }

  function drawLearned() {
    const s = store.settings;
    body.append(h('div', { class: 'card', style: { marginBottom: '14px' } },
      setRow('Learn from my corrections', `After Navi pastes, it looks again ${s.learning.checkAfterSecs} seconds later. If you have fixed a word, it notices. Fix the same word ${s.learning.autoAddAfter} times and it is added to your words automatically. The text is read on this computer only.`,
        toggle(s.learning.enabled, (v) => store.saveSettings((n) => { n.learning.enabled = v; })))));
    const list = h('div', { class: 'card' });
    if (!suggestions.length) list.append(h('div', { class: 'empty' }, h('div', { class: 'h2' }, 'Nothing to review'), h('p', {}, 'Corrections Navi spots will wait here for a yes or no.')));
    for (const g of suggestions) {
      list.append(h('div', { class: 'learn-item', style: { padding: '14px 4px' } },
        h('div', { class: 'pair' }, h('s', {}, g.from), h('span', { class: 'arrow' }, '→'), h('b', { class: 'selectable' }, g.to),
          h('div', { class: 'small faint', style: { marginTop: '2px' } }, `Seen ${g.count}× · last in ${g.appName || 'an app'} ${relTime(g.lastSeen)}`)),
        btn('Add to my words', { size: 'sm', icon: 'check', onClick: async () => { await invoke('accept_suggestion', { id: g.id }); toast(`Added ${g.to}`); load(); refreshSuggestions(); } }),
        btn('Ignore', { size: 'sm', variant: 'ghost', onClick: async () => { await invoke('dismiss_suggestion', { id: g.id }); load(); refreshSuggestions(); } })));
    }
    body.append(list);
  }

  await load();
}
