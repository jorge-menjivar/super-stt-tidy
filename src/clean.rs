// SPDX-License-Identifier: GPL-3.0-only
//! The cleanup rules.
//!
//! Pure, deterministic, allocation-light string transforms — the same family of
//! normalization applied to text corpora before training: fold the Unicode
//! punctuation a dictation engine emits down to ASCII, drop the vocalized
//! fillers, repair spacing, and restore sentence casing.
//!
//! Nothing here is a model. Every rule is a rule you can read, and the whole
//! module compiles for the host, so all of it is unit-tested natively.

use std::ops::Range;

/// Which rules run. Each maps to a `[[options]]` entry in `backend.toml`, read
/// off the `x-voice-option-*` headers the daemon injects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one switch per rule, mirroring the manifest's options one-for-one;               packing them into flags would break that correspondence for nothing"
)]
pub struct Rules {
    /// Fold Unicode punctuation to ASCII and strip invisibles.
    pub normalize_unicode: bool,
    /// Drop vocalized fillers ("um", "uh", …).
    pub remove_fillers: bool,
    /// Collapse an immediately repeated word ("the the" → "the").
    pub dedupe_words: bool,
    /// Repair spacing around punctuation.
    pub fix_punctuation: bool,
    /// Collapse whitespace runs and trim.
    pub collapse_whitespace: bool,
    /// Capitalize sentence starts and the standalone pronoun "i".
    pub sentence_case: bool,
    /// Write the user's vocabulary terms the way they spelled them.
    pub vocabulary_spelling: bool,
}

impl Default for Rules {
    /// Everything on. A post-processor the user has deliberately enabled is
    /// asked to clean up; each rule is individually switchable for the cases
    /// where one of them is unwelcome.
    fn default() -> Self {
        Self {
            normalize_unicode: true,
            remove_fillers: true,
            dedupe_words: true,
            fix_punctuation: true,
            collapse_whitespace: true,
            sentence_case: true,
            vocabulary_spelling: true,
        }
    }
}

/// Vocalized fillers, matched case-insensitively against a token's letters.
///
/// Deliberately only sounds, never words that carry meaning. "like", "so", and
/// "you know" are fillers in speech and words in sentences, and no rule here can
/// tell which one the speaker meant — removing them would silently rewrite what
/// the user said.
const FILLERS: &[&str] = &[
    "um", "umm", "ummm", "uh", "uhh", "uhhh", "uhm", "erm", "er", "ah", "ahh", "eh", "hmm", "hm",
    "mhm", "mm", "mmm",
];

/// Words that legitimately double in English, exempted from [`dedupe_words`].
///
/// "I had had enough" and "the thing that that broke" are grammatical. Dictation
/// stutter is far more common than either, so the rule stays on by default and
/// carries this exception list rather than being off by default.
const LEGITIMATE_DOUBLES: &[&str] = &["had", "that", "is", "was", "no"];

/// Run the enabled rules over `text`, in the order they have to run in.
///
/// `vocabulary` is the user's: terms spelled the way they want them, from
/// their dictation context. The token rules never cut a term out of it, and
/// [`respell_vocabulary`] writes terms in the user's spelling.
///
/// Order matters and is not configurable: Unicode folding first so later rules
/// see ASCII punctuation; token rules next while whitespace still separates
/// tokens; spacing repair after, since removing a token can strand a comma;
/// whitespace collapse after that, to absorb what the earlier rules left;
/// casing after that, so it sees final sentence boundaries; and the user's
/// spellings last, so "iPhone" survives starting a sentence.
#[must_use]
pub fn clean(text: &str, rules: &Rules, vocabulary: &[String]) -> String {
    let mut out = text.to_string();
    if rules.normalize_unicode {
        out = normalize_unicode(&out);
    }
    if rules.remove_fillers {
        out = remove_fillers(&out, vocabulary);
    }
    if rules.dedupe_words {
        out = dedupe_words(&out, vocabulary);
    }
    if rules.fix_punctuation {
        out = fix_punctuation(&out);
    }
    if rules.collapse_whitespace {
        out = collapse_whitespace(&out);
    }
    if rules.sentence_case {
        out = sentence_case(&out);
    }
    if rules.vocabulary_spelling {
        out = respell_vocabulary(&out, vocabulary);
    }
    out
}

