//! Learning from your corrections: compare what Navi pasted with what the field says later.
//!
//! OWNER: text agent.
//!
//! Contract:
//! * `find_corrections(inserted, field_now)`: locate the inserted text inside the field (it may have
//!   moved or have text around it; find the best-matching window, word-level), diff word by word
//!   (similar crate), and return substitutions that look like spelling fixes of the same word or
//!   name: 1-3 words replaced by 1-3 words, similar sound/spelling (normalised Levenshtein ≥ 0.5 or
//!   the replacement is a capitalisation/spacing change like "fig ma" -> "Figma"), not
//!   just punctuation, not deletions, not rewrites of whole clauses. Return at most 3 per take.
//! * If the inserted text cannot be found at all (≥ 60% of its words missing), return nothing:
//!   the user rewrote it or moved on, and that is not a correction.
//!
//! Also rejected, because they would make harmful dictionary entries: changes involving digits
//! ("2025" -> "2026" is a fact, not a spelling), grammar swaps between two common words ("then" ->
//! "than", "their" -> "there"), inflections ("call" -> "calls"), opposites ("are" -> "aren't",
//! "possible" -> "impossible", "include" -> "exclude"), one weekday, month, number or compass point
//! for another ("Tuesday" -> "Thursday"), sentence-case changes of common words ("the" -> "The")
//! and words under three letters. If more than half the inserted words changed, the text was
//! rewritten and nothing is returned.
//!
//! `can_auto_add` says whether a correction may go into the dictionary without asking: not when
//! the misheard words are everyday English, since the entry would rewrite them in every later take.

use similar::{capture_diff_slices, Algorithm, DiffTag};

pub fn find_corrections(inserted: &str, field_now: &str) -> Vec<(String, String)> {
    let a = words(inserted);
    let b = words(field_now);
    let n = a.len();
    if n == 0 || b.is_empty() {
        return Vec::new();
    }
    let Some((lo, hi)) = best_window(&a, &b) else {
        return Vec::new();
    };
    let window = &b[lo..hi];
    let ops = capture_diff_slices(Algorithm::Myers, &a, window);

    // A rewrite, not a correction: too many of the inserted words changed.
    let changed: usize = ops.iter().filter(|op| op.tag() != DiffTag::Equal && op.tag() != DiffTag::Insert).map(|op| op.old_range().len()).sum();
    if changed * 2 > n {
        return Vec::new();
    }

    let mut out: Vec<(String, String)> = Vec::new();
    for op in &ops {
        let (tag, old, new) = op.as_tag_tuple();
        if tag != DiffTag::Replace || old.is_empty() || new.is_empty() || old.len() > 3 || new.len() > 3 {
            continue;
        }
        let from = a[old].join(" ");
        let to = window[new].join(" ");
        if is_spelling_fix(&from, &to) && !out.iter().any(|(f, t)| *f == from && *t == to) {
            out.push((from, to));
            if out.len() == 3 {
                break;
            }
        }
    }
    out
}

/// Whether a correction seen often enough may join the dictionary without asking. Not when every
/// misheard word is everyday English ("quickly" -> "Quikly", "their" -> "Thea"): the entry
/// would rewrite that word in everything said from then on, so it stays a suggestion to accept.
pub fn can_auto_add(from: &str) -> bool {
    let misheard = words(from);
    !misheard.is_empty() && !misheard.iter().all(|w| COMMON.contains(&w.to_lowercase().as_str()))
}

