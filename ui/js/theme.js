// Light or dark (Settings > General > Appearance) and the colour (Settings > General > Colour, or
// "Navi, turn gold") on every Navi window: the main window, the bubble and the meeting card each
// call applyTheme with the saved choices, and again on settings-changed.
const media = window.matchMedia('(prefers-color-scheme: dark)');
let choice = 'light';
let colour = 'lilac';

// The colours, as src-tauri/src/colours.rs COLOURS has them. `tile`: the logo tile's colour
// (tokens.css --tile), the swatch Settings shows for it.
export const COLOURS = [
  { id: 'lilac', name: 'Lilac', tile: '#8064D6' },
  { id: 'gold', name: 'Gold', tile: '#ECC825' },
  { id: 'teal', name: 'Teal', tile: '#01877D' },
  { id: 'sky', name: 'Sky', tile: '#0383BD' },
  { id: 'rose', name: 'Rose', tile: '#C64A7D' },
  { id: 'graphite', name: 'Graphite', tile: '#5D646F' },
];

// A colour Navi doesn't have (a hand-edited settings.json) shows as Lilac, as colours.rs find does.
export const colourId = (id) => (COLOURS.some((c) => c.id === id) ? id : 'lilac');

export function applyTheme(next, nextColour) {
  choice = next || 'light';
  colour = colourId(nextColour);
  const dark = choice === 'dark' || (choice === 'system' && media.matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  document.documentElement.dataset.colour = colour;
}

// "Match Windows" ("Match macOS" on a Mac) follows the system when it switches.
media.addEventListener('change', () => applyTheme(choice, colour));
