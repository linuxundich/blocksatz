//! Document statistics shown in the right pane's "Statistik" tab, computed
//! straight from the Markdown source text.

use adw::prelude::*;

use crate::i18n::tr;

/// VG Wort (the German collecting society compensating authors for online
/// text use) only counts an article toward compensation once its plain-
/// text length clears a minimum - this project's own site uses the
/// "Worthy" WordPress plugin (wordpress.org/plugins/wp-worthy/) for the
/// actual counting-pixel/Zählmarke, whose thresholds this mirrors exactly
/// (verified 2026-09-21 against its published source - `MIN_LENGTH`/
/// `WARN_LIMIT` in `wp-worthy.php` - not a number invented for this hint).
/// Compared against `chars_with_spaces` above, which is close enough to
/// Worthy's own count (it strips rendered HTML and collapses whitespace;
/// this instead counts the raw Markdown source, so syntax characters like
/// `#`/`**`/`[]()` inflate the estimate slightly) for a rough "not there
/// yet"/"cleared" signal - the same "rough heuristic, not a precise
/// measurement" spirit as the readability score above.
pub const VGWORT_MIN_LENGTH: usize = 1800;
/// An early warning threshold below the real minimum, while still
/// writing - Worthy's own `WARN_LIMIT`.
pub const VGWORT_WARN_LENGTH: usize = 1600;

#[derive(Clone, Copy)]
pub struct Stats {
    pub words: usize,
    pub chars_with_spaces: usize,
    pub chars_without_spaces: usize,
    pub paragraphs: usize,
    pub reading_minutes: usize,
    /// German-adapted Flesch reading-ease score (Amstad's formula), 0-100,
    /// higher meaning easier to read - `None` for an empty document, where
    /// the underlying words-per-sentence/syllables-per-word ratios are
    /// undefined rather than meaningfully zero.
    pub readability_score: Option<f64>,
    /// The two inputs the score above is actually computed from - shown
    /// alongside it so the number isn't just a black box, and reused by
    /// `readability_tips` to suggest which one to work on.
    pub avg_sentence_length: Option<f64>,
    pub avg_syllables_per_word: Option<f64>,
}

pub fn compute(markdown: &str) -> Stats {
    let words = markdown.split_whitespace().count();
    let paragraphs = markdown.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).count();
    let reading_minutes = if words == 0 { 0 } else { ((words as f64 / 200.0).ceil() as usize).max(1) };
    let metrics = readability_metrics(markdown, words);
    Stats {
        words,
        chars_with_spaces: markdown.chars().count(),
        chars_without_spaces: markdown.chars().filter(|c| !c.is_whitespace()).count(),
        paragraphs,
        reading_minutes,
        readability_score: metrics.map(|m| m.score),
        avg_sentence_length: metrics.map(|m| m.avg_sentence_length),
        avg_syllables_per_word: metrics.map(|m| m.avg_syllables_per_word),
    }
}

#[derive(Clone, Copy)]
struct ReadabilityMetrics {
    avg_sentence_length: f64,
    avg_syllables_per_word: f64,
    score: f64,
}

/// Amstad's German adaptation of the Flesch reading-ease formula: `180 -
/// ASL - (58.5 * ASW)`, where ASL is average sentence length (words per
/// sentence) and ASW is average syllables per word - the same 0-100 scale
/// as the original English Flesch score (higher = easier), just
/// recalibrated for German's longer average word length. Sentence and
/// syllable counting are both simple heuristics (counting `.`/`!`/`?` and
/// vowel-group runs respectively) rather than true linguistic analysis -
/// good enough for a rough "is this getting hard to read" signal, not a
/// precise measurement.
fn readability_metrics(markdown: &str, words: usize) -> Option<ReadabilityMetrics> {
    if words == 0 {
        return None;
    }
    let sentences = markdown.chars().filter(|c| matches!(c, '.' | '!' | '?')).count().max(1);
    let syllables: usize = markdown.split_whitespace().map(count_syllables).sum();
    let avg_sentence_length = words as f64 / sentences as f64;
    let avg_syllables_per_word = syllables as f64 / words as f64;
    let score = (180.0 - avg_sentence_length - 58.5 * avg_syllables_per_word).clamp(0.0, 100.0);
    Some(ReadabilityMetrics { avg_sentence_length, avg_syllables_per_word, score })
}