/// Words with surrounding punctuation removed ("Tuesday," -> "Tuesday"); inner apostrophes,
/// hyphens and dots stay ("Quikly's", "e.g").
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The field words [lo, hi) that best match the inserted words, or None when fewer than 40% of the
/// inserted words can be found there.
fn best_window(a: &[String], b: &[String]) -> Option<(usize, usize)> {
    let n = a.len();
    let la: Vec<String> = a.iter().map(|w| w.to_lowercase()).collect();
    let lb: Vec<String> = b.iter().map(|w| w.to_lowercase()).collect();
    let ops = capture_diff_slices(Algorithm::Patience, &la, &lb);
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for op in &ops {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            pairs.extend(old.zip(new));
        }
    }
    let needed = (n * 2).div_ceil(5); // at least 40% of the words
    if pairs.len() < needed.max(1) {
        return None;
    }
    // Densest run of matches whose field span is no longer than the insertion plus slack.
    let span = n + n / 2 + 3;
    let (mut best_start, mut best_end, mut best_count) = (0usize, 0usize, 0usize);
    let mut s = 0;
    for e in 0..pairs.len() {
        while pairs[e].1 - pairs[s].1 >= span {
            s += 1;
        }
        let count = e - s + 1;
        if count > best_count || (count == best_count && pairs[e].1 > pairs[best_end].1) {
            best_start = s;
            best_end = e;
            best_count = count;
        }
    }
    if best_count < needed.max(1) {
        return None;
    }
    let (first_a, first_b) = pairs[best_start];
    let (last_a, last_b) = pairs[best_end];
    // Widen so corrected words at either edge of the insertion are inside the window.
    let lo = first_b.saturating_sub(first_a);
    let hi = (last_b + (n - 1 - last_a) + 1).min(b.len());
    Some((lo, hi))
}

fn is_spelling_fix(from: &str, to: &str) -> bool {
    if from.chars().any(|c| c.is_ascii_digit()) || to.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let (fs, ts) = (squash(from), squash(to));
    if fs == ts {
        return false; // punctuation only ("its" -> "it's", "e-mail" -> "email")
    }
    let (fl, tl) = (fs.to_lowercase(), ts.to_lowercase());
    if fl.chars().count() < 3 || tl.chars().count() < 3 {
        return false;
    }
    if fl == tl {
        // Capitals or spacing only. "fig ma" -> "Figma" and "tori" -> "Tori" count;
        // "the" -> "The" (a new sentence) and "The" -> "the" do not.
        let from_single = !from.contains(' ');
        let lowered = to.chars().all(|c| !c.is_uppercase());
        if lowered || (from_single && COMMON.contains(&fl.as_str())) {
            return false;
        }
        return true;
    }
    let from_l = from.to_lowercase();
    let to_l = to.to_lowercase();
    if COMMON.contains(&from_l.as_str()) && COMMON.contains(&to_l.as_str()) {
        return false; // grammar swaps between real words
    }
    if is_inflection(&fl, &tl) {
        return false;
    }
    if is_opposite(&fl, &tl) || same_closed_set(&fl, &tl) {
        return false; // a change of meaning that happens to look alike
    }
    strsim::normalized_levenshtein(&from_l, &to_l) >= 0.5 || strsim::normalized_levenshtein(&fl, &tl) >= 0.5
}

/// Opposites, on lower-case letters only: a negating prefix or suffix ("possible" / "impossible",
/// "are" / "arent", "can" / "cannot"), or swapped prefixes or suffixes on the same stem ("include" /
/// "exclude", "upload" / "download", "careful" / "careless").
fn is_opposite(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if long.strip_prefix(short).is_some_and(|s| s == "nt" || s == "not" || (short == "can" && s == "t")) {
        return true;
    }
    if long.strip_suffix(short).is_some_and(|p| ["un", "in", "im", "il", "ir", "dis", "non", "mis", "anti", "de"].contains(&p)) {
        return true;
    }
    if [("will", "wont"), ("shall", "shant")].iter().any(|&(x, y)| (a == x && b == y) || (a == y && b == x)) {
        return true;
    }
    const PREFIXES: &[(&str, &str)] = &[("in", "ex"), ("im", "ex"), ("in", "de"), ("in", "out"), ("up", "down"), ("over", "under"), ("pre", "post"), ("max", "min")];
    const SUFFIXES: &[(&str, &str)] = &[("ful", "less")];
    let same_stem = |ra: Option<&str>, rb: Option<&str>| matches!((ra, rb), (Some(x), Some(y)) if x == y && x.len() >= 3);
    PREFIXES.iter().any(|&(p, q)| same_stem(a.strip_prefix(p), b.strip_prefix(q)) || same_stem(a.strip_prefix(q), b.strip_prefix(p)))
        || SUFFIXES.iter().any(|&(p, q)| same_stem(a.strip_suffix(p), b.strip_suffix(q)) || same_stem(a.strip_suffix(q), b.strip_suffix(p)))
}

