//! US to British spelling, deterministic and conservative.
//!
//! OWNER: text agent.
//!
//! Contract: only change words where the British form is unambiguous in normal prose
//! (colour, favourite, organise/organisation/organising, realise, recognise, analyse, centre,
//! theatre, metre (length), litre, travelled/travelling/traveller, cancelled, modelled, labelled,
//! defence, offence, licence (noun only when clearly a noun is hard, so leave "license"), grey,
//! catalogue, dialogue, programme (NOT for computer programs: leave "program"), mum, aluminium,
//! jewellery, pyjamas, cheque only in banking context: skip), and keep the case pattern of the
//! original (Color -> Colour, COLOR -> COLOUR). Never touch words inside URLs, email addresses,
//! file paths, @mentions, `code spans`, or identifiers with underscores or camelCase. Never change
//! "size", "seize", "capsize", "prize", "maize", "baize", "assize". Aim for a few hundred common
//! words plus the -ize/-ization/-yze rule with an exception list.
//!
//! Implementation notes:
//! * About 430 listed words (with their inflections) plus the -ize/-yze rules.
//! * "meter" only becomes "metre" after a quantity ("100 meters"), "dialog" stays before "box".
//! * Proper nouns are left alone: a capitalised word mid-sentence next to another capitalised word
//!   ("Pearl Harbor", "Lincoln Center", "Mr. Gray") is not changed. At the start of a sentence a
//!   capitalised noun followed by another capitalised word ("Center Parcs") is also left alone.
//! * Ambiguous words are never touched: program, license, practice/practices, check, tire, curb,
//!   story, draft, disk, inquire, ax.
//! * Code that reads like prose is left alone too: hyphenated class names with a number
//!   ("text-gray-500"), CSS properties and values ("background-color", "justify-center") and
//!   function calls ("initialize()"). A number in front is prose ("a 3-color print" converts).

use crate::text::spans;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

pub fn britishise(text: &str) -> String {
    britishise_keeping(text, &[])
}

