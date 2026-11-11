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
/// Order matters and is not configurable: Unicode folding first so later rules
/// see ASCII punctuation; token rules next while whitespace still separates
/// tokens; spacing repair after, since removing a token can strand a comma;
/// whitespace collapse after that, to absorb what the earlier rules left; and
/// casing last, so it sees final sentence boundaries.
#[must_use]
pub fn clean(text: &str, rules: &Rules) -> String {
    let mut out = text.to_string();
    if rules.normalize_unicode {
        out = normalize_unicode(&out);
    }
    if rules.remove_fillers {
        out = remove_fillers(&out);
    }
    if rules.dedupe_words {
        out = dedupe_words(&out);
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

/// The sentence-ending punctuation a token carries, if any.
fn terminal_punctuation(token: &str) -> Option<char> {
    token.chars().rev().find(|c| matches!(c, '.' | '!' | '?'))
}

/// Drop tokens that are nothing but a vocalized filler.
///
/// A dropped token's sentence-ending punctuation moves onto the previous word,
/// so "I think, um. We should" does not lose the full stop that ended the
/// sentence.
#[must_use]
pub fn remove_fillers(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        let core = core_word(token);
        if core.is_empty() || !FILLERS.contains(&core.as_str()) {
            out.push(token.to_string());
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
#[must_use]
pub fn dedupe_words(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut previous = String::new();
    for token in text.split_whitespace() {
        let core = core_word(token);
        let repeats =
            !core.is_empty() && core == previous && !LEGITIMATE_DOUBLES.contains(&core.as_str());
        if repeats {
            // Keep whichever copy carries punctuation; otherwise drop this one.
            if token.chars().any(|c| !c.is_alphanumeric() && c != '\'')
                && let Some(last) = out.last_mut()
            {
                *last = token.to_string();
            }
            continue;
        }
        previous = core;
        out.push(token.to_string());
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

#[cfg(test)]
mod tests {
    use super::{
        Rules, clean, collapse_whitespace, dedupe_words, fix_punctuation, normalize_unicode,
        remove_fillers, sentence_case,
    };

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
            remove_fillers("um so uh I think erm we should ship it"),
            "so I think we should ship it"
        );
    }

    #[test]
    fn a_filler_with_a_comma_takes_the_comma_with_it() {
        assert_eq!(
            remove_fillers("I think, um, we should"),
            "I think, we should"
        );
    }

    /// Dropping the token must not drop the sentence: the full stop moves back
    /// onto the previous word.
    #[test]
    fn a_filler_ending_a_sentence_leaves_the_full_stop_behind() {
        assert_eq!(
            remove_fillers("that is all um. Next thing"),
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
            assert_eq!(remove_fillers(text), text, "rewrote: {text}");
        }
    }

    /// Case and trailing punctuation do not hide a filler.
    #[test]
    fn fillers_are_matched_regardless_of_case_or_punctuation() {
        assert_eq!(remove_fillers("Um, hello"), "hello");
        assert_eq!(remove_fillers("HMM hello"), "hello");
    }

    // ── dedupe ─────────────────────────────────────────────────────────────

    #[test]
    fn a_stuttered_word_collapses() {
        assert_eq!(dedupe_words("the the cat sat"), "the cat sat");
        assert_eq!(dedupe_words("I I think"), "I think");
    }

    /// English doubles some words legitimately; the rule must not "fix" them.
    #[test]
    fn legitimate_doubles_survive() {
        assert_eq!(dedupe_words("I had had enough"), "I had had enough");
        assert_eq!(
            dedupe_words("the thing that that broke"),
            "the thing that that broke"
        );
    }

    /// The copy carrying punctuation is the one that survives, so the comma is
    /// not collapsed away with the stutter.
    #[test]
    fn the_punctuated_copy_of_a_stutter_wins() {
        assert_eq!(dedupe_words("well well, no"), "well, no");
    }

    #[test]
    fn non_adjacent_repeats_are_left_alone() {
        assert_eq!(dedupe_words("the cat and the dog"), "the cat and the dog");
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
            clean(raw, &Rules::default()),
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
        };
        assert_eq!(clean(raw, &off), raw, "all rules off must be a no-op");
    }

    /// Rules are independent: switching one off leaves the others working.
    #[test]
    fn rules_are_individually_switchable() {
        let raw = "um the the thing";
        let keep_fillers = Rules {
            remove_fillers: false,
            ..Rules::default()
        };
        assert_eq!(clean(raw, &keep_fillers), "Um the thing");

        let keep_stutter = Rules {
            dedupe_words: false,
            ..Rules::default()
        };
        assert_eq!(clean(raw, &keep_stutter), "The the thing");
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
            let once = clean(raw, &Rules::default());
            let twice = clean(&once, &Rules::default());
            assert_eq!(once, twice, "second pass changed: {raw:?} -> {once:?}");
        }
    }

    /// A transcript of nothing but fillers cleans to nothing. The daemon reads
    /// an empty answer as "keep the raw text", so this is a safe outcome rather
    /// than a lost transcript.
    #[test]
    fn an_all_filler_transcript_cleans_to_nothing() {
        assert_eq!(clean("um uh hmm", &Rules::default()), "");
    }

    #[test]
    fn empty_and_whitespace_input_stay_empty() {
        assert_eq!(clean("", &Rules::default()), "");
        assert_eq!(clean("   \n ", &Rules::default()), "");
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
            let _ = clean(raw, &Rules::default());
        }
    }
}