/// Fold the Unicode punctuation dictation engines emit down to ASCII, and drop
/// invisible characters outright.
///
/// Smart quotes and dashes are what make otherwise-identical transcripts
/// compare unequal; zero-width characters and stray control bytes are invisible
/// and survive every other rule.
#[must_use]
pub fn normalize_unicode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            // Single quotes / apostrophes.
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => out.push('\''),
            // Double quotes.
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => out.push('"'),
            // Dashes: en, em, figure, minus, non-breaking hyphen.
            '\u{2010}'..='\u{2015}' | '\u{2212}' => out.push('-'),
            // Ellipsis, spelled out so the spacing rules can see the dots.
            '\u{2026}' => out.push_str("..."),
            // Non-breaking and typographic spaces.
            '\u{00A0}' | '\u{2007}' | '\u{2009}' | '\u{202F}' | '\u{205F}' => out.push(' '),
            // Zero-width characters and the BOM: invisible, so they are dropped
            // rather than folded to anything.
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' => {}
            // Other C0/C1 controls, except the whitespace the collapse rule
            // handles.
            c if c.is_control() && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

/// The letters of a token, lowercased, with surrounding punctuation removed.
///
/// The apostrophe stays so "don't" survives as one word; everything else that
/// is not alphanumeric is stripped, which is what turns "um," into "um".
fn core_word(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Where the user's vocabulary terms occur among `tokens`, as token ranges.
///
/// Words are compared the way the token rules see them, through [`core_word`],
/// so neither case nor the punctuation stuck to a token hides a term.
fn term_spans(tokens: &[&str], vocabulary: &[String]) -> Vec<Range<usize>> {
    let cores: Vec<String> = tokens.iter().map(|token| core_word(token)).collect();
    let mut spans = Vec::new();
    for term in vocabulary {
        let words: Vec<String> = term.split_whitespace().map(core_word).collect();
        // A term with nothing a token rule could match, "" or "--", names no
        // words to keep.
        if words.is_empty() || words.iter().any(String::is_empty) {
            continue;
        }
        for (start, window) in cores.windows(words.len()).enumerate() {
            if window == words.as_slice() {
                spans.push(start..start + words.len());
            }
        }
    }
    spans
}

/// The sentence-ending punctuation a token carries, if any.
fn terminal_punctuation(token: &str) -> Option<char> {
    token.chars().rev().find(|c| matches!(c, '.' | '!' | '?'))
}

/// Drop tokens that are nothing but a vocalized filler.
///
/// A dropped token's sentence-ending punctuation moves onto the previous word,
/// so "I think, um. We should" does not lose the full stop that ended the
/// sentence.
///
/// A token inside one of the user's `vocabulary` terms stays, filler or not:
/// "Ah" in "Ah Puch" is a name the user said.
#[must_use]
pub fn remove_fillers(text: &str, vocabulary: &[String]) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let spans = term_spans(&tokens, vocabulary);
    let mut out: Vec<String> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let core = core_word(token);
        let in_term = spans.iter().any(|span| span.contains(&i));
        if in_term || core.is_empty() || !FILLERS.contains(&core.as_str()) {
            out.push((*token).to_string());
            continue;
        }
        if let Some(mark) = terminal_punctuation(token)
            && let Some(previous) = out.last_mut()
            && !previous.ends_with(['.', '!', '?'])
        {
            previous.push(mark);
        }
    }
    out.join(" ")
}

/// Collapse an immediately repeated word to one.
///
/// When the repeat carries punctuation the first copy does not — "the the," —
/// the later token wins, so the comma is not dropped along with the stutter.
///
/// A double inside one of the user's `vocabulary` terms is said on purpose,
/// "Bora Bora", and stays. A term does not protect its own stutter: with
/// "Rust" in the vocabulary, "Rust Rust" is still one word too many.
#[must_use]
pub fn dedupe_words(text: &str, vocabulary: &[String]) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let spans = term_spans(&tokens, vocabulary);
    let mut out: Vec<String> = Vec::new();
    let mut previous = String::new();
    for (i, token) in tokens.iter().enumerate() {
        let core = core_word(token);
        let doubled_in_term = spans.iter().any(|span| span.start < i && i < span.end);
        let repeats = !core.is_empty()
            && core == previous
            && !LEGITIMATE_DOUBLES.contains(&core.as_str())
            && !doubled_in_term;
        if repeats {
            // Keep whichever copy carries punctuation; otherwise drop this one.
            if token.chars().any(|c| !c.is_alphanumeric() && c != '\'')
                && let Some(last) = out.last_mut()
            {
                *last = (*token).to_string();
            }
            continue;
        }
        previous = core;
        out.push((*token).to_string());
    }
    out.join(" ")
}