/// Sentence-length threshold (words/sentence) above which a sentence is
/// generally considered hard to follow in German writing-style guidance -
/// not derived from the formula itself, just a common rule-of-thumb
/// boundary editors use.
const LONG_SENTENCE_WORDS: f64 = 20.0;
const SOMEWHAT_LONG_SENTENCE_WORDS: f64 = 15.0;
/// Average syllables-per-word thresholds - German prose typically runs
/// somewhat above 1.5 due to compound words, so these sit a bit higher
/// than the sentence-length ones would suggest in isolation.
const MANY_SYLLABLES_PER_WORD: f64 = 2.0;
const SOMEWHAT_MANY_SYLLABLES_PER_WORD: f64 = 1.8;

/// Concrete, actionable suggestions for improving the readability score -
/// derived from which of the two underlying metrics is actually dragging
/// it down, not just a generic "write more simply" message, so the
/// author knows what to actually change. Never empty: falls back to an
/// encouraging message when neither metric is worth flagging.
fn readability_tips(avg_sentence_length: f64, avg_syllables_per_word: f64) -> Vec<String> {
    let mut tips = Vec::new();
    if avg_sentence_length > LONG_SENTENCE_WORDS {
        tips.push(tr("Die Sätze sind im Schnitt sehr lang (über 20 Wörter) - lange Sätze an Konjunktionen wie „und“ oder „weil“ in zwei kürzere aufteilen."));
    } else if avg_sentence_length > SOMEWHAT_LONG_SENTENCE_WORDS {
        tips.push(tr("Die Sätze sind im Schnitt eher lang - kürzere Sätze sind für Leser:innen oft leichter zu verarbeiten."));
    }
    if avg_syllables_per_word > MANY_SYLLABLES_PER_WORD {
        tips.push(tr("Viele lange, silbenreiche Wörter - wo möglich, kürzere und geläufigere Wörter statt Fach- oder Fremdwörtern verwenden."));
    } else if avg_syllables_per_word > SOMEWHAT_MANY_SYLLABLES_PER_WORD {
        tips.push(tr("Die Wörter sind im Schnitt eher lang - einfachere Formulierungen können den Text zugänglicher machen."));
    }
    if tips.is_empty() {
        tips.push(tr("Der Text ist bereits gut lesbar - kurze Sätze und einfache Wörter beibehalten."));
    }
    tips
}

/// Counts vowel-group runs in `word` as a proxy for syllables (e.g.
/// "Lesbarkeit" -> Le-s-ba-r-kei-t counts 3 vowel groups: e, a, ei) -
/// a common heuristic for languages without irregular silent vowels, and
/// close enough for German. Always at least 1, so an all-consonant token
/// (an abbreviation, a stray punctuation-only "word") doesn't zero out
/// the average.
fn count_syllables(word: &str) -> usize {
    let mut count = 0;
    let mut prev_is_vowel = false;
    for ch in word.to_lowercase().chars() {
        let is_vowel = matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'ä' | 'ö' | 'ü');
        if is_vowel && !prev_is_vowel {
            count += 1;
        }
        prev_is_vowel = is_vowel;
    }
    count.max(1)
}

/// The short qualitative label shown next to the numeric score, using the
/// same bucket boundaries as the standard Flesch-Reading-Ease scale.
fn readability_label(score: f64) -> String {
    match score {
        s if s >= 90.0 => tr("Sehr leicht"),
        s if s >= 70.0 => tr("Leicht"),
        s if s >= 50.0 => tr("Mittel"),
        s if s >= 30.0 => tr("Schwer"),
        _ => tr("Sehr schwer"),
    }
}