/// Two members of the same closed set ("tuesday" / "thursday", "june" / "july", "thirteen" /
/// "thirty"): a different fact, not a spelling. Plurals count ("tuesdays").
fn same_closed_set(a: &str, b: &str) -> bool {
    const SETS: &[&[&str]] = &[
        &["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"],
        &["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"],
        &[
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen",
            "fifteen", "sixteen", "seventeen", "eighteen", "nineteen", "twenty", "thirty", "forty", "fifty", "sixty", "seventy",
            "eighty", "ninety", "hundred", "thousand", "million", "billion", "first", "second", "third", "fourth", "fifth", "sixth",
            "seventh", "eighth", "ninth", "tenth",
        ],
        &["north", "south", "east", "west", "northern", "southern", "eastern", "western"],
    ];
    let member = |set: &[&str], w: &str| set.contains(&w) || w.strip_suffix('s').is_some_and(|base| set.contains(&base));
    SETS.iter().any(|set| member(set, a) && member(set, b))
}

fn is_inflection(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    match long.strip_prefix(short) {
        Some(suffix) => ["s", "es", "ed", "d", "ing", "er", "ers", "ly", "r", "st", "est"].contains(&suffix),
        None => {
            // "make" -> "making", "try" -> "tried"
            let stem: String = short.chars().take(short.chars().count().saturating_sub(1)).collect();
            stem.chars().count() >= 3 && long.strip_prefix(stem.as_str()).is_some_and(|s| ["ing", "ed", "ies", "ied", "er", "est"].contains(&s))
        }
    }
}