/// Repair spacing around punctuation, and collapse punctuation pile-ups.
///
/// Removing a filler can stranded a comma against the preceding space ("I think
/// , we should"); dictation also emits doubled commas and runs of dots.
#[must_use]
pub fn fix_punctuation(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out: String = String::with_capacity(text.len());
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        // A run of the same mark collapses: `,,` → `,`, and two or more dots
        // become an ellipsis (which is what the speaker's pause meant).
        if matches!(ch, ',' | ';' | ':' | '.' | '!' | '?') {
            let mut run = 1;
            while i + run < chars.len() && chars[i + run] == ch {
                run += 1;
            }
            // Drop any space sitting between the previous word and this mark.
            while out.ends_with(' ') {
                out.pop();
            }
            if ch == '.' && run >= 2 {
                out.push_str("...");
            } else {
                out.push(ch);
            }
            i += run;
            // Exactly one space after the mark when a word follows it.
            if let Some(next) = chars.get(i)
                && !next.is_whitespace()
                && !matches!(
                    next,
                    ',' | ';' | ':' | '.' | '!' | '?' | ')' | ']' | '"' | '\''
                )
            {
                out.push(' ');
            }
            continue;
        }

        // No space inside brackets: "( like this )" → "(like this)".
        if matches!(ch, ')' | ']') {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push(ch);
            i += 1;
            continue;
        }
        if matches!(ch, '(' | '[') {
            out.push(ch);
            i += 1;
            while matches!(chars.get(i), Some(c) if c.is_whitespace()) {
                i += 1;
            }
            continue;
        }

        out.push(ch);
        i += 1;
    }
    out
}

/// Collapse every whitespace run to a single space and trim the ends.
#[must_use]
pub fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Capitalize the first letter of each sentence, and the standalone pronoun
/// "i".
///
/// Only the sentence-initial letter is touched: a transcript's other casing is
/// the engine's, and a rule that recased more would mangle names and acronyms.
#[must_use]
pub fn sentence_case(text: &str) -> String {
    let cased = capitalize_sentences(text);
    capitalize_lone_i(&cased)
}

fn capitalize_sentences(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at_sentence_start = true;
    for (i, &ch) in chars.iter().enumerate() {
        if at_sentence_start && ch.is_alphabetic() {
            out.extend(ch.to_uppercase());
            at_sentence_start = false;
            continue;
        }
        out.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            // A terminator ends a sentence only when whitespace (or nothing)
            // follows it. The dot inside "i.e." or "3.5" binds to what comes
            // next, and treating it as a boundary would recase the abbreviation
            // into "i.E." and the decimal into something equally wrong.
            at_sentence_start = chars.get(i + 1).is_none_or(|c| c.is_whitespace());
        }
    }
    out
}

/// `i` → `I`, but only as a whole word — "i think" becomes "I think" while
/// "i.e." and "iPhone" are left alone.
fn capitalize_lone_i(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for token in text.split(' ') {
        if token == "i" || token.starts_with("i'") {
            let mut fixed = String::from("I");
            fixed.push_str(&token[1..]);
            out.push(fixed);
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
}

/// Write the user's vocabulary terms the way they spelled them, where the
/// text has them in other letter case: "iphone" becomes "iPhone".
///
/// Only the case of letters already there changes, so no word is added,
/// dropped or swapped. A term matches whole words only, and the words of a
/// term may be separated by any whitespace. Only a term whose spelling could
/// not be an ordinary word's is applied; see [`carries_spelling`].
#[must_use]
pub fn respell_vocabulary(text: &str, vocabulary: &[String]) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    for term in vocabulary.iter().filter(|term| carries_spelling(term)) {
        let words: Vec<Vec<char>> = term
            .split_whitespace()
            .map(|word| word.chars().collect())
            .collect();
        let mut at = 0;
        while at < chars.len() {
            match match_term(&chars, at, &words) {
                Some((spelled, end)) => {
                    for (i, ch) in spelled {
                        chars[i] = ch;
                    }
                    at = end;
                }
                None => at += 1,
            }
        }
    }
    chars.into_iter().collect()
}

/// Whether the text should be brought round to a term's spelling.
///
/// Yes for a term of several words with a capital in it ("Super Voice"), and
/// for a word with a capital past its first letter and a lowercase letter
/// somewhere ("iPhone", "GitHub", "macOS"). No for a single word that is all
/// capitals, all lowercase or merely capitalized: "IT", "rebase" and "Go" are
/// ordinary words too, and applying them would recase every "it", every
/// "Rebase" starting a sentence and every "go".
fn carries_spelling(term: &str) -> bool {
    match term.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] => false,
        [word] => {
            word.chars().skip(1).any(char::is_uppercase) && word.chars().any(char::is_lowercase)
        }
        _ => term.chars().any(char::is_uppercase),
    }
}