fn readability_display(score: Option<f64>) -> String {
    match score {
        Some(score) => tr("{label} ({score})").replace("{label}", &readability_label(score)).replace("{score}", &score.round().to_string()),
        None => "–".to_string(),
    }
}

/// One decimal place, German-style comma - matching `statusbar.rs`'s own
/// German-formatted (`.`-thousands-separated) number display convention.
fn format_decimal_de(value: f64) -> String {
    format!("{value:.1}").replace('.', ",")
}

/// The VG-Wort row's subtitle - not just a bare number, since "1523" means
/// nothing without the threshold it's being measured against. A separate
/// "bald erreicht" wording once past `VGWORT_WARN_LENGTH` (Worthy's own
/// early-warning point) gives a heads-up while still writing, distinct
/// from "you're not even close yet" further below it.
fn vgwort_status_text(chars_with_spaces: usize) -> String {
    if chars_with_spaces >= VGWORT_MIN_LENGTH {
        tr("{chars} Zeichen - Mindestlänge erreicht.").replace("{chars}", &chars_with_spaces.to_string())
    } else if chars_with_spaces >= VGWORT_WARN_LENGTH {
        tr("{chars} von {min} Zeichen - bald erreicht.").replace("{chars}", &chars_with_spaces.to_string()).replace("{min}", &VGWORT_MIN_LENGTH.to_string())
    } else {
        tr("{chars} von {min} Zeichen.").replace("{chars}", &chars_with_spaces.to_string()).replace("{min}", &VGWORT_MIN_LENGTH.to_string())
    }
}

pub struct StatsView {
    pub widget: gtk4::Widget,
    words: gtk4::Label,
    chars_with_spaces: gtk4::Label,
    chars_without_spaces: gtk4::Label,
    paragraphs: gtk4::Label,
    reading_minutes: gtk4::Label,
    vgwort_row: adw::ActionRow,
    vgwort_icon: gtk4::Image,
    readability_row: adw::ExpanderRow,
    sentence_length_value: gtk4::Label,
    syllables_value: gtk4::Label,
    tips_label: gtk4::Label,
}

impl StatsView {
    pub fn new() -> Self {
        let words = value_label();
        let chars_with_spaces = value_label();
        let chars_without_spaces = value_label();
        let paragraphs = value_label();
        let reading_minutes = value_label();

        // VG Wort's minimum-length gate (see `VGWORT_MIN_LENGTH`'s doc
        // comment) - a plain `Adw.ActionRow` with a status icon, the same
        // pass/fail shape `properties.rs`'s own "URL-Länge (SEO)" row
        // already uses, not a value row like the ones above (there's no
        // single "value" here, just a threshold check).
        let vgwort_row = adw::ActionRow::builder().title(tr("VG-Wort-Länge")).build();
        let vgwort_icon = gtk4::Image::new();
        vgwort_icon.set_valign(gtk4::Align::Center);
        vgwort_row.add_suffix(&vgwort_icon);

        // An `Adw.ExpanderRow`, not just another plain value row - the
        // score alone doesn't say anything about *why* it came out that
        // way, so this expands into the formula, the two measurements it
        // was computed from, and concrete tips (see `readability_tips`)
        // instead of leaving the number as an unexplained black box.
        let readability_row = adw::ExpanderRow::builder().title(tr("Lesbarkeit")).build();

        let formula_row = adw::ActionRow::builder()
            .title(tr("Berechnungsgrundlage"))
            .subtitle(tr("Deutsch angepasste Flesch-Formel (Amstad): 180 − Ø Wörter/Satz − 58,5 × Ø Silben/Wort"))
            .build();
        readability_row.add_row(&formula_row);

        let sentence_length_value = value_label();
        readability_row.add_row(&value_row(&tr("Ø Wörter pro Satz"), &sentence_length_value));

        let syllables_value = value_label();
        readability_row.add_row(&value_row(&tr("Ø Silben pro Wort"), &syllables_value));

        let tips_label = gtk4::Label::builder().wrap(true).xalign(0.0).margin_top(8).margin_bottom(8).margin_start(12).margin_end(12).build();
        tips_label.add_css_class("dim-label");
        readability_row.add_row(&tips_label);

        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::None);
        list.add_css_class("boxed-list");
        list.append(&value_row(&tr("Wörter"), &words));
        list.append(&value_row(&tr("Zeichen (mit Leerzeichen)"), &chars_with_spaces));
        list.append(&value_row(&tr("Zeichen (ohne Leerzeichen)"), &chars_without_spaces));
        list.append(&value_row(&tr("Absätze"), &paragraphs));
        list.append(&value_row(&tr("Geschätzte Lesezeit"), &reading_minutes));
        list.append(&vgwort_row);
        list.append(&readability_row);

