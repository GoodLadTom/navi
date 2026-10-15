// Light or dark (Settings > General > Appearance) on every Navi window: the main window, the
// bubble and the meeting card each call applyTheme with the saved choice.
const media = window.matchMedia('(prefers-color-scheme: dark)');
let choice = 'light';

export function applyTheme(next) {
  choice = next || 'light';
  const dark = choice === 'dark' || (choice === 'system' && media.matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
}

// "Match Windows" follows Windows when it switches.
media.addEventListener('change', () => applyTheme(choice));