/// Same as [`britishise`], but never changes the given terms (for example dictionary words such as
/// a brand that is spelt the American way). Matching is whole-word and case-insensitive.
pub fn britishise_keeping(text: &str, keep: &[String]) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut protected = spans::protected_ranges(text);
    for term in keep {
        protected.extend(spans::find_whole_ci(text, term));
    }
    let words = word_ranges(text);
    protected.extend(code_like_ranges(text, &words));
    let mut out = String::with_capacity(text.len() + 16);
    let mut last = 0;
    for idx in 0..words.len() {
        let r = &words[idx];
        if spans::overlaps(&protected, r) {
            continue;
        }
        if let Some(rep) = convert(text, &words, idx) {
            out.push_str(&text[last..r.start]);
            out.push_str(&rep);
            last = r.end;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Runs of letters, digits and underscores. Apostrophes and hyphens split words, so "color's"
/// and "color-coded" still convert, while "color_picker" stays one (skipped) identifier.
fn word_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let w = spans::is_word_char(c);
        match (w, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(s..text.len());
    }
    out
}

/// Code the general machine-text rules can't see, because it has no dots or slashes: lower-case
/// utility classes with a number ("text-gray-500", "bg-gray-100", "gray-500"), words joined by
/// hyphens that pair a CSS word with a value ("background-color", "justify-center",
/// "self-center"), and a word called as a function ("initialize()"). Prose compounds
/// ("colour-coded", "self-centred") still convert, and so does a number in front ("a 3-colour
/// print", "a 10-metre dive"): a class name never starts with a digit.
fn code_like_ranges(text: &str, words: &[Range<usize>]) -> Vec<Range<usize>> {
    const CSS_WORDS: &[&str] = &[
        "accent", "align", "background", "bg", "border", "caret", "content", "decoration", "divide", "fill", "font", "items",
        "justify", "object", "origin", "outline", "place", "ring", "scheme", "self", "stroke", "text",
    ];
    const CSS_VALUES: &[&str] = &["center", "color", "colors", "gray"];
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        // The run of words joined by single hyphens that starts here.
        let mut j = i;
        while j + 1 < words.len() && &text[words[j].end..words[j + 1].start] == "-" {
            j += 1;
        }
        let run = words[i].start..words[j].end;
        if j > i {
            let parts: Vec<&str> = words[i..=j].iter().map(|r| &text[r.clone()]).collect();
            let lower = parts.iter().all(|p| !p.bytes().any(|b| b.is_ascii_uppercase()));
            let has_digit = parts.iter().any(|p| p.bytes().any(|b| b.is_ascii_digit()));
            let digit_first = parts[0].bytes().next().is_some_and(|b| b.is_ascii_digit());
            let ends_in_number = parts[parts.len() - 1].bytes().all(|b| b.is_ascii_digit());
            // A utility class: a word first, then three or more parts, a CSS word or a shade number.
            let utility = lower
                && has_digit
                && !digit_first
                && (parts.len() >= 3 || CSS_WORDS.contains(&parts[0]) || ends_in_number);
            let css = lower && parts.iter().any(|p| CSS_WORDS.contains(p)) && parts.iter().any(|p| CSS_VALUES.contains(p));
            if utility || css {
                out.push(run.clone());
            }
        }
        if text[run.end..].starts_with('(') {
            out.push(run);
        }
        i = j + 1;
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Lower,
    Upper,
    Title,
}

/// None for mixed case (camelCase, PascalCase, iPhone), which is treated as an identifier.
fn case_of(w: &str) -> Option<Case> {
    let mut chars = w.chars();
    let first = chars.next()?;
    if w.chars().all(|c| c.is_ascii_lowercase()) {
        return Some(Case::Lower);
    }
    if w.chars().all(|c| c.is_ascii_uppercase()) {
        return Some(Case::Upper);
    }
    if first.is_ascii_uppercase() && chars.all(|c| c.is_ascii_lowercase()) {
        return Some(Case::Title);
    }
    None
}

fn apply_case(rep: &str, case: Case) -> String {
    match case {
        Case::Lower => rep.to_string(),
        Case::Upper => rep.to_ascii_uppercase(),
        Case::Title => {
            let mut c = rep.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        }
    }
}

fn convert(text: &str, words: &[Range<usize>], idx: usize) -> Option<String> {
    let r = words[idx].clone();
    let w = &text[r.clone()];
    if w.len() < 2 || !w.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let case = case_of(w)?;
    let lower = w.to_ascii_lowercase();
    let (rep, is_verb) = lookup(&lower, text, words, idx)?;
    if case == Case::Title {
        let starts = sentence_start(text, r.start);
        let next_cap = neighbour_capitalised(text, words, idx, false);
        if !starts && (next_cap || neighbour_capitalised(text, words, idx, true)) {
            return None;
        }
        if starts && next_cap && !is_verb {
            return None;
        }
    }
    Some(apply_case(&rep, case))
}

/// (British form in lower case, whether it came from the -ize/-yze verb rule).
fn lookup(lower: &str, text: &str, words: &[Range<usize>], idx: usize) -> Option<(String, bool)> {
    match lower {
        "meter" | "meters" => {
            if !prev_is_quantity(text, words, idx) {
                return None;
            }
            let rep = if lower == "meter" { "metre" } else { "metres" };
            return Some((rep.to_string(), false));
        }
        "dialog" | "dialogs" => {
            if let Some(next) = next_word(text, words, idx) {
                let next = next.to_ascii_lowercase();
                if matches!(next.as_str(), "box" | "boxes" | "window" | "windows") {
                    return None;
                }
            }
            let rep = if lower == "dialog" { "dialogue" } else { "dialogues" };
            return Some((rep.to_string(), false));
        }
        _ => {}
    }
    if let Some(rep) = MAP.get(lower) {
        return Some(((*rep).to_string(), false));
    }
    ize_rule(lower)
}

/// -ize/-ise and -yze/-yse. The stem must have at least three letters (so size, seize, prize,
/// maize and baize never match) and words built on "size" (resize, downsize, oversized) are kept.
fn ize_rule(lower: &str) -> Option<(String, bool)> {
    const YZE: &[(&str, &str, bool)] = &[
        ("lyzers", "lysers", false),
        ("lyzer", "lyser", false),
        ("lyzing", "lysing", true),
        ("lyzes", "lyses", true),
        ("lyzed", "lysed", true),
        ("lyze", "lyse", true),
    ];
    for (suffix, rep, verb) in YZE {
        if let Some(stem) = lower.strip_suffix(suffix) {
            if stem.len() >= 2 {
                return Some((format!("{stem}{rep}"), *verb));
            }
            return None;
        }
    }
    const IZE: &[(&str, &str, bool)] = &[
        ("izations", "isations", false),
        ("izational", "isational", false),
        ("ization", "isation", false),
        ("izingly", "isingly", false),
        ("izers", "isers", false),
        ("izer", "iser", false),
        ("izable", "isable", false),
        ("izing", "ising", true),
        ("izes", "ises", true),
        ("ized", "ised", true),
        ("ize", "ise", true),
    ];
    for (suffix, rep, verb) in IZE {
        if let Some(stem) = lower.strip_suffix(suffix) {
            if stem.len() < 3 || is_ize_exception(&format!("{stem}ize")) {
                return None;
            }
            return Some((format!("{stem}{rep}"), *verb));
        }
    }
    None
}

fn is_ize_exception(base: &str) -> bool {
    const EXCEPTIONS: &[&str] = &["size", "seize", "prize", "maize", "baize", "capsize", "assize", "belize", "brize"];
    const SIZE_PREFIXES: &[&str] = &[
        "re", "down", "up", "over", "under", "out", "super", "mid", "king", "queen", "bite", "life", "full", "half", "pint",
        "plus", "fun", "man", "mini", "jumbo", "giant", "family", "economy", "outer", "wee", "child", "travel", "pocket",
        "true", "one", "same", "kid", "large", "small", "medium", "regular", "standard", "double", "single",
    ];
    if EXCEPTIONS.contains(&base) || base.ends_with("seize") || base.ends_with("prize") {
        return true;
    }
    if let Some(prefix) = base.strip_suffix("size") {
        return prefix.is_empty() || SIZE_PREFIXES.contains(&prefix);
    }
    false
}

/// The word token right before `idx`, when only spaces (or a hyphen, if allowed) separate them.
fn prev_word<'a>(text: &'a str, words: &[Range<usize>], idx: usize, allow_hyphen: bool) -> Option<&'a str> {
    let prev = words.get(idx.checked_sub(1)?)?;
    let gap = &text[prev.end..words[idx].start];
    let ok = !gap.is_empty() && gap.chars().all(|c| c == ' ' || c == '\t' || (allow_hyphen && c == '-'));
    ok.then(|| &text[prev.clone()])
}