        // A plain boxed list: it sits inside the scrolled "Beitrag" view
        // of the right-hand pane (`postpane.rs`), which brings its own
        // scrolling and margins.
        Self {
            widget: list.upcast(),
            words,
            chars_with_spaces,
            chars_without_spaces,
            paragraphs,
            reading_minutes,
            vgwort_row,
            vgwort_icon,
            readability_row,
            sentence_length_value,
            syllables_value,
            tips_label,
        }
    }

    pub fn update(&self, markdown: &str) {
        let stats = compute(markdown);
        self.words.set_label(&stats.words.to_string());
        self.chars_with_spaces.set_label(&stats.chars_with_spaces.to_string());
        self.chars_without_spaces.set_label(&stats.chars_without_spaces.to_string());
        self.paragraphs.set_label(&stats.paragraphs.to_string());
        self.reading_minutes.set_label(&tr("{n} min").replace("{n}", &stats.reading_minutes.to_string()));
        self.vgwort_row.set_subtitle(&vgwort_status_text(stats.chars_with_spaces));
        if stats.chars_with_spaces >= VGWORT_MIN_LENGTH {
            self.vgwort_icon.set_icon_name(Some("object-select-symbolic"));
            self.vgwort_icon.remove_css_class("warning");
            self.vgwort_icon.add_css_class("success");
        } else {
            self.vgwort_icon.set_icon_name(Some("dialog-warning-symbolic"));
            self.vgwort_icon.remove_css_class("success");
            self.vgwort_icon.add_css_class("warning");
        }
        self.readability_row.set_subtitle(&readability_display(stats.readability_score));
        match (stats.avg_sentence_length, stats.avg_syllables_per_word) {
            (Some(avg_sentence_length), Some(avg_syllables_per_word)) => {
                self.sentence_length_value.set_label(&format_decimal_de(avg_sentence_length));
                self.syllables_value.set_label(&format_decimal_de(avg_syllables_per_word));
                let tips = readability_tips(avg_sentence_length, avg_syllables_per_word);
                self.tips_label.set_label(&tips.iter().map(|tip| format!("• {tip}")).collect::<Vec<_>>().join("\n"));
            }
            _ => {
                self.sentence_length_value.set_label("–");
                self.syllables_value.set_label("–");
                self.tips_label.set_label("");
            }
        }
    }
}

fn value_label() -> gtk4::Label {
    let label = gtk4::Label::new(Some("0"));
    label.add_css_class("dim-label");
    label
}

