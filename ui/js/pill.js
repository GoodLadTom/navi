// The overlay window: shows the bubble and follows the "pill", "level" and "ramble-partial" events.
import { listen, invoke, isTauri } from './bridge.js';
import { BubbleView } from './bubble.js';
import { applyTheme } from './theme.js';

// The firefly has the whole screen here.
const bubble = new BubbleView(document.body, { roam: true });

// The theme and the firefly setting, now and whenever Settings change.
const followTheme = () => invoke('get_settings').then((s) => {
  applyTheme(s?.general?.theme);
  bubble.setFirefly(s?.general?.showFirefly !== false);
}).catch(() => {});
followTheme();
listen('settings-changed', followTheme);

listen('pill', (p) => bubble.set(p));
listen('level', (v) => bubble.level(v));
listen('ramble-partial', (text) => bubble.live(text));

if (!isTauri) {
  // Browser preview: loop a demo take on a stone background.
  document.body.classList.add('preview');
  const loop = async () => { await invoke('simulate_take'); setTimeout(loop, 1400); };
  loop();
}
