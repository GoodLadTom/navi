import { invoke, pickFile, saveFile } from '../bridge.js';
import { h, ico, btn, chip, toast, confirmDialog, promptDialog, relTime, errText } from '../ui.js';

const FEATURED = [
  { file: 'packs/ai-tools.navipack', id: 'ai-tools', name: 'AI tools starter', author: 'GoodLad', description: 'The names of the AI tools people talk about every day, spelt the way they should be.' },
];

export async function render(root, { store }) {
  root.append(h('div', { class: 'page-head' },
    h('div', {}, h('div', { class: 'eyebrow' }, 'Share your language'), h('h1', { class: 'h1' }, 'Packs'),
      h('p', { class: 'lead' }, 'A pack bundles words, snippets and modes into one file. Give one to your team and everyone gets the same names spelt the same way, from their first day.'))));

  const actions = h('div', { class: 'grid cols-3', style: { marginBottom: '14px' } },
    actionCard('upload', 'Export my pack', 'Your words, snippets and custom modes, as a .navipack file.', async () => {
      const meta = await promptDialog('Export my pack', [
        { key: 'name', label: 'Pack name', value: `${store.settings.userName || 'My'} words` },
        { key: 'author', label: 'Author', value: store.settings.userName || '' },
        { key: 'description', label: 'Description', placeholder: 'What is in it and who it is for', multiline: true },
      ], { ok: 'Choose where to save', note: 'Your snippets go in too, so check none of them holds anything private before you share the file.' });
      if (!meta) return;
      const path = await saveFile({ defaultPath: `${(meta.name || 'my-words').replace(/[^\w\- ]+/g, '').trim() || 'my-words'}.navipack`, filters: [{ name: 'Navi pack', extensions: ['navipack'] }] });
      if (!path) return;
      try { await invoke('export_pack', { path, name: meta.name, author: meta.author, description: meta.description }); toast('Pack exported'); }
      catch (e) { toast(errText(e), { error: true }); }
    }),
    actionCard('download', 'Import a file', 'Open a .navipack someone sent you.', async () => {
      const path = await pickFile({ filters: [{ name: 'Navi pack', extensions: ['navipack', 'json'] }] });
      if (!path) return;
      try { const p = await invoke('import_pack_file', { path }); toast(`Installed ${p.name}`); draw(); }
      catch (e) { toast(errText(e), { error: true }); }
    }),
    actionCard('link', 'Install from a link', 'Subscribe to a pack online. Navi keeps it up to date.', async () => {
      const r = await promptDialog('Install from a link', [{ key: 'url', label: 'Pack address (https://…)', placeholder: 'https://example.com/team.navipack' }], { ok: 'Install' });
      if (!r?.url) return;
      try { const p = await invoke('install_pack_url', { url: r.url }); toast(`Installed ${p.name}`); draw(); }
      catch (e) { toast(errText(e), { error: true }); }
    }));
  root.append(actions);

  const installedCard = h('div', { class: 'card', style: { marginBottom: '14px' } });
  const featuredCard = h('div', { class: 'card' });
  root.append(installedCard, featuredCard);

  async function draw() {
    const packs = await invoke('list_packs').catch(() => []);
    installedCard.innerHTML = '';
    installedCard.append(h('div', { class: 'card-head' }, h('div', {}, h('div', { class: 'eyebrow' }, 'Installed'), h('div', { class: 'h2', style: { marginTop: '4px' } }, packs.length ? `${packs.length} pack${packs.length > 1 ? 's' : ''}` : 'No packs yet'))));
    if (!packs.length) installedCard.append(h('p', { class: 'small quiet' }, 'Install the starter pack below, or import one from a teammate.'));
    for (const p of packs) {
      installedCard.append(h('div', { class: 'row between', style: { padding: '14px 0', borderTop: '1px solid var(--line-soft)', alignItems: 'flex-start' } },
        h('div', { class: 'row', style: { gap: '14px', alignItems: 'flex-start' } },
          h('span', { class: 'app-av', style: { background: 'var(--accent-wash)', color: 'var(--accent-ink)', borderColor: 'transparent' } }, ico('packs', 17)),
          h('div', {},
            h('div', { class: 'h3' }, p.name, h('span', { class: 'faint small', style: { fontWeight: 400, marginLeft: '8px' } }, `v${p.version || '1'} · ${p.author || 'Unknown author'}`)),
            p.description ? h('p', { class: 'small quiet', style: { marginTop: '3px', maxWidth: '560px' } }, p.description) : null,
            h('div', { class: 'row', style: { gap: '6px', marginTop: '8px' } },
              chip(`${p.words} words`, 'line'), chip(`${p.snippets} snippets`, 'line'), p.modes ? chip(`${p.modes} modes`, 'line') : null,
              p.sourceUrl ? chip('Kept up to date', 'accent', true) : null,
              h('span', { class: 'small faint' }, `Installed ${relTime(p.installedAt)}`)))),
        btn('Remove', { size: 'sm', variant: 'ghost', onClick: async () => {
          if (!(await confirmDialog(`Remove ${p.name}?`, 'Its words, snippets and modes come out of Navi. Anything you added yourself stays.', { ok: 'Remove', danger: true }))) return;
          await invoke('remove_pack', { id: p.id }); toast('Removed'); draw();
        } })));
    }

    featuredCard.innerHTML = '';
    featuredCard.append(h('div', { class: 'card-head' }, h('div', {}, h('div', { class: 'eyebrow' }, 'Starter packs'), h('div', { class: 'h2', style: { marginTop: '4px' } }, 'Ready to go'))));
    for (const f of FEATURED) {
      const installed = packs.some((p) => p.id === f.id);
      featuredCard.append(h('div', { class: 'row between', style: { padding: '12px 0', borderTop: '1px solid var(--line-soft)' } },
        h('div', {}, h('div', { class: 'h3' }, f.name), h('p', { class: 'small quiet', style: { marginTop: '2px' } }, f.description)),
        installed ? chip('Installed', 'accent', true) : btn('Install', { size: 'sm', variant: 'primary', icon: 'download', onClick: async () => {
          try { const json = await (await fetch(f.file)).text(); await invoke('install_pack_json', { json }); toast(`Installed ${f.name}`); draw(); }
          catch (e) { toast(errText(e), { error: true }); }
        } })));
    }
  }

  await draw();
}

function actionCard(iconName, title, desc, onClick) {
  return h('button', { class: 'card mode-card', style: { minHeight: '0' }, onClick },
    h('span', { class: 'ico' }, ico(iconName, 19)),
    h('div', { class: 'h2' }, title),
    h('div', { class: 'desc' }, desc));
}
