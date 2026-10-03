//! Persistence for the editor context menu's AI actions: the five built-in
//! prompt templates (editable/resettable, like the chat's system prompt)
//! and the user's own custom prompts. All plain, non-sensitive text, so
//! this is a single JSON file under
//! `glib::user_config_dir()/blocksatz/ai_prompts.json` - no secrets
//! involved here.

use std::path::PathBuf;

use gtk4::glib;
use serde_json::Value;

use crate::i18n::tr;

pub struct BuiltinPrompt {
    pub id: &'static str,
    pub default_template: &'static str,
}

/// The five built-in context-menu actions. `adjust-length`'s template
/// contains a `{length_instruction}` placeholder filled in from the "Länge
/// anpassen" dialog's inputs. Display titles live in [`builtin_title`], not
/// here - they need to go through `tr()`, which can't be called in a
/// `const` initializer.
pub const BUILTIN_PROMPTS: &[BuiltinPrompt] = &[
    BuiltinPrompt {
        id: "check-content",
        default_template: "Prüfe den folgenden Artikel bzw. Abschnitt auf inhaltliche und technische Korrektheit. Recherchiere bei Bedarf im Internet, um Fakten, Versionsnummern, Befehle und technische Zusammenhänge zu verifizieren. Liste gefundene Fehler oder fragwürdige Aussagen übersichtlich auf, jeweils mit kurzer Begründung und - falls zutreffend - einer Quelle. Schlage Korrekturen vor, gib aber keine komplette Neufassung des Textes aus, sondern nur die Liste der Befunde.",
    },
    BuiltinPrompt {
        id: "check-style",
        default_template: "Prüfe den folgenden Artikel bzw. Abschnitt auf Schreibstil und übliche Formatierung. Orientiere dich dabei an bestehenden Artikeln dieses Blogs (Tonfall, Ansprache der Leser, typische Satzlänge, Einsatz von Zwischenüberschriften, Code-Formatierung, Aufzählungen). Liste Abweichungen übersichtlich auf und schlage konkrete Verbesserungen vor. Gib im Anschluss zusätzlich eine überarbeitete Fassung des Textes aus, die Inhalt und Kernaussagen unverändert lässt.",
    },
    BuiltinPrompt {
        id: "check-spelling",
        default_template: "Prüfe den folgenden Artikel bzw. Abschnitt auf Rechtschreibfehler. Stelle mir die gefundenen Fehler übersichtlich als Liste zusammen (jeweils die fehlerhafte Stelle und die Korrektur). Gib im Anschluss den vollständig korrigierten Text aus - inhaltlich und stilistisch unverändert, nur mit korrigierter Rechtschreibung.",
    },
    BuiltinPrompt {
        id: "check-punctuation",
        default_template: "Prüfe den folgenden Artikel bzw. Abschnitt auf Fehler in der Zeichensetzung (Kommasetzung, Anführungszeichen, Bindestriche/Gedankenstriche, sonstige Satzzeichen). Stelle mir die gefundenen Fehler übersichtlich als Liste zusammen (jeweils die fehlerhafte Stelle und die Korrektur). Gib im Anschluss den vollständig korrigierten Text aus - inhaltlich und stilistisch unverändert, nur mit korrigierter Zeichensetzung.",
    },
    BuiltinPrompt {
        id: "adjust-length",
        default_template: "Passe die Länge des folgenden Artikels bzw. Abschnitts {length_instruction} an. Kürze bzw. erweitere passend zum bestehenden Schreibstil und behalte die Kernaussagen bei. Gib ausschließlich den angepassten Text aus, ohne zusätzliche Erklärungen.",
    },
];

/// The translated display title for a built-in prompt - `BuiltinPrompt.title`
/// itself stays the fixed German key used for lookups/JSON, since `tr()`
/// needs a literal at its call site (see `po/README.md`) and can't see
/// through that field.
pub fn builtin_title(id: &str) -> String {
    match id {
        "check-content" => tr("Inhalt prüfen"),
        "check-style" => tr("Stil & Formatierung prüfen"),
        "check-spelling" => tr("Rechtschreibung prüfen"),
        "check-punctuation" => tr("Zeichensetzung prüfen"),
        "adjust-length" => tr("Länge anpassen"),
        _ => tr("KI-Aktion"),
    }
}

fn default_template_for(id: &str) -> &'static str {
    BUILTIN_PROMPTS.iter().find(|p| p.id == id).map(|p| p.default_template).unwrap_or("")
}

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir
}

fn prompts_path() -> PathBuf {
    let mut path = config_dir();
    path.push("ai_prompts.json");
    path
}