/// Very common English words and homophones. A change between two of these is grammar, not a name.
const COMMON: &[&str] = &[
    "the", "be", "to", "of", "and", "a", "in", "that", "have", "i", "it", "for", "not", "on", "with", "he", "as", "you",
    "do", "at", "this", "but", "his", "by", "from", "they", "we", "say", "her", "she", "or", "an", "will", "my", "one",
    "all", "would", "there", "their", "what", "so", "up", "out", "if", "about", "who", "get", "which", "go", "me",
    "when", "make", "can", "like", "time", "no", "just", "him", "know", "take", "people", "into", "year", "your",
    "good", "some", "could", "them", "see", "other", "than", "then", "now", "look", "only", "come", "its", "over",
    "think", "also", "back", "after", "use", "two", "how", "our", "work", "first", "well", "way", "even", "new",
    "want", "because", "any", "these", "give", "day", "most", "us", "is", "are", "was", "were", "been", "has", "had",
    "did", "does", "said", "made", "went", "got", "may", "might", "must", "should", "here", "where", "why", "very",
    "much", "more", "many", "such", "own", "same", "few", "those", "through", "down", "off", "again", "once", "both",
    "each", "too", "can't", "won't", "don't", "isn't", "wasn't", "weren't", "hasn't", "haven't", "hadn't", "doesn't",
    "didn't", "couldn't", "shouldn't", "wouldn't", "it's", "they're", "we're", "wear", "you're", "whose", "who's",
    "affect", "effect", "accept", "except", "lose", "loose", "quiet", "quite", "advice", "advise", "weather", "whether",
    "principal", "principle", "complement", "compliment", "stationary", "stationery", "break", "brake", "buy", "bye",
    "cite", "site", "sight", "hear", "knew", "right", "write", "rite", "piece", "peace", "plain", "plane", "sea",
    "sole", "soul", "son", "sun", "tail", "tale", "wait", "weight", "weigh", "week", "weak", "whole", "hole", "wood",
    "allowed", "aloud", "bare", "bear", "board", "bored", "cell", "sell", "cent", "scent", "sent", "dear", "deer",
    "die", "dye", "fair", "fare", "flour", "flower", "four", "fore", "great", "grate", "hair", "hare", "heal", "heel",
    "higher", "hire", "hour", "night", "knight", "maid", "mail", "male", "meat", "meet", "morning", "mourning",
    "pair", "pear", "pale", "pail", "passed", "past", "patience", "patients", "poor", "pour", "pore", "pray", "prey",
    "rain", "reign", "rein", "read", "red", "reed", "role", "roll", "sail", "sale", "scene", "seen", "sew", "sow",
    "steal", "steel", "suite", "sweet", "threw", "tide", "tied", "toe", "tow", "vain", "vein", "waist", "waste",
    "ware", "warn", "worn", "witch", "loan", "lone", "lead", "led", "lessen", "lesson", "medal", "metal", "miner",
    "minor", "naval", "navel", "won", "pedal", "peddle", "profit", "prophet", "real", "reel", "rest", "ring", "wring",
    "road", "rode", "rowed", "root", "route", "rose", "rows", "sauce", "source", "seam", "seem", "serial", "cereal",
    "some", "sum", "stair", "stare", "stake", "steak", "story", "storey", "tea", "team", "tire", "tyre", "vary",
    "wail", "whale", "wine", "whine", "access", "excess", "adapt", "adopt", "addition", "edition", "breath",
    "breathe", "capital", "censor", "sensor", "clothes", "cloths", "coarse", "course", "conscience", "conscious",
    "council", "counsel", "decent", "descent", "desert", "dessert", "device", "devise", "elicit", "illicit",
    "envelop", "envelope", "everyday", "every", "farther", "further", "formally", "formerly", "forth", "fourth",
    "human", "humane", "later", "latter", "lay", "lie", "lightning", "lightening", "moral", "morale", "personal",
    "personnel", "precede", "proceed", "than", "thorough", "though", "thought", "tough", "trough", "were", "where",
    "we'll", "will", "wheel", "while", "wile", "about", "above", "across", "act", "add", "age", "ago", "agree",
    "air", "allow", "almost", "alone", "along", "already", "although", "always", "among", "amount", "answer",
    "anyone", "anything", "appear", "apply", "area", "arm", "around", "arrive", "art", "ask", "away", "baby", "bad",
    "bag", "ball", "bank", "base", "bed", "before", "begin", "behind", "believe", "below", "best", "better",
    "between", "big", "bill", "bit", "black", "blood", "blue", "body", "book", "born", "box", "boy", "bring",
    "brother", "build", "business", "busy", "call", "car", "care", "carry", "case", "cause", "centre", "center",
    "chair", "chance", "change", "child", "choose", "city", "class", "clear", "close", "cold", "colour", "color",
    "common", "company", "complete", "computer", "consider", "continue", "control", "cost", "country", "cover",
    "create", "cut", "dark", "data", "date", "daughter", "dead", "deal", "death", "decide", "deep", "design",
    "detail", "develop", "different", "difficult", "dinner", "direction", "discuss", "doctor", "dog", "door",
    "draw", "dream", "dress", "drink", "drive", "drop", "during", "early", "east", "easy", "eat", "edge", "eight",
    "either", "else", "end", "enjoy", "enough", "enter", "entire", "event", "ever", "everyone", "everything",
    "exactly", "example", "expect", "explain", "eye", "face", "fact", "fail", "fall", "family", "far", "fast",
    "father", "fear", "feel", "field", "fight", "figure", "file", "fill", "film", "final", "find", "fine", "finger",
    "finish", "fire", "firm", "fish", "five", "floor", "fly", "follow", "food", "foot", "force", "forget", "form",
    "forward", "free", "friend", "front", "full", "fun", "future", "game", "garden", "general", "girl", "glass",
    "goal", "gold", "gone", "green", "ground", "group", "grow", "guess", "guy", "half", "hand", "hang", "happen",
    "happy", "hard", "head", "health", "heart", "heat", "heavy", "help", "high", "history", "hit", "hold", "home",
    "hope", "hot", "house", "huge", "idea", "image", "imagine", "important", "include", "increase", "inside",
    "instead", "interest", "issue", "item", "job", "join", "keep", "key", "kid", "kind", "king", "land", "language",
    "large", "last", "late", "laugh", "law", "learn", "least", "leave", "left", "leg", "less", "let", "letter",
    "level", "life", "light", "likely", "line", "list", "listen", "little", "live", "local", "long", "lot", "love",
    "low", "main", "major", "man", "manage", "market", "matter", "mean", "measure", "member", "memory", "mention",
    "message", "method", "middle", "mind", "minute", "miss", "model", "moment", "money", "month", "mother", "move",
    "movie", "music", "name", "navigate", "near", "need", "never", "news", "next", "nice", "none", "north", "note",
    "nothing", "notice", "number", "offer", "office", "often", "old", "open", "order", "others", "outside", "page",
    "pain", "paper", "parent", "part", "party", "pass", "pay", "person", "phone", "pick", "picture", "place", "plan",
    "plant", "play", "point", "policy", "popular", "position", "possible", "power", "present", "pretty", "price",
    "probably", "problem", "process", "produce", "product", "program", "project", "provide", "public", "pull",
    "purpose", "push", "question", "quickly", "race", "raise", "range", "rate", "rather", "reach", "ready", "reason",
    "receive", "recent", "record", "remain", "remember", "report", "result", "return", "rich", "rise", "risk",
    "rock", "room", "rule", "run", "safe", "save", "school", "score", "seat", "second", "section", "send", "sense",
    "series", "serious", "serve", "service", "seven", "several", "share", "short", "shot", "show", "side", "sign",
    "simple", "since", "sing", "single", "sister", "sit", "six", "size", "skill", "small", "smile", "social",
    "something", "sometimes", "song", "soon", "sort", "sound", "south", "space", "speak", "special", "spend",
    "sport", "spring", "staff", "stage", "stand", "standard", "star", "start", "state", "stay", "step", "still",
    "stop", "store", "street", "strong", "student", "study", "style", "subject", "success", "summer", "support",
    "sure", "system", "table", "talk", "task", "teach", "teacher", "term", "test", "thank", "thing", "thousand",
    "three", "throw", "today", "together", "tonight", "top", "total", "toward", "towards", "town", "trade",
    "travel", "tree", "trial", "trip", "trouble", "true", "truth", "try", "turn", "type", "under", "understand",
    "unit", "until", "usually", "value", "view", "visit", "voice", "vote", "wall", "war", "watch", "water", "west",
    "white", "wide", "wife", "win", "window", "wish", "woman", "wonder", "word", "worker", "world", "worry",
    "wrong", "yard", "yeah", "yes", "yet", "young", "with", "without", "within", "upon", "onto", "via", "thanks",
    "hello", "sorry", "please", "okay", "cheers", "great", "good", "bad", "nice",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn learns_real_corrections() {
        assert_eq!(
            find_corrections("I've been working with quickly on the new site.", "I've been working with Quikly on the new site."),
            vec![pair("quickly", "Quikly")]
        );
        assert_eq!(find_corrections("Can you send it to tory today?", "Can you send it to Tori today?"), vec![pair("tory", "Tori")]);
        assert_eq!(find_corrections("Have a look at fig ma tomorrow.", "Have a look at Figma tomorrow."), vec![pair("fig ma", "Figma")]);
        assert_eq!(find_corrections("I asked clawed code to fix it.", "I asked Claude Code to fix it."), vec![pair("clawed code", "Claude Code")]);
    }

    #[test]
    fn finds_the_insertion_among_other_text() {
        let inserted = "Thanks for the update, I'll ask tory about it.";
        let field = "Hi Sam,\n\nGood to hear from you. Thanks for the update, I'll ask Tori about it.\n\nSpeak soon,\nAlex";
        assert_eq!(find_corrections(inserted, field), vec![pair("tory", "Tori")]);
        // The same words elsewhere in the field do not confuse the window.
        let field2 = "the the the update. Thanks for the update, I'll ask Tori about it. More text the user typed afterwards here.";
        assert_eq!(find_corrections(inserted, field2), vec![pair("tory", "Tori")]);
    }

    #[test]
    fn corrected_first_and_last_words_are_found() {
        assert_eq!(find_corrections("tory said yes to the plan", "Tori said yes to the plan"), vec![pair("tory", "Tori")]);
        assert_eq!(find_corrections("the plan was approved by tory", "the plan was approved by Tori"), vec![pair("tory", "Tori")]);
    }

    #[test]
    fn ignores_non_corrections() {
        // Whole sentence rewritten.
        assert!(find_corrections("Let's meet on Tuesday to talk about the budget.", "Could we chat later this week instead?").is_empty());
        // A clause rewritten.
        assert!(find_corrections("I think we should go with the blue design for now.", "I think we should wait until the client has seen all three options.").is_empty());
        // Deletions only.
        assert!(find_corrections("This is really very good news for us.", "This is good news for us.").is_empty());
        // Punctuation only.
        assert!(find_corrections("its done, thanks", "it's done. Thanks!").is_empty());
        // Text moved on: nothing like the insertion is there any more.
        assert!(find_corrections("Please send me the invoice by Friday.", "Totally different content now, nothing in common at all.").is_empty());
        // Empty inputs.
        assert!(find_corrections("", "anything").is_empty());
        assert!(find_corrections("anything", "").is_empty());
    }

    #[test]
    fn ignores_harmful_entries() {
        assert!(find_corrections("We launch in 2025 as planned.", "We launch in 2026 as planned.").is_empty());
        assert!(find_corrections("It's better then the old one.", "It's better than the old one.").is_empty());
        assert!(find_corrections("I left it over their by the door.", "I left it over there by the door.").is_empty());
        assert!(find_corrections("Please call me when you land.", "Please calls me when you land.").is_empty());
        assert!(find_corrections("the meeting is at noon. the room is booked.", "the meeting is at noon. The room is booked.").is_empty());
        assert!(find_corrections("I went in the house.", "I went on the house.").is_empty());
        // A different word that merely sounds unrelated is a rewrite of that word.
        assert!(find_corrections("The results were good overall.", "The results were great overall.").is_empty());
    }

    #[test]
    fn opposites_and_swapped_facts_are_not_spellings() {
        for (said, fixed) in [
            ("They are coming tonight.", "They aren't coming tonight."),
            ("That is possible for us.", "That is impossible for us."),
            ("It seems likely to work.", "It seems unlikely to work."),
            ("I agree with the plan.", "I disagree with the plan."),
            ("Please include the VAT.", "Please exclude the VAT."),
            ("We will increase the price.", "We will decrease the price."),
            ("You can attend the session.", "You cannot attend the session."),
            ("Upload the file again.", "Download the file again."),
            ("Be careful with it.", "Be careless with it."),
            ("See you on Tuesday then.", "See you on Thursday then."),
            ("It starts in June this year.", "It starts in July this year."),
            ("There were thirteen of them.", "There were thirty of them."),
            ("Head east from the station.", "Head west from the station."),
        ] {
            assert!(find_corrections(said, fixed).is_empty(), "{said} -> {fixed}");
        }
    }

    #[test]
    fn everyday_words_are_never_added_without_asking() {
        assert!(can_auto_add("tory"));
        assert!(can_auto_add("clawed code"));
        assert!(can_auto_add("quick ly"));
        // The entry would rewrite these in every take, so they stay suggestions.
        assert!(!can_auto_add("quickly"));
        assert!(!can_auto_add("their"));
        assert!(!can_auto_add("you and"));
        assert!(!can_auto_add(""));
    }

    #[test]
    fn at_most_three_per_take() {
        let inserted = "alpha tory beta quickly gamma clawed delta jonn epsilon zeta eta theta iota kappa lambda mu nu xi";
        let field = "alpha Tori beta Quikly gamma Claude delta John epsilon zeta eta theta iota kappa lambda mu nu xi";
        let found = find_corrections(inserted, field);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0], pair("tory", "Tori"));
    }
}