fn next_word<'a>(text: &'a str, words: &[Range<usize>], idx: usize) -> Option<&'a str> {
    let next = words.get(idx + 1)?;
    let gap = &text[words[idx].end..next.start];
    let ok = !gap.is_empty() && gap.chars().all(|c| c == ' ' || c == '\t');
    ok.then(|| &text[next.clone()])
}

const TITLES: &[&str] = &["mr", "mrs", "ms", "dr", "prof", "st", "sr", "jr", "rev", "capt", "lt", "col", "gen", "sgt", "hon", "mt", "fr"];

/// Is the neighbouring word (whitespace-separated) capitalised, as in a multi-word proper noun?
fn neighbour_capitalised(text: &str, words: &[Range<usize>], idx: usize, prev: bool) -> bool {
    let word = if prev {
        match prev_word(text, words, idx, false) {
            Some(w) => Some(w),
            None => {
                // "Mr. Gray": a title abbreviation before the name counts as a capitalised neighbour.
                let Some(p) = idx.checked_sub(1).and_then(|i| words.get(i)) else {
                    return false;
                };
                let gap = &text[p.end..words[idx].start];
                let w = &text[p.clone()];
                if gap.trim_end() == "." && TITLES.contains(&w.to_ascii_lowercase().as_str()) {
                    return true;
                }
                None
            }
        }
    } else {
        next_word(text, words, idx)
    };
    match word {
        Some(w) => w != "I" && w.chars().next().is_some_and(|c| c.is_uppercase()),
        None => false,
    }
}

/// Does the word at `pos` start a sentence (start of text, after . ! ? : or a line break, allowing
/// for quotes, brackets and list markers in between)?
fn sentence_start(text: &str, pos: usize) -> bool {
    let mut chars = text[..pos].chars().rev().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' || c == '\t' || "\"'“‘([*•#>-–—".contains(c) {
            chars.next();
        } else {
            break;
        }
    }
    match chars.next() {
        None | Some('\n') | Some('\r') | Some('!') | Some('?') | Some(':') => true,
        Some('.') => {
            let before: String = chars.take_while(|c| c.is_alphabetic()).collect();
            let word: String = before.chars().rev().collect::<String>().to_ascii_lowercase();
            !TITLES.contains(&word.as_str())
        }
        _ => false,
    }
}

fn prev_is_quantity(text: &str, words: &[Range<usize>], idx: usize) -> bool {
    const QUANTITY: &[&str] = &[
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen",
        "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen", "twenty", "thirty", "forty", "fifty",
        "sixty", "seventy", "eighty", "ninety", "hundred", "hundreds", "thousand", "thousands", "million", "half",
        "few", "several", "square", "cubic", "dozen",
    ];
    match prev_word(text, words, idx, true) {
        Some(w) => w.chars().all(|c| c.is_ascii_digit()) || QUANTITY.contains(&w.to_ascii_lowercase().as_str()),
        None => false,
    }
}

static MAP: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| PAIRS.iter().copied().collect());