/// Match `words` at `start` in `chars`, ignoring case. Returns each matched
/// position with the letter the term spells there, and where the match ends.
fn match_term(
    chars: &[char],
    start: usize,
    words: &[Vec<char>],
) -> Option<(Vec<(usize, char)>, usize)> {
    if start > 0 && chars[start - 1].is_alphanumeric() {
        return None;
    }
    let mut at = start;
    let mut spelled = Vec::new();
    for (k, word) in words.iter().enumerate() {
        if k > 0 {
            let gap = at;
            while chars.get(at).is_some_and(|c| c.is_whitespace()) {
                at += 1;
            }
            if at == gap {
                return None;
            }
        }
        for &want in word {
            let have = *chars.get(at)?;
            if have != want && !have.to_lowercase().eq(want.to_lowercase()) {
                return None;
            }
            spelled.push((at, want));
            at += 1;
        }
    }
    if chars.get(at).is_some_and(|c| c.is_alphanumeric()) {
        return None;
    }
    Some((spelled, at))
}

#[cfg(test)]
mod tests {
    use super::{
        Rules, clean, collapse_whitespace, dedupe_words, fix_punctuation, normalize_unicode,
        remove_fillers, respell_vocabulary, sentence_case,
    };

    fn vocabulary(terms: &[&str]) -> Vec<String> {
        terms.iter().map(ToString::to_string).collect()
    }

    // ── unicode ────────────────────────────────────────────────────────────

    #[test]
    fn smart_punctuation_folds_to_ascii() {
        assert_eq!(
            normalize_unicode("\u{201C}don\u{2019}t\u{201D} \u{2014} really\u{2026}"),
            "\"don't\" - really..."
        );
    }

    /// Invisible characters survive every other rule, so they are dropped here
    /// or not at all.
    #[test]
    fn invisible_characters_are_dropped() {
        assert_eq!(
            normalize_unicode("he\u{200B}llo\u{FEFF} wor\u{200D}ld"),
            "hello world"
        );
        assert_eq!(normalize_unicode("a\u{0007}b"), "ab");
    }

    /// A non-breaking space is a space, not an invisible: folding it to one is
    /// what lets the whitespace rule see it.
    #[test]
    fn exotic_spaces_become_ordinary_ones() {
        assert_eq!(normalize_unicode("a\u{00A0}b\u{202F}c"), "a b c");
    }

    /// Ordinary text is left exactly as it is — including non-Latin scripts,
    /// which none of these rules has any business rewriting.
    #[test]
    fn ordinary_text_is_untouched() {
        assert_eq!(normalize_unicode("Hello, world."), "Hello, world.");
        assert_eq!(normalize_unicode("¿Dónde está?"), "¿Dónde está?");
        assert_eq!(normalize_unicode("日本語のテキスト"), "日本語のテキスト");
    }

    // ── fillers ────────────────────────────────────────────────────────────

    #[test]
    fn vocalized_fillers_are_removed() {
        assert_eq!(
            remove_fillers("um so uh I think erm we should ship it", &[]),
            "so I think we should ship it"
        );
    }

    #[test]
    fn a_filler_with_a_comma_takes_the_comma_with_it() {
        assert_eq!(
            remove_fillers("I think, um, we should", &[]),
            "I think, we should"
        );
    }

