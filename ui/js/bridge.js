// The one place the interface talks to Rust. In a plain browser (design preview) it falls back to
// mock.js, which answers the same commands with sample data, so every screen can be built and
// reviewed without the app running.

const T = window.__TAURI__;
export const isTauri = !!(T && T.core);

let mockMod = null;
async function mock() {
  if (!mockMod) mockMod = await import('./mock.js');
  return mockMod;
}

export async function invoke(cmd, args = {}) {
  if (isTauri) return T.core.invoke(cmd, args);
  return (await mock()).invoke(cmd, args);
}

export async function listen(event, cb) {
  if (isTauri) return T.event.listen(event, (e) => cb(e.payload));
  return (await mock()).listen(event, cb);
}

export function fileSrc(path) {
  if (!path) return '';
  return isTauri ? T.core.convertFileSrc(path) : path;
}

export const win = {
  current() { return isTauri ? T.window.getCurrentWindow() : null; },
  minimize() { this.current()?.minimize(); },
  toggleMaximize() { this.current()?.toggleMaximize(); },
  hide() { this.current()?.hide(); },
  startDragging() { this.current()?.startDragging(); },
};

export async function pickFile(opts = {}) {
  if (isTauri && T.dialog) {
    return T.dialog.open({ multiple: false, directory: false, ...opts });
  }
  return opts.directory ? 'C:\\Users\\you\\Projects\\navi' : 'C:\\Users\\you\\Desktop\\sample.navipack';
}

export async function saveFile(opts = {}) {
  if (isTauri && T.dialog) return T.dialog.save(opts);
  return 'C:\\Users\\you\\Desktop\\my-words.navipack';
}

// A web link opens in the browser; a file Navi saved is shown in File Explorer. Both go through Rust:
// the window itself isn't allowed to open local files, which once left a button doing nothing.
export async function openPath(path) {
  if (isTauri) return invoke(/^https?:/i.test(path) ? 'open_link' : 'reveal_file', /^https?:/i.test(path) ? { url: path } : { path });
  window.open(path, '_blank');
}

export async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    await invoke('copy_text', { text });
  }
}
