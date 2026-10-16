//! Articles in the GNOME Shell's search: the `org.gnome.Shell.SearchProvider2`
//! interface on the app's own bus name. A title typed into the Activities
//! Overview lists matching library articles; picking one opens it in
//! Blocksatz. Registered in `data/de.linuxundich.Blocksatz.search-provider.ini`
//! (with a D-Bus service file, so the Shell can start the app for it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use crate::i18n::tr;
use crate::{library, recentfiles};

const OBJECT_PATH: &str = "/de/linuxundich/Blocksatz/SearchProvider";

const INTERFACE: &str = r#"<node>
  <interface name="org.gnome.Shell.SearchProvider2">
    <method name="GetInitialResultSet">
      <arg type="as" name="terms" direction="in"/>
      <arg type="as" name="results" direction="out"/>
    </method>
    <method name="GetSubsearchResultSet">
      <arg type="as" name="previous_results" direction="in"/>
      <arg type="as" name="terms" direction="in"/>
      <arg type="as" name="results" direction="out"/>
    </method>
    <method name="GetResultMetas">
      <arg type="as" name="identifiers" direction="in"/>
      <arg type="aa{sv}" name="metas" direction="out"/>
    </method>
    <method name="ActivateResult">
      <arg type="s" name="identifier" direction="in"/>
      <arg type="as" name="terms" direction="in"/>
      <arg type="u" name="timestamp" direction="in"/>
    </method>
    <method name="LaunchSearch">
      <arg type="as" name="terms" direction="in"/>
      <arg type="u" name="timestamp" direction="in"/>
    </method>
  </interface>
</node>"#;

/// Most results the Shell gets; it shows a handful anyway.
const MAX_RESULTS: usize = 20;

/// One article the search can find.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    pub title: String,
    pub description: String,
}

/// Library articles (one per folder, the original first) and recently
/// opened files outside the library.
fn candidates() -> Vec<Candidate> {
    let mut out: Vec<Candidate> = library::scan_pairs(&library::root())
        .iter()
        .map(|pair| {
            let entry = pair.primary();
            let langs: Vec<String> = pair.files.iter().map(|e| library::file_lang(&e.path).flatten().unwrap_or_else(|| "de".into()).to_uppercase()).collect();
            Candidate { path: entry.path.clone(), title: library::title_hint(&entry.document).unwrap_or_else(|| folder_name(&pair.dir)), description: format!("{} · {}", entry.document.frontmatter.status.label(), langs.join(" + ")) }
        })
        .collect();
    for path in recentfiles::load() {
        if out.iter().any(|c| c.path == path) {
            continue;
        }
        let title = crate::document::read(&path).ok().and_then(|doc| library::title_hint(&doc)).unwrap_or_else(|| folder_name(&path));
        out.push(Candidate { description: path.display().to_string(), path, title });
    }
    out
}

fn folder_name(path: &Path) -> String {
    path.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_string()
}

/// Lower case with umlauts written out, so "ueber" finds „Über“.
fn fold(text: &str) -> String {
    text.to_lowercase().replace('ä', "ae").replace('ö', "oe").replace('ü', "ue").replace('ß', "ss")
}

/// The candidates whose title contains every term.
pub fn matching(candidates: &[Candidate], terms: &[String]) -> Vec<String> {
    let terms: Vec<String> = terms.iter().map(|t| fold(t)).filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    candidates
        .iter()
        .filter(|c| {
            let title = fold(&c.title);
            terms.iter().all(|t| title.contains(t.as_str()))
        })
        .take(MAX_RESULTS)
        .map(|c| c.path.display().to_string())
        .collect()
}

/// Registers the provider on the session bus connection - the shared one
/// the application itself takes its name on later, so the object is there
/// before the first call can arrive.
pub fn register(app: &adw::Application) {
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return };
    let info = match gio::DBusNodeInfo::for_xml(INTERFACE) {
        Ok(node) => node.lookup_interface("org.gnome.Shell.SearchProvider2"),
        Err(err) => {
            eprintln!("search provider: {err}");
            return;
        }
    };
    let Some(info) = info else { return };
    let app = app.downgrade();
    let registered = connection
        .register_object(OBJECT_PATH, &info)
        .method_call(move |_, _, _, _, method, params, invocation| {
            let Some(app) = app.upgrade() else {
                invocation.return_value(None);
                return;
            };
            handle(&app, method, &params, invocation);
        })
        .build();
    if let Err(err) = registered {
        eprintln!("search provider: {err}");
    }
    // `bus_get_sync` hands out a shared connection only while someone holds
    // it: dropped here, it would close - object and all - before the
    // application takes it up.
    CONNECTION.with(|slot| *slot.borrow_mut() = Some(connection));
}

thread_local! {
    static CONNECTION: std::cell::RefCell<Option<gio::DBusConnection>> = const { std::cell::RefCell::new(None) };
}

fn handle(app: &adw::Application, method: &str, params: &glib::Variant, invocation: gio::DBusMethodInvocation) {
    match method {
        "GetInitialResultSet" => {
            let (terms,) = params.get::<(Vec<String>,)>().unwrap_or_default();
            invocation.return_value(Some(&(matching(&candidates(), &terms),).to_variant()));
        }
        "GetSubsearchResultSet" => {
            let (_, terms) = params.get::<(Vec<String>, Vec<String>)>().unwrap_or_default();
            invocation.return_value(Some(&(matching(&candidates(), &terms),).to_variant()));
        }
        "GetResultMetas" => {
            let (ids,) = params.get::<(Vec<String>,)>().unwrap_or_default();
            let all = candidates();
            let icon = gio::ThemedIcon::new(crate::APP_ID);
            let metas: Vec<HashMap<String, glib::Variant>> = ids
                .iter()
                .filter_map(|id| all.iter().find(|c| c.path.display().to_string() == *id))
                .map(|c| {
                    let mut meta = HashMap::new();
                    meta.insert("id".to_string(), c.path.display().to_string().to_variant());
                    meta.insert("name".to_string(), c.title.to_variant());
                    meta.insert("description".to_string(), c.description.to_variant());
                    if let Some(icon) = IconExt::serialize(&icon) {
                        meta.insert("icon".to_string(), icon);
                    }
                    meta
                })
                .collect();
            invocation.return_value(Some(&(metas,).to_variant()));
        }
        "ActivateResult" => {
            let (id, _, _) = params.get::<(String, Vec<String>, u32)>().unwrap_or_default();
            // Only what this provider offered - not any path a caller names.
            if candidates().iter().any(|c| c.path.display().to_string() == id) {
                app.open(&[gio::File::for_path(&id)], "");
            }
            invocation.return_value(None);
        }
        "LaunchSearch" => {
            app.activate();
            invocation.return_value(None);
        }
        _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", &tr("Unbekannte Methode")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(title: &str) -> Candidate {
        Candidate { path: PathBuf::from(format!("/lib/{title}/artikel.md")), title: title.into(), description: String::new() }
    }

    #[test]
    fn every_term_must_match_with_umlauts_folded() {
        let all = vec![candidate("Über GNOME 51"), candidate("KDE Plasma"), candidate("GNOME Shell erweitern")];
        assert_eq!(matching(&all, &["gnome".into()]).len(), 2);
        assert_eq!(matching(&all, &["ueber".into(), "gnome".into()]), vec!["/lib/Über GNOME 51/artikel.md".to_string()]);
        assert_eq!(matching(&all, &["über".into()]).len(), 1);
        assert!(matching(&all, &[]).is_empty());
    }
}