fn value_row(title: &str, value: &gtk4::Label) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(value);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readability_score_is_none_for_an_empty_document() {
        assert_eq!(compute("").readability_score, None);
    }

    #[test]
    fn count_syllables_counts_vowel_group_runs() {
        assert_eq!(count_syllables("Haus"), 1);
        assert_eq!(count_syllables("Lesbarkeit"), 3);
        assert_eq!(count_syllables("xyz"), 1, "an all-consonant token still counts as at least one syllable");
    }

    #[test]
    fn short_simple_sentences_score_higher_than_one_long_complex_sentence() {
        let simple = "Der Hund rennt. Die Katze schläft. Wir gehen raus.";
        let complex = "Der ungewöhnlich lebhafte, außergewöhnlich neugierige Schäferhund rannte, \
             ohne einen einzigen Moment innezuhalten oder sich umzuschauen, unaufhaltsam \
             durch den weitläufigen, dicht bewachsenen Garten seines Nachbarn.";
        let simple_score = compute(simple).readability_score.expect("simple text should have a score");
        let complex_score = compute(complex).readability_score.expect("complex text should have a score");
        assert!(simple_score > complex_score, "expected {simple_score} > {complex_score}");
    }

    #[test]
    fn readability_label_uses_the_standard_flesch_bucket_boundaries() {
        assert_eq!(readability_label(95.0), "Sehr leicht");
        assert_eq!(readability_label(75.0), "Leicht");
        assert_eq!(readability_label(55.0), "Mittel");
        assert_eq!(readability_label(35.0), "Schwer");
        assert_eq!(readability_label(10.0), "Sehr schwer");
    }

    #[test]
    fn readability_display_shows_a_dash_for_no_score() {
        assert_eq!(readability_display(None), "–");
    }

    #[test]
    fn readability_display_combines_label_and_rounded_score() {
        assert_eq!(readability_display(Some(95.4)), "Sehr leicht (95)");
    }

    #[test]
    fn avg_sentence_length_and_syllables_are_none_for_an_empty_document() {
        let stats = compute("");
        assert_eq!(stats.avg_sentence_length, None);
        assert_eq!(stats.avg_syllables_per_word, None);
    }

    #[test]
    fn avg_sentence_length_and_syllables_are_computed_alongside_the_score() {
        let stats = compute("Der Hund rennt. Die Katze schläft.");
        assert_eq!(stats.avg_sentence_length, Some(3.0));
        assert!(stats.avg_syllables_per_word.unwrap() > 0.0);
    }

    #[test]
    fn format_decimal_de_uses_a_comma_and_one_decimal_place() {
        assert_eq!(format_decimal_de(12.34), "12,3");
        assert_eq!(format_decimal_de(2.0), "2,0");
    }

    #[test]
    fn vgwort_status_text_reports_the_minimum_reached() {
        let text = vgwort_status_text(1800);
        assert!(text.contains("1800"), "{text}");
        assert!(text.contains("Mindestlänge erreicht"), "{text}");
    }

    #[test]
    fn vgwort_status_text_warns_when_close_but_not_there_yet() {
        let text = vgwort_status_text(1700);
        assert!(text.contains("1700"), "{text}");
        assert!(text.contains("bald erreicht"), "{text}");
    }

    #[test]
    fn vgwort_status_text_is_plain_when_far_below_the_minimum() {
        let text = vgwort_status_text(200);
        assert!(text.contains("200"), "{text}");
        assert!(!text.contains("erreicht"), "{text}");
    }

    #[test]
    fn readability_tips_flags_long_sentences() {
        let tips = readability_tips(25.0, 1.5);
        assert_eq!(tips.len(), 1);
        assert!(tips[0].contains("lang"), "{tips:?}");
    }

    #[test]
    fn readability_tips_flags_syllable_heavy_words() {
        let tips = readability_tips(8.0, 2.5);
        assert_eq!(tips.len(), 1);
        assert!(tips[0].contains("silbenreich"), "{tips:?}");
    }

    #[test]
    fn readability_tips_can_flag_both_metrics_at_once() {
        let tips = readability_tips(25.0, 2.5);
        assert_eq!(tips.len(), 2);
    }

    #[test]
    fn readability_tips_is_encouraging_when_both_metrics_are_fine() {
        let tips = readability_tips(8.0, 1.4);
        assert_eq!(tips.len(), 1);
        assert!(tips[0].contains("bereits gut lesbar"), "{tips:?}");
    }
}
