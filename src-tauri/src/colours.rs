//! Navi's colours (Settings > General > Colour, or "Navi, turn gold"). The interface draws each one
//! from ui/css/tokens.css; this holds what Rust needs: the names, the words the voice command
//! listens for (spoken.rs), and the logo tile's colours, which the tray icon is redrawn in.

use crate::state::App;
use std::sync::Arc;
use tauri::image::Image;

pub struct Colour {
    /// As saved in settings.json (general.colour) and in tokens.css (data-colour).
    pub id: &'static str,
    /// As the bubble and Settings name it.
    pub name: &'static str,
    /// What "Navi, turn ..." listens for, as phrase keys: its name, then plainer words for it.
    /// One word each: spoken.rs adds them to the end of the command's phrases.
    pub words: &'static [&'static str],
    /// The logo's tile and the firefly on it (tokens.css --tile and --tile-mark).
    pub tile: [u8; 3],
    pub mark: [u8; 3],
}

/// In the order Settings shows them. Lilac is the one Navi starts with.
pub const COLOURS: &[Colour] = &[
    Colour { id: "lilac", name: "Lilac", words: &["lilac", "purple", "lavender"], tile: [0x80, 0x64, 0xD6], mark: [0xFF, 0xFF, 0xFF] },
    Colour { id: "gold", name: "Gold", words: &["gold", "golden", "yellow"], tile: [0xEC, 0xC8, 0x25], mark: [0x2A, 0x27, 0x15] },
    Colour { id: "teal", name: "Teal", words: &["teal", "green"], tile: [0x01, 0x87, 0x7D], mark: [0xFF, 0xFF, 0xFF] },
    Colour { id: "sky", name: "Sky", words: &["sky", "blue"], tile: [0x03, 0x83, 0xBD], mark: [0xFF, 0xFF, 0xFF] },
    Colour { id: "rose", name: "Rose", words: &["rose", "pink"], tile: [0xC6, 0x4A, 0x7D], mark: [0xFF, 0xFF, 0xFF] },
    Colour { id: "graphite", name: "Graphite", words: &["graphite", "grey", "gray"], tile: [0x5D, 0x64, 0x6F], mark: [0xFF, 0xFF, 0xFF] },
];

/// The colour a new settings.json starts with.
pub const DEFAULT: &str = "lilac";

/// The colour saved as `id`; Lilac for anything else (a hand-edited file, or a colour from a newer
/// Navi), as the interface does (theme.js colourId).
pub fn find(id: &str) -> &'static Colour {
    COLOURS.iter().find(|c| c.id == id.trim()).unwrap_or(&COLOURS[0])
}

/// The colour that phrase key `word` names ("gold", "yellow"), if any.
pub fn by_word(word: &str) -> Option<&'static Colour> {
    COLOURS.iter().find(|c| c.words.contains(&word))
}

/// Makes `id` Navi's colour, as picking it in Settings does: saved, every window told
/// (settings-changed: the bubble and the corner card change at once) and the tray icon redrawn.
pub fn switch(app: &Arc<App>, id: &str) -> &'static Colour {
    let colour = find(id);
    let settings = app.settings();
    if settings.general.colour != colour.id {
        let mut next = settings;
        next.general.colour = colour.id.to_string();
        if let Err(e) = crate::settings::save(&app.paths, &next) {
            log::warn!("could not save the colour: {e:#}");
        }
        *app.settings.write() = next;
        app.emit("settings-changed", ());
        set_tray(app);
    }
    log::info!("colour is now {}", colour.name);
    colour
}

/// Redraws the tray icon in the colour from Settings.
pub fn set_tray(app: &App) {
    let Some(tray) = app.handle.tray_by_id("navi") else { return };
    let result = tray_icon(&app.settings().general.colour).and_then(|icon| tray.set_icon(Some(icon)));
    if let Err(e) = result {
        log::warn!("could not recolour the tray icon: {e}");
    }
}

/// The tray icon (icons/tray.png, the logo on its lilac tile) in colour `id`.
pub fn tray_icon(id: &str) -> tauri::Result<Image<'static>> {
    let lilac = Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let colour = find(id);
    if colour.id == DEFAULT {
        return Ok(lilac);
    }
    let mut rgba = lilac.rgba().to_vec();
    recolour(&mut rgba, colour.tile, colour.mark);
    Ok(Image::new_owned(rgba, lilac.width(), lilac.height()))
}

/// tray.png's tile colour (scripts/make_icons.py TILE). Its mark is white.
const SOURCE_TILE: [u8; 3] = [0x80, 0x64, 0xD6];