fn load_root() -> Value {
    std::fs::read_to_string(prompts_path())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn save_root(root: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(prompts_path(), root.to_string())
}

/// The active text for a built-in prompt: the user's saved override if
/// they've customized it, otherwise its default template.
pub fn load_prompt_text(id: &str) -> String {
    load_root()
        .get("builtin_overrides")
        .and_then(|overrides| overrides.get(id))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| default_template_for(id).to_string())
}

/// Like `load_prompt_text`, for a prompt that isn't one of the context-menu
/// built-ins (the translation prompt, `translate.rs`) and so brings its own
/// default.
pub fn load_text_or(id: &str, default: &str) -> String {
    load_root()
        .get("builtin_overrides")
        .and_then(|overrides| overrides.get(id))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string())
}

pub fn save_prompt_text(id: &str, text: &str) -> std::io::Result<()> {
    let mut root = load_root();
    root["builtin_overrides"][id] = serde_json::json!(text);
    save_root(&root)
}

pub fn reset_prompt_text(id: &str) -> std::io::Result<()> {
    let mut root = load_root();
    if let Some(overrides) = root.get_mut("builtin_overrides").and_then(Value::as_object_mut) {
        overrides.remove(id);
    }
    save_root(&root)
}

pub fn is_prompt_customized(id: &str) -> bool {
    load_root().get("builtin_overrides").and_then(|overrides| overrides.get(id)).is_some()
}

#[derive(Debug, Clone, PartialEq)]
pub struct CustomPrompt {
    pub id: String,
    pub title: String,
    pub template: String,
}

pub fn load_custom_prompts() -> Vec<CustomPrompt> {
    load_root()
        .get("custom_prompts")
        .and_then(Value::as_array)
        .map(|prompts| {
            prompts
                .iter()
                .filter_map(|p| {
                    Some(CustomPrompt {
                        id: p.get("id")?.as_str()?.to_string(),
                        title: p.get("title")?.as_str().unwrap_or("").to_string(),
                        template: p.get("template")?.as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn save_custom_prompts(prompts: &[CustomPrompt]) -> std::io::Result<()> {
    let mut root = load_root();
    root["custom_prompts"] = serde_json::json!(
        prompts.iter().map(|p| serde_json::json!({"id": p.id, "title": p.title, "template": p.template})).collect::<Vec<_>>()
    );
    save_root(&root)
}

/// A new, unique id for a freshly created custom prompt - not exposed to
/// the user (they see only `title`).
pub fn new_custom_prompt_id() -> String {
    format!("custom-{}", glib::monotonic_time())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_prompt_has_a_non_empty_default_template() {
        for prompt in BUILTIN_PROMPTS {
            assert!(!prompt.default_template.is_empty(), "{} has an empty default template", prompt.id);
        }
    }

    #[test]
    fn adjust_length_template_carries_its_placeholder() {
        assert!(default_template_for("adjust-length").contains("{length_instruction}"));
    }

    /// All tests below read-modify-write the *same* `ai_prompts.json` file,
    /// so without serializing them, cargo's default parallel test threads
    /// race and clobber each other's writes - this lock (recovered on
    /// poison, since a panicking test shouldn't wedge the rest) makes them
    /// behave as if run with `--test-threads=1` regardless of the harness's
    /// actual thread count.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn lock_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Touches the real config dir like `chatconfig`'s tests do - captures
    /// and restores whatever was already saved so the test machine's actual
    /// config isn't left changed.
    #[test]
    fn builtin_prompt_save_load_reset_round_trips() {
        let _guard = lock_guard();
        let id = "check-content";
        let was_customized = is_prompt_customized(id);
        let original = load_prompt_text(id);

        save_prompt_text(id, "Custom test template").expect("save failed");
        assert_eq!(load_prompt_text(id), "Custom test template");
        assert!(is_prompt_customized(id));

        reset_prompt_text(id).expect("reset failed");
        assert_eq!(load_prompt_text(id), default_template_for(id));
        assert!(!is_prompt_customized(id));

        if was_customized {
            save_prompt_text(id, &original).expect("restore failed");
        }
    }

    #[test]
    fn custom_prompts_round_trip() {
        let _guard = lock_guard();
        let original = load_custom_prompts();

        let edited = vec![CustomPrompt {
            id: "custom-test".to_string(),
            title: "Test-Prompt".to_string(),
            template: "Do the thing.".to_string(),
        }];
        save_custom_prompts(&edited).expect("save failed");
        assert_eq!(load_custom_prompts(), edited);

        save_custom_prompts(&original).expect("restore failed");
    }

    #[test]
    fn new_custom_prompt_ids_are_unique() {
        let a = new_custom_prompt_id();
        let b = new_custom_prompt_id();
        assert_ne!(a, b);
    }
}
