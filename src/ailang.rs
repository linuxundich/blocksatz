//! The language an AI action writes in for the open document - one rule
//! for every action that produces text for the article (alt text, caption,
//! tags), so an English language version doesn't get German back just
//! because the prompts themselves are written in German.
//!
//! The language is the document's own (`Frontmatter::lang`, set on a
//! language version like `artikel.en.md`); an original without one is in
//! the blog's language, German.

use crate::document::Frontmatter;

/// The language code AI output for `frontmatter`'s document should be in.
pub fn of(frontmatter: &Frontmatter) -> String {
    frontmatter.lang.clone().filter(|lang| !lang.trim().is_empty()).unwrap_or_else(|| "de".to_string())
}

/// The sentence appended to a prompt to pin the answer's language - in the
/// prompt's own German and, for any other language, once more in English,
/// which models follow most reliably.
pub fn instruction(lang: &str) -> String {
    match lang {
        "de" => "\n\nAntworte auf Deutsch.".to_string(),
        other => {
            let english = crate::translate::language_name(other);
            format!("\n\nWichtig: Antworte ausschließlich auf {} - nicht auf Deutsch. Write your answer in {english} only.", german_name(other))
        }
    }
}

fn german_name(lang: &str) -> String {
    match lang {
        "en" => "Englisch".to_string(),
        "fr" => "Französisch".to_string(),
        "es" => "Spanisch".to_string(),
        "it" => "Italienisch".to_string(),
        "nl" => "Niederländisch".to_string(),
        other => other.to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_version_answers_in_its_own_language() {
        let fm = Frontmatter { lang: Some("en".into()), ..Frontmatter::default() };
        assert_eq!(of(&fm), "en");
        assert!(instruction(&of(&fm)).contains("in English only"));
    }

    #[test]
    fn an_original_without_a_language_is_german() {
        assert_eq!(of(&Frontmatter::default()), "de");
        assert!(!instruction("de").contains("English"));
    }
}