/// Every pixel of tray.png is the lilac tile, the white firefly or a blend of the two at their
/// edges. Each keeps its blend, now between `tile` and `mark`; transparency is untouched.
fn recolour(rgba: &mut [u8], tile: [u8; 3], mark: [u8; 3]) {
    let span: i32 = SOURCE_TILE.iter().map(|&c| 255 - c as i32).sum();
    for px in rgba.as_chunks_mut::<4>().0 {
        if px[3] == 0 {
            continue;
        }
        // How far from the tile towards white, over all three channels at once.
        let towards: i32 = (0..3).map(|i| px[i] as i32 - SOURCE_TILE[i] as i32).sum();
        let t = (towards as f32 / span as f32).clamp(0.0, 1.0);
        for i in 0..3 {
            px[i] = (tile[i] as f32 + (mark[i] as f32 - tile[i] as f32) * t).round() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(c: [u8; 3]) -> String {
        format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
    }

    #[test]
    fn unknown_colours_are_lilac() {
        assert_eq!(find("gold").name, "Gold");
        assert_eq!(find(" teal ").id, "teal");
        for id in ["", "orange", "Gold", "purple"] {
            assert_eq!(find(id).id, DEFAULT, "{id:?}");
        }
        assert_eq!(COLOURS[0].id, DEFAULT);
    }

    #[test]
    fn every_word_names_one_colour_and_is_one_lower_case_word() {
        let mut seen = Vec::new();
        for c in COLOURS {
            assert_eq!(c.words[0], c.id, "its own name is heard first");
            for w in c.words {
                assert_eq!(crate::types::phrase_key(w), *w, "{w}: a phrase key");
                assert!(!w.contains(' '), "{w}: one word");
                assert!(!seen.contains(w), "{w} twice");
                seen.push(*w);
                assert_eq!(by_word(w).map(|x| x.id), Some(c.id));
            }
        }
        assert!(by_word("orange").is_none() && by_word("navy").is_none());
    }

    #[test]
    fn the_interface_has_the_same_colours() {
        // tokens.css draws each colour and theme.js lists them for Settings: the same ids, and the
        // same tile, as the tray icon is drawn in.
        let css = include_str!("../../ui/css/tokens.css");
        let js = include_str!("../../ui/js/theme.js");
        for c in COLOURS {
            assert!(js.contains(&format!("{{ id: '{}', name: '{}', tile: '{}' }}", c.id, c.name, hex(c.tile))), "theme.js: {}", c.id);
            let tile = format!("--tile: {};", hex(c.tile));
            let mark = format!("--tile-mark: {};", hex(c.mark));
            let block = if c.id == DEFAULT {
                css.split(":root[data-theme=\"dark\"]").next().unwrap()
            } else {
                let start = css.find(&format!(":root[data-colour=\"{}\"] {{", c.id)).unwrap_or_else(|| panic!("tokens.css has no {}", c.id));
                css[start..].split('}').next().unwrap()
            };
            assert!(block.contains(&tile) && block.contains(&mark), "tokens.css {}: {tile} {mark}", c.id);
            if c.id != DEFAULT {
                assert!(css.contains(&format!(":root[data-colour=\"{}\"][data-theme=\"dark\"] {{", c.id)), "tokens.css has no dark {}", c.id);
            }
        }
    }

    #[test]
    fn the_tray_icon_keeps_its_shape_in_every_colour() {
        let lilac = Image::from_bytes(include_bytes!("../icons/tray.png")).unwrap();
        assert_eq!(tray_icon("lilac").unwrap().rgba(), lilac.rgba(), "lilac is the icon as it is");
        let gold = find("gold");
        let icon = tray_icon("gold").unwrap();
        assert_eq!((icon.width(), icon.height()), (lilac.width(), lilac.height()));
        for (a, b) in lilac.rgba().as_chunks::<4>().0.iter().zip(icon.rgba().as_chunks::<4>().0) {
            assert_eq!(a[3], b[3], "transparency is untouched");
            if a[3] == 255 && a[..3] == SOURCE_TILE {
                assert_eq!(b[..3], gold.tile);
            }
            if a[3] == 255 && a[..3] == [255, 255, 255] {
                assert_eq!(b[..3], gold.mark);
            }
        }
    }

    #[test]
    fn edges_keep_their_blend() {
        let mut px = vec![0x80, 0x64, 0xD6, 255, 255, 255, 255, 255, 191, 178, 234, 128, 9, 9, 9, 0];
        recolour(&mut px, [0, 100, 200], [200, 100, 0]);
        assert_eq!(&px[0..4], &[0, 100, 200, 255], "the tile");
        assert_eq!(&px[4..8], &[200, 100, 0, 255], "the mark");
        assert_eq!(&px[8..12], &[100, 100, 100, 128], "half way, half transparent");
        assert_eq!(&px[12..16], &[9, 9, 9, 0], "fully transparent: left alone");
    }
}