/// US -> British, lower case. Inflections are listed explicitly; nothing here is stemmed.
const PAIRS: &[(&str, &str)] = &[
    // -or -> -our
    ("color", "colour"), ("colors", "colours"), ("colored", "coloured"), ("coloring", "colouring"),
    ("colorings", "colourings"), ("colorful", "colourful"), ("colorfully", "colourfully"), ("colorless", "colourless"),
    ("colorist", "colourist"), ("colorists", "colourists"), ("colorway", "colourway"), ("colorways", "colourways"),
    ("colorblind", "colourblind"), ("colorize", "colourise"), ("colorized", "colourised"), ("colorizes", "colourises"),
    ("colorizing", "colourising"), ("colorization", "colourisation"), ("discolor", "discolour"),
    ("discolors", "discolours"), ("discolored", "discoloured"), ("discoloring", "discolouring"),
    ("discoloration", "discolouration"), ("multicolor", "multicolour"), ("multicolored", "multicoloured"),
    ("watercolor", "watercolour"), ("watercolors", "watercolours"), ("tricolor", "tricolour"),
    ("flavor", "flavour"), ("flavors", "flavours"), ("flavored", "flavoured"), ("flavoring", "flavouring"),
    ("flavorings", "flavourings"), ("flavorful", "flavourful"), ("flavorless", "flavourless"),
    ("flavorsome", "flavoursome"),
    ("humor", "humour"), ("humors", "humours"), ("humored", "humoured"), ("humoring", "humouring"),
    ("humorless", "humourless"),
    ("honor", "honour"), ("honors", "honours"), ("honored", "honoured"), ("honoring", "honouring"),
    ("honorable", "honourable"), ("honorably", "honourably"), ("dishonor", "dishonour"), ("dishonors", "dishonours"),
    ("dishonored", "dishonoured"), ("dishonoring", "dishonouring"), ("dishonorable", "dishonourable"),
    ("dishonorably", "dishonourably"),
    ("labor", "labour"), ("labors", "labours"), ("labored", "laboured"), ("laboring", "labouring"),
    ("laborer", "labourer"), ("laborers", "labourers"), ("belabor", "belabour"), ("belabors", "belabours"),
    ("belabored", "belaboured"), ("belaboring", "belabouring"),
    ("neighbor", "neighbour"), ("neighbors", "neighbours"), ("neighbored", "neighboured"),
    ("neighboring", "neighbouring"), ("neighborhood", "neighbourhood"), ("neighborhoods", "neighbourhoods"),
    ("neighborly", "neighbourly"), ("neighborliness", "neighbourliness"),
    ("behavior", "behaviour"), ("behaviors", "behaviours"), ("behavioral", "behavioural"),
    ("behaviorally", "behaviourally"), ("behaviorism", "behaviourism"), ("behaviorist", "behaviourist"),
    ("behaviorists", "behaviourists"), ("misbehavior", "misbehaviour"),
    ("favor", "favour"), ("favors", "favours"), ("favored", "favoured"), ("favoring", "favouring"),
    ("favorable", "favourable"), ("favorably", "favourably"), ("unfavorable", "unfavourable"),
    ("unfavorably", "unfavourably"), ("favorite", "favourite"), ("favorites", "favourites"),
    ("favoritism", "favouritism"), ("disfavor", "disfavour"),
    ("harbor", "harbour"), ("harbors", "harbours"), ("harbored", "harboured"), ("harboring", "harbouring"),
    ("rumor", "rumour"), ("rumors", "rumours"), ("rumored", "rumoured"), ("rumoring", "rumouring"),
    ("savior", "saviour"), ("saviors", "saviours"), ("savor", "savour"), ("savors", "savours"),
    ("savored", "savoured"), ("savoring", "savouring"), ("savory", "savoury"), ("unsavory", "unsavoury"),
    ("armor", "armour"), ("armors", "armours"), ("armored", "armoured"), ("armoring", "armouring"),
    ("armory", "armoury"), ("armories", "armouries"), ("armorer", "armourer"), ("armorers", "armourers"),
    ("ardor", "ardour"), ("candor", "candour"), ("clamor", "clamour"), ("clamors", "clamours"),
    ("clamored", "clamoured"), ("clamoring", "clamouring"), ("endeavor", "endeavour"), ("endeavors", "endeavours"),
    ("endeavored", "endeavoured"), ("endeavoring", "endeavouring"), ("fervor", "fervour"), ("glamor", "glamour"),
    ("odor", "odour"), ("odors", "odours"), ("odorless", "odourless"), ("malodor", "malodour"),
    ("parlor", "parlour"), ("parlors", "parlours"), ("rancor", "rancour"), ("rigor", "rigour"), ("rigors", "rigours"),
    ("splendor", "splendour"), ("splendors", "splendours"), ("tumor", "tumour"), ("tumors", "tumours"),
    ("valor", "valour"), ("vapor", "vapour"), ("vapors", "vapours"), ("vigor", "vigour"), ("demeanor", "demeanour"),
    ("demeanors", "demeanours"), ("arbor", "arbour"), ("arbors", "arbours"), ("succor", "succour"),
    ("mold", "mould"), ("molds", "moulds"), ("molded", "moulded"), ("molding", "moulding"), ("moldings", "mouldings"),
    ("moldy", "mouldy"), ("moldier", "mouldier"), ("moldiest", "mouldiest"), ("molt", "moult"), ("molts", "moults"),
    ("molted", "moulted"), ("molting", "moulting"), ("smolder", "smoulder"), ("smolders", "smoulders"),
    ("smoldered", "smouldered"), ("smoldering", "smouldering"),
    // -er -> -re
    ("center", "centre"), ("centers", "centres"), ("centered", "centred"), ("centering", "centring"),
    ("centerpiece", "centrepiece"), ("centerpieces", "centrepieces"), ("centerfold", "centrefold"),
    ("centerline", "centreline"), ("epicenter", "epicentre"), ("epicenters", "epicentres"),
    ("theater", "theatre"), ("theaters", "theatres"), ("theatergoer", "theatregoer"), ("theatergoers", "theatregoers"),
    ("kilometer", "kilometre"), ("kilometers", "kilometres"), ("centimeter", "centimetre"),
    ("centimeters", "centimetres"), ("millimeter", "millimetre"), ("millimeters", "millimetres"),
    ("nanometer", "nanometre"), ("nanometers", "nanometres"), ("decimeter", "decimetre"), ("decimeters", "decimetres"),
    ("liter", "litre"), ("liters", "litres"), ("milliliter", "millilitre"), ("milliliters", "millilitres"),
    ("centiliter", "centilitre"), ("centiliters", "centilitres"), ("deciliter", "decilitre"), ("kiloliter", "kilolitre"),
    ("fiber", "fibre"), ("fibers", "fibres"), ("fiberglass", "fibreglass"), ("fiberboard", "fibreboard"),
    ("caliber", "calibre"), ("calibers", "calibres"), ("saber", "sabre"), ("sabers", "sabres"), ("somber", "sombre"),
    ("somberly", "sombrely"), ("specter", "spectre"), ("specters", "spectres"), ("luster", "lustre"),
    ("lackluster", "lacklustre"), ("meager", "meagre"), ("meagerly", "meagrely"), ("ocher", "ochre"),
    ("scepter", "sceptre"), ("scepters", "sceptres"), ("sepulcher", "sepulchre"), ("miter", "mitre"),
    ("miters", "mitres"), ("louver", "louvre"), ("louvers", "louvres"), ("goiter", "goitre"),
    ("reconnoiter", "reconnoitre"), ("saltpeter", "saltpetre"),
    ("maneuver", "manoeuvre"), ("maneuvers", "manoeuvres"), ("maneuvered", "manoeuvred"),
    ("maneuvering", "manoeuvring"), ("maneuverable", "manoeuvrable"), ("maneuverability", "manoeuvrability"),
    // doubled l
    ("traveled", "travelled"), ("traveling", "travelling"), ("traveler", "traveller"), ("travelers", "travellers"),
    ("canceled", "cancelled"), ("canceling", "cancelling"),
    ("modeled", "modelled"), ("modeling", "modelling"), ("modeler", "modeller"), ("modelers", "modellers"),
    ("labeled", "labelled"), ("labeling", "labelling"), ("labeler", "labeller"),
    ("leveled", "levelled"), ("leveling", "levelling"), ("leveler", "leveller"),
    ("fueled", "fuelled"), ("fueling", "fuelling"),
    ("dueled", "duelled"), ("dueling", "duelling"), ("duelist", "duellist"), ("duelists", "duellists"),
    ("signaled", "signalled"), ("signaling", "signalling"),
    ("totaled", "totalled"), ("totaling", "totalling"),
    ("marveled", "marvelled"), ("marveling", "marvelling"), ("marvelous", "marvellous"),
    ("marvelously", "marvellously"),
    ("jeweled", "jewelled"), ("jeweler", "jeweller"), ("jewelers", "jewellers"), ("jewelry", "jewellery"),
    ("counseled", "counselled"), ("counseling", "counselling"), ("counselor", "counsellor"),
    ("counselors", "counsellors"), ("councilor", "councillor"), ("councilors", "councillors"),
    ("channeled", "channelled"), ("channeling", "channelling"),
    ("tunneled", "tunnelled"), ("tunneling", "tunnelling"),
    ("quarreled", "quarrelled"), ("quarreling", "quarrelling"),
    ("rivaled", "rivalled"), ("rivaling", "rivalling"),
    ("equaled", "equalled"), ("equaling", "equalling"),
    ("dialed", "dialled"), ("dialing", "dialling"),
    ("funneled", "funnelled"), ("funneling", "funnelling"),
    ("grueling", "gruelling"),
    ("libeled", "libelled"), ("libeling", "libelling"), ("libelous", "libellous"),
    ("paneled", "panelled"), ("paneling", "panelling"), ("panelist", "panellist"), ("panelists", "panellists"),
    ("pedaled", "pedalled"), ("pedaling", "pedalling"),
    ("penciled", "pencilled"), ("penciling", "pencilling"),
    ("reveled", "revelled"), ("reveling", "revelling"), ("reveler", "reveller"), ("revelers", "revellers"),
    ("shoveled", "shovelled"), ("shoveling", "shovelling"),
    ("shriveled", "shrivelled"), ("shriveling", "shrivelling"),
    ("snorkeled", "snorkelled"), ("snorkeling", "snorkelling"),
    ("spiraled", "spiralled"), ("spiraling", "spiralling"),
    ("stenciled", "stencilled"), ("stenciling", "stencilling"),
    ("swiveled", "swivelled"), ("swiveling", "swivelling"),
    ("toweled", "towelled"), ("toweling", "towelling"),
    ("unraveled", "unravelled"), ("unraveling", "unravelling"),
    ("initialed", "initialled"), ("initialing", "initialling"),
    ("barreled", "barrelled"), ("barreling", "barrelling"),
    ("beveled", "bevelled"), ("beveling", "bevelling"),
    ("chiseled", "chiselled"), ("chiseling", "chiselling"),
    ("disheveled", "dishevelled"), ("enameled", "enamelled"),
    ("marshaled", "marshalled"), ("marshaling", "marshalling"),
    ("medalist", "medallist"), ("medalists", "medallists"),
    ("kidnaped", "kidnapped"), ("kidnaping", "kidnapping"), ("kidnaper", "kidnapper"),
    ("worshiped", "worshipped"), ("worshiping", "worshipping"), ("worshiper", "worshipper"),
    ("worshipers", "worshippers"),
    ("woolen", "woollen"), ("woolens", "woollens"), ("wooly", "woolly"),
    ("tranquility", "tranquillity"), ("tranquilize", "tranquillise"), ("tranquilizes", "tranquillises"),
    ("tranquilized", "tranquillised"), ("tranquilizing", "tranquillising"), ("tranquilizer", "tranquilliser"),
    ("tranquilizers", "tranquillisers"),
    ("fulfill", "fulfil"), ("fulfills", "fulfils"), ("fulfillment", "fulfilment"),
    ("enroll", "enrol"), ("enrolls", "enrols"), ("enrollment", "enrolment"), ("enrollments", "enrolments"),
    ("installment", "instalment"), ("installments", "instalments"),
    ("skillful", "skilful"), ("skillfully", "skilfully"), ("willful", "wilful"), ("willfully", "wilfully"),
    ("distill", "distil"), ("distills", "distils"), ("instill", "instil"), ("instills", "instils"),
    ("appall", "appal"), ("appalls", "appals"), ("enthrall", "enthral"), ("enthralls", "enthrals"),
    // -ense -> -ence, and the verb forms of practise
    ("defense", "defence"), ("defenses", "defences"), ("defenseless", "defenceless"),
    ("offense", "offence"), ("offenses", "offences"), ("pretense", "pretence"), ("pretenses", "pretences"),
    ("practiced", "practised"), ("practicing", "practising"),
    // -og -> -ogue
    ("catalog", "catalogue"), ("catalogs", "catalogues"), ("cataloged", "catalogued"), ("cataloging", "cataloguing"),
    ("analog", "analogue"), ("analogs", "analogues"), ("monolog", "monologue"), ("monologs", "monologues"),
    ("epilog", "epilogue"), ("epilogs", "epilogues"), ("travelog", "travelogue"), ("travelogs", "travelogues"),
    // ae / oe
    ("anesthesia", "anaesthesia"), ("anesthetic", "anaesthetic"), ("anesthetics", "anaesthetics"),
    ("anesthetist", "anaesthetist"), ("anesthetists", "anaesthetists"), ("anesthesiologist", "anaesthesiologist"),
    ("anesthetize", "anaesthetise"), ("anesthetized", "anaesthetised"), ("anesthetizing", "anaesthetising"),
    ("anemia", "anaemia"), ("anemic", "anaemic"),
    ("archeology", "archaeology"), ("archeological", "archaeological"), ("archeologist", "archaeologist"),
    ("archeologists", "archaeologists"),
    ("pediatric", "paediatric"), ("pediatrics", "paediatrics"), ("pediatrician", "paediatrician"),
    ("pediatricians", "paediatricians"), ("orthopedic", "orthopaedic"), ("orthopedics", "orthopaedics"),
    ("hemoglobin", "haemoglobin"), ("hemorrhage", "haemorrhage"), ("hemorrhages", "haemorrhages"),
    ("hemorrhaging", "haemorrhaging"), ("hemorrhoids", "haemorrhoids"), ("hemophilia", "haemophilia"),
    ("hematology", "haematology"), ("leukemia", "leukaemia"), ("diarrhea", "diarrhoea"), ("esophagus", "oesophagus"),
    ("estrogen", "oestrogen"), ("gynecology", "gynaecology"), ("gynecologist", "gynaecologist"),
    ("gynecologists", "gynaecologists"), ("gynecological", "gynaecological"), ("cesarean", "caesarean"),
    ("edema", "oedema"), ("feces", "faeces"), ("fecal", "faecal"), ("paleontology", "palaeontology"),
    ("paleontologist", "palaeontologist"), ("paleontologists", "palaeontologists"), ("esthetic", "aesthetic"),
    ("esthetics", "aesthetics"), ("esthetically", "aesthetically"), ("pedophile", "paedophile"),
    ("pedophiles", "paedophiles"), ("pedophilia", "paedophilia"), ("ameba", "amoeba"), ("amebas", "amoebas"),
    ("cesium", "caesium"),
    // assorted single words
    ("gray", "grey"), ("grays", "greys"), ("grayer", "greyer"), ("grayest", "greyest"), ("grayish", "greyish"),
    ("grayscale", "greyscale"), ("grayed", "greyed"), ("graying", "greying"), ("grayness", "greyness"),
    ("mom", "mum"), ("moms", "mums"), ("mommy", "mummy"), ("mommies", "mummies"),
    ("aluminum", "aluminium"), ("pajamas", "pyjamas"),
    ("plow", "plough"), ("plows", "ploughs"), ("plowed", "ploughed"), ("plowing", "ploughing"),
    ("snowplow", "snowplough"), ("snowplows", "snowploughs"),
    ("mustache", "moustache"), ("mustaches", "moustaches"), ("mustached", "moustached"),
    ("cozy", "cosy"), ("cozier", "cosier"), ("coziest", "cosiest"), ("cozily", "cosily"), ("coziness", "cosiness"),
    ("skeptic", "sceptic"), ("skeptics", "sceptics"), ("skeptical", "sceptical"), ("skeptically", "sceptically"),
    ("skepticism", "scepticism"),
    ("sulfur", "sulphur"), ("sulfate", "sulphate"), ("sulfates", "sulphates"), ("sulfide", "sulphide"),
    ("sulfuric", "sulphuric"), ("sulfurous", "sulphurous"),
    ("donut", "doughnut"), ("donuts", "doughnuts"), ("artifact", "artefact"), ("artifacts", "artefacts"),
    ("judgment", "judgement"), ("judgments", "judgements"), ("judgmental", "judgemental"),
    ("acknowledgment", "acknowledgement"), ("acknowledgments", "acknowledgements"), ("abridgment", "abridgement"),
    ("lodgment", "lodgement"),
    ("aging", "ageing"), ("airplane", "aeroplane"), ("airplanes", "aeroplanes"),
    ("omelet", "omelette"), ("omelets", "omelettes"), ("tidbit", "titbit"), ("tidbits", "titbits"),
    ("yogurt", "yoghurt"), ("yogurts", "yoghurts"), ("mollusk", "mollusc"), ("mollusks", "molluscs"),
    ("carburetor", "carburettor"), ("carburetors", "carburettors"), ("chili", "chilli"), ("chilies", "chillies"),
    ("chilis", "chillies"), ("furor", "furore"), ("checkered", "chequered"), ("checkerboard", "chequerboard"),
    ("likable", "likeable"), ("sizable", "sizeable"), ("livable", "liveable"), ("unlivable", "unliveable"),
    ("salable", "saleable"), ("toward", "towards"), ("afterward", "afterwards"), ("percent", "per cent"),
    ("math", "maths"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> String {
        britishise(s)
    }

    #[test]
    fn brief_examples() {
        assert_eq!(b("Color"), "Colour");
        assert_eq!(b("ORGANIZE"), "ORGANISE");
        assert_eq!(b("I realized it"), "I realised it");
        assert_eq!(b("we traveled far"), "we travelled far");
        assert_eq!(b("the center of town"), "the centre of town");
        assert_eq!(b("concentrate on the concert"), "concentrate on the concert");
        assert_eq!(b("a licensed driver"), "a licensed driver");
        assert_eq!(b("my program crashed"), "my program crashed");
    }

    #[test]
    fn size_family_and_lookalikes_are_untouched() {
        for w in [
            "size", "sizes", "sized", "sizing", "seize", "seized", "seizing", "prize", "prizes", "prized", "capsize",
            "capsized", "maize", "baize", "assize", "resize", "resized", "downsizing", "oversized", "upsize",
            "supersize", "midsize", "Pfizer", "Belize", "citizen", "horizon", "wizard", "bizarre", "frieze",
        ] {
            assert_eq!(b(w), w, "{w} should not change");
        }
        assert_eq!(b("a sizable crowd"), "a sizeable crowd");
    }

    #[test]
    fn ize_and_yze_rule() {
        assert_eq!(b("organization"), "organisation");
        assert_eq!(b("Organizing the organizers"), "Organising the organisers");
        assert_eq!(b("recognize recognizable"), "recognise recognisable");
        assert_eq!(b("emphasize synthesizer hypothesize fantasize"), "emphasise synthesiser hypothesise fantasise");
        assert_eq!(b("apologize ionize itemize"), "apologise ionise itemise");
        assert_eq!(b("civilizations"), "civilisations");
        assert_eq!(b("analyze paralyzed analyzer catalyzes"), "analyse paralysed analyser catalyses");
        assert_eq!(b("colorize"), "colourise");
        assert_eq!(b("tranquilizer"), "tranquilliser");
    }

    #[test]
    fn listed_words() {
        let cases = [
            ("favorite flavor", "favourite flavour"),
            ("neighbors behavior", "neighbours behaviour"),
            ("theater meters", "theatre meters"),
            ("the theater was 100 meters away", "the theatre was 100 metres away"),
            ("five kilometers", "five kilometres"),
            ("two liters of milk", "two litres of milk"),
            ("canceled modeled labeled", "cancelled modelled labelled"),
            ("defense offense pretense", "defence offence pretence"),
            ("gray catalog", "grey catalogue"),
            ("my mom", "my mum"),
            ("aluminum jewelry pajamas", "aluminium jewellery pyjamas"),
            ("fulfill enrollment", "fulfil enrolment"),
            ("the skeptical judgment", "the sceptical judgement"),
            ("she was practicing", "she was practising"),
            ("50 percent", "50 per cent"),
            ("do the math", "do the maths"),
            ("an anti-aging cream", "an anti-ageing cream"),
        ];
        for (us, uk) in cases {
            assert_eq!(b(us), uk);
        }
    }

    #[test]
    fn ambiguous_words_stay() {
        for s in ["practice makes perfect", "a parking meter", "the gas meters", "check the tire", "a curb", "first draft", "the license", "programs and programming"] {
            assert_eq!(b(s), s);
        }
    }

    #[test]
    fn dialog_box_stays() {
        assert_eq!(b("open the dialog box"), "open the dialog box");
        assert_eq!(b("a long dialog"), "a long dialogue");
    }

    #[test]
    fn keeps_case_pattern() {
        assert_eq!(b("COLOR"), "COLOUR");
        assert_eq!(b("Favorite things."), "Favourite things.");
        assert_eq!(b("THE NEIGHBORHOOD"), "THE NEIGHBOURHOOD");
        assert_eq!(b("Realize this."), "Realise this.");
    }

    #[test]
    fn apostrophes_and_hyphens() {
        assert_eq!(b("the color's tone"), "the colour's tone");
        assert_eq!(b("color-coded and self-centered"), "colour-coded and self-centred");
    }

    #[test]
    fn machine_text_is_untouched() {
        let cases = [
            "see https://example.com/color/center",
            "email alex@color.example now",
            "ping @colorbot",
            "use `color` here",
            "edit src/color/center.ts",
            "open colors.json",
            "set color_picker",
            "call colorPicker",
            "the ColorPicker class",
            "C:\\color\\center",
            "tag #color",
            "```\nlet color = center();\n```",
        ];
        for s in cases {
            assert_eq!(b(s), s, "{s}");
        }
    }

    #[test]
    fn code_that_reads_like_prose_is_untouched() {
        for s in [
            "add text-gray-500 and justify-center to the card",
            "set background-color on the body",
            "use self-center and items-center here",
            "make it bg-gray-100",
            "use gray-500 for the border",
            "add border-gray-200 hover:bg-gray-50",
            "call the initialize() method",
            "then color(x) returns it",
        ] {
            assert_eq!(b(s), s, "{s}");
        }
        // Prose next to it, and prose compounds, still convert.
        assert_eq!(b("the gray-blue sky has a text-gray-500 label"), "the grey-blue sky has a text-gray-500 label");
        // A number in front is a quantity, not a class name.
        assert_eq!(b("a 3-color print run"), "a 3-colour print run");
        assert_eq!(b("a 2-center tour"), "a 2-centre tour");
        assert_eq!(b("a 10-meter-high wall"), "a 10-metre-high wall");
        assert_eq!(b("a text-heavy color scheme"), "a text-heavy colour scheme");
        assert_eq!(b("Color-coded and self-centered (mostly)"), "Colour-coded and self-centred (mostly)");
    }

    #[test]
    fn proper_nouns_are_left_alone() {
        assert_eq!(b("I visited Pearl Harbor yesterday"), "I visited Pearl Harbor yesterday");
        assert_eq!(b("we met at the Lincoln Center."), "we met at the Lincoln Center.");
        assert_eq!(b("Center Parcs is fun"), "Center Parcs is fun");
        assert_eq!(b("Mr. Gray called"), "Mr. Gray called");
        assert_eq!(b("I read Dorian Gray"), "I read Dorian Gray");
        assert_eq!(b("the harbor is gray"), "the harbour is grey");
        assert_eq!(b("Analyze Tesla's numbers"), "Analyse Tesla's numbers");
    }

    #[test]
    fn idempotent_and_british_text_unchanged() {
        let uk = "We organised the colour scheme at the centre and travelled to the theatre.";
        assert_eq!(b(uk), uk);
        let us = "We organized the color scheme at the center and traveled to the theater.";
        let once = b(us);
        assert_eq!(once, uk);
        assert_eq!(b(&once), once);
    }

    #[test]
    fn keeping_protects_terms() {
        let keep = vec!["Color Studio".to_string(), "favorite".to_string()];
        assert_eq!(britishise_keeping("I use color studio daily, my favorite color", &keep), "I use color studio daily, my favorite colour");
    }

    #[test]
    fn non_ascii_and_digits_are_skipped() {
        assert_eq!(b("color2 café colors"), "color2 café colours");
    }
}