    /// Dropping the token must not drop the sentence: the full stop moves back
    /// onto the previous word.
    #[test]
    fn a_filler_ending_a_sentence_leaves_the_full_stop_behind() {
        assert_eq!(
            remove_fillers("that is all um. Next thing", &[]),
            "that is all. Next thing"
        );
    }

    /// The rule matches sounds, not words. Anything that could be a real word
    /// is left alone — a cleanup that silently edits meaning is worse than no
    /// cleanup.
    #[test]
    fn words_that_merely_look_like_fillers_survive() {
        for text in [
            "I like it",
            "so we ship",
            "you know the answer",
            "a hum in the room",
            "the ahead of us",
        ] {
            assert_eq!(remove_fillers(text, &[]), text, "rewrote: {text}");
        }
    }

    /// Case and trailing punctuation do not hide a filler.
    #[test]
    fn fillers_are_matched_regardless_of_case_or_punctuation() {
        assert_eq!(remove_fillers("Um, hello", &[]), "hello");
        assert_eq!(remove_fillers("HMM hello", &[]), "hello");
    }

    // ── dedupe ─────────────────────────────────────────────────────────────

    #[test]
    fn a_stuttered_word_collapses() {
        assert_eq!(dedupe_words("the the cat sat", &[]), "the cat sat");
        assert_eq!(dedupe_words("I I think", &[]), "I think");
    }

    /// English doubles some words legitimately; the rule must not "fix" them.
    #[test]
    fn legitimate_doubles_survive() {
        assert_eq!(dedupe_words("I had had enough", &[]), "I had had enough");
        assert_eq!(
            dedupe_words("the thing that that broke", &[]),
            "the thing that that broke"
        );
    }

    /// The copy carrying punctuation is the one that survives, so the comma is
    /// not collapsed away with the stutter.
    #[test]
    fn the_punctuated_copy_of_a_stutter_wins() {
        assert_eq!(dedupe_words("well well, no", &[]), "well, no");
    }

    #[test]
    fn non_adjacent_repeats_are_left_alone() {
        assert_eq!(
            dedupe_words("the cat and the dog", &[]),
            "the cat and the dog"
        );
    }

    // ── punctuation ────────────────────────────────────────────────────────

    #[test]
    fn spacing_around_punctuation_is_repaired() {
        assert_eq!(fix_punctuation("hello , world ."), "hello, world.");
        assert_eq!(fix_punctuation("wait.what"), "wait. what");
    }

    #[test]
    fn punctuation_pile_ups_collapse() {
        assert_eq!(fix_punctuation("hello,, world"), "hello, world");
        assert_eq!(fix_punctuation("wait.. what"), "wait... what");
    }

    /// An ellipsis is a pause the speaker made; it stays one.
    #[test]
    fn an_ellipsis_survives_as_an_ellipsis() {
        assert_eq!(fix_punctuation("well... maybe"), "well... maybe");
    }

    #[test]
    fn brackets_lose_their_inner_padding() {
        assert_eq!(fix_punctuation("a ( b ) c"), "a (b) c");
    }

    /// A dot with no space after it is not a sentence boundary, so neither an
    /// abbreviation nor a decimal gets recased.
    #[test]
    fn a_dot_inside_a_word_is_not_a_sentence_boundary() {
        assert_eq!(
            sentence_case("we shipped 3.5 today"),
            "We shipped 3.5 today"
        );
        assert_eq!(sentence_case("e.g. this one"), "E.g. This one");
    }

    #[test]
    fn a_closing_quote_after_a_stop_is_not_pushed_apart() {
        assert_eq!(
            fix_punctuation("he said \"go.\" then left"),
            "he said \"go.\" then left"
        );
    }

    // ── whitespace + casing ────────────────────────────────────────────────

    #[test]
    fn whitespace_runs_collapse_and_ends_are_trimmed() {
        assert_eq!(collapse_whitespace("  a   b \n\t c  "), "a b c");
    }

    #[test]
    fn sentences_are_capitalized() {
        assert_eq!(
            sentence_case("hello there. how are you? fine!"),
            "Hello there. How are you? Fine!"
        );
    }

    #[test]
    fn the_lone_pronoun_is_capitalized() {
        assert_eq!(
            sentence_case("so i think i'm right"),
            "So I think I'm right"
        );
    }

    /// Only a standalone "i" is the pronoun. Casing "iPhone" or "i.e." would be
    /// a rewrite, not a fix.
    #[test]
    fn other_words_starting_with_i_are_left_alone() {
        assert_eq!(sentence_case("the iPhone is here"), "The iPhone is here");
        assert_eq!(sentence_case("that is, i.e. this"), "That is, i.e. This");
    }

    /// Existing casing elsewhere in the sentence is the engine's, not this
    /// module's, business.
    #[test]
    fn interior_casing_is_preserved() {
        assert_eq!(sentence_case("we use NASA data"), "We use NASA data");
    }

    // ── the pipeline ───────────────────────────────────────────────────────

    /// The case the backend exists for: a raw dictation transcript in, a clean
    /// sentence out.
    #[test]
    fn a_raw_dictation_transcript_comes_out_clean() {
        let raw = "um so i i think that uh we should  ship it \u{2014} you know , today .";
        assert_eq!(
            clean(raw, &Rules::default(), &[]),
            "So I think that we should ship it - you know, today."
        );
    }

    #[test]
    fn every_rule_can_be_switched_off() {
        let raw = "um  the the thing ,  yes";
        let off = Rules {
            normalize_unicode: false,
            remove_fillers: false,
            dedupe_words: false,
            fix_punctuation: false,
            collapse_whitespace: false,
            sentence_case: false,
            vocabulary_spelling: false,
        };
        assert_eq!(clean(raw, &off, &[]), raw, "all rules off must be a no-op");
    }

    /// Rules are independent: switching one off leaves the others working.
    #[test]
    fn rules_are_individually_switchable() {
        let raw = "um the the thing";
        let keep_fillers = Rules {
            remove_fillers: false,
            ..Rules::default()
        };
        assert_eq!(clean(raw, &keep_fillers, &[]), "Um the thing");

        let keep_stutter = Rules {
            dedupe_words: false,
            ..Rules::default()
        };
        assert_eq!(clean(raw, &keep_stutter, &[]), "The the thing");
    }

    /// Cleaning something already clean must not change it. This is the
    /// property that keeps the backend from slowly degrading a transcript that
    /// passes through it twice.
    #[test]
    fn cleaning_is_idempotent() {
        for raw in [
            "um so i think, uh, we should ship it today.",
            "the the cat sat on  the mat",
            "\u{201C}hello\u{201D} \u{2014} she said\u{2026}",
            "wait.. what?? really",
        ] {
            let once = clean(raw, &Rules::default(), &[]);
            let twice = clean(&once, &Rules::default(), &[]);
            assert_eq!(once, twice, "second pass changed: {raw:?} -> {once:?}");
        }
    }

    /// A transcript of nothing but fillers cleans to nothing. The daemon reads
    /// an empty answer as "keep the raw text", so this is a safe outcome rather
    /// than a lost transcript.
    #[test]
    fn an_all_filler_transcript_cleans_to_nothing() {
        assert_eq!(clean("um uh hmm", &Rules::default(), &[]), "");
    }

    #[test]
    fn empty_and_whitespace_input_stay_empty() {
        assert_eq!(clean("", &Rules::default(), &[]), "");
        assert_eq!(clean("   \n ", &Rules::default(), &[]), "");
    }

    /// The rules must never panic on adversarial input; a post-processor that
    /// crashes takes the transcript with it.
    #[test]
    fn odd_input_does_not_panic() {
        for raw in [
            ",,,,",
            "...",
            "((((",
            "'",
            "\u{200B}\u{200B}",
            "a".repeat(10_000).as_str(),
            "é\u{0301}",
        ] {
            let _ = clean(raw, &Rules::default(), &[]);
        }
    }

    // ── the user's vocabulary ──────────────────────────────────────────────

    #[test]
    fn a_term_is_written_the_way_the_user_spelled_it() {
        let terms = vocabulary(&["iPhone", "GitHub", "CubeCL"]);
        assert_eq!(
            respell_vocabulary("my iphone, a github repo and IPHONE again", &terms),
            "my iPhone, a GitHub repo and iPhone again"
        );
        assert_eq!(
            respell_vocabulary("cubecl's kernels", &terms),
            "CubeCL's kernels"
        );
    }

    /// Only whole words: a term is not respelled inside a longer word.
    #[test]
    fn a_term_inside_a_longer_word_is_left_alone() {
        let terms = vocabulary(&["GitHub", "iOS"]);
        assert_eq!(
            respell_vocabulary("githubusercontent and bios", &terms),
            "githubusercontent and bios"
        );
    }

    #[test]
    fn a_term_of_several_words_is_matched_across_any_whitespace() {
        let terms = vocabulary(&["Super Voice"]);
        assert_eq!(
            respell_vocabulary("open super voice, then super\n  voice", &terms),
            "open Super Voice, then Super\n  Voice"
        );
        assert_eq!(respell_vocabulary("supervoice", &terms), "supervoice");
    }

    /// A single word that is all capitals, all lowercase or merely capitalized
    /// is an ordinary word too. Applying it would rewrite the user's sentences
    /// instead of their term.
    #[test]
    fn a_term_that_is_also_an_ordinary_word_recases_nothing() {
        let terms = vocabulary(&["Go", "IT", "rebase", "Rust", "main branch"]);
        for text in [
            "we go home and it works",
            "Rebase onto the main branch",
            "Main branch is green, rust never sleeps",
        ] {
            assert_eq!(respell_vocabulary(text, &terms), text, "rewrote: {text}");
        }
    }

    #[test]
    fn a_term_keeps_its_spelling_starting_a_sentence() {
        assert_eq!(
            clean(
                "iphone sales grew. so did macos.",
                &Rules::default(),
                &vocabulary(&["iPhone", "macOS"])
            ),
            "iPhone sales grew. So did macOS."
        );
    }

    /// The vocabulary names words the user said on purpose, so the token rules
    /// never cut one out.
    #[test]
    fn a_term_is_never_removed_as_a_filler() {
        assert_eq!(
            remove_fillers("um Ah Puch was a god", &vocabulary(&["Ah Puch"])),
            "Ah Puch was a god"
        );
        assert_eq!(
            remove_fillers("say hmm, uh, now", &vocabulary(&["hmm"])),
            "say hmm, now"
        );
    }

    #[test]
    fn a_term_that_doubles_a_word_is_not_a_stutter() {
        let terms = vocabulary(&["Bora Bora"]);
        assert_eq!(
            dedupe_words("we flew to Bora Bora", &terms),
            "we flew to Bora Bora"
        );
        assert_eq!(dedupe_words("the the trip", &terms), "the trip");
    }

    /// A term protects the double it spells, not a stutter on itself.
    #[test]
    fn a_stutter_on_a_term_still_collapses() {
        assert_eq!(
            dedupe_words("Rust Rust is fast", &vocabulary(&["Rust"])),
            "Rust is fast"
        );
    }

    #[test]
    fn a_dictated_transcript_comes_out_in_the_users_spelling() {
        let terms = vocabulary(&["iPhone", "GitHub", "Bora Bora"]);
        assert_eq!(
            clean(
                "um i pushed it to github from my iphone iphone in bora bora .",
                &Rules::default(),
                &terms
            ),
            "I pushed it to GitHub from my iPhone in Bora Bora."
        );
    }

    #[test]
    fn spelling_can_be_switched_off() {
        let rules = Rules {
            vocabulary_spelling: false,
            ..Rules::default()
        };
        assert_eq!(
            clean("my iphone", &rules, &vocabulary(&["iPhone"])),
            "My iphone"
        );
    }

    #[test]
    fn cleaning_with_a_vocabulary_is_idempotent() {
        let terms = vocabulary(&["iPhone", "Super Voice", "Bora Bora"]);
        for raw in [
            "iphone iphone in bora bora. super voice um works",
            "IPHONE. iPhone.",
        ] {
            let once = clean(raw, &Rules::default(), &terms);
            let twice = clean(&once, &Rules::default(), &terms);
            assert_eq!(once, twice, "second pass changed: {raw:?} -> {once:?}");
        }
    }

    /// Terms come from the user, so none of them may make a rule panic.
    #[test]
    fn odd_terms_do_not_panic() {
        let terms = vocabulary(&[
            "",
            " ",
            "--",
            ",",
            "\u{e9}\u{301}",
            "A B C D E F",
            "\u{130}stanbul",
        ]);
        for raw in [
            "",
            "a b c d e f",
            "-- , --",
            "istanbul \u{130}STANBUL",
            "\u{e9}\u{301}",
        ] {
            let _ = clean(raw, &Rules::default(), &terms);
        }
    }
}
