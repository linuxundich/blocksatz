//! "Mit Blog-Fassung vergleichen": the working copy next to the post as it
//! is on the blog right now, as a line diff - lines only on the blog,
//! lines only here, and the unchanged rest. Offered from the main
//! action's menu and from the conflict dialog, so a decision between
//! "take the blog's version" and "keep mine" isn't taken blind.

use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::tr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Same,
    /// Only in the blog's version.
    Removed,
    /// Only in the local version.
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub change: Change,
    pub text: String,
}

/// Line diff from `blog` to `local` (longest common subsequence). Common
/// leading and trailing lines are matched first, so the quadratic part
/// only ever sees the region that actually differs.
pub fn line_diff(blog: &str, local: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = blog.lines().collect();
    let b: Vec<&str> = local.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (mid_a, mid_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);

    // lcs[i][j]: length of the LCS of mid_a[i..] and mid_b[j..].
    let (n, m) = (mid_a.len(), mid_b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if mid_a[i] == mid_b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }

    let line = |change, text: &str| DiffLine { change, text: text.to_string() };
    let mut out: Vec<DiffLine> = a[..prefix].iter().map(|t| line(Change::Same, t)).collect();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if mid_a[i] == mid_b[j] {
            out.push(line(Change::Same, mid_a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(line(Change::Removed, mid_a[i]));
            i += 1;
        } else {
            out.push(line(Change::Added, mid_b[j]));
            j += 1;
        }
    }
    out.extend(mid_a[i..].iter().map(|t| line(Change::Removed, t)));
    out.extend(mid_b[j..].iter().map(|t| line(Change::Added, t)));
    out.extend(a[a.len() - suffix..].iter().map(|t| line(Change::Same, t)));
    out
}

/// What the user picked in the comparison dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    TakeBlog,
    KeepMine,
}

/// Shows the diff. `offer_keep_mine` adds "Meine Fassung behalten" (for a
/// conflict); `on_choice` runs for the chosen button, not on close.
pub fn open(parent: &impl IsA<gtk4::Widget>, blog: &str, local: &str, offer_keep_mine: bool, on_choice: impl Fn(Choice) + 'static) {
    let diff = line_diff(blog, local);
    let buffer = sourceview5::Buffer::new(None::<&gtk4::TextTagTable>);
    crate::appearance::follow_scheme(&buffer);
    let removed = buffer.create_tag(Some("removed"), &[("paragraph-background-rgba", &gtk4::gdk::RGBA::new(0.88, 0.11, 0.14, 0.18))]).expect("new tag");
    let added = buffer.create_tag(Some("added"), &[("paragraph-background-rgba", &gtk4::gdk::RGBA::new(0.15, 0.64, 0.41, 0.20))]).expect("new tag");
    let same = buffer.create_tag(Some("same"), &[("foreground-rgba", &gtk4::gdk::RGBA::new(0.5, 0.5, 0.5, 1.0))]).expect("new tag");
    let changes = diff.iter().filter(|l| l.change != Change::Same).count();
    for line in &diff {
        let (prefix, tag) = match line.change {
            Change::Same => ("  ", &same),
            Change::Removed => ("− ", &removed),
            Change::Added => ("+ ", &added),
        };
        let mut end = buffer.end_iter();
        buffer.insert_with_tags(&mut end, &format!("{prefix}{}\n", line.text), &[tag]);
    }
    let view = sourceview5::View::builder().buffer(&buffer).editable(false).monospace(true).wrap_mode(gtk4::WrapMode::WordChar).top_margin(12).bottom_margin(12).left_margin(12).right_margin(12).build();
    view.add_css_class(crate::appearance::EDITOR_FONT_CSS_CLASS);
    let scrolled = gtk4::ScrolledWindow::builder().child(&view).vexpand(true).build();

    let title = adw::WindowTitle::new(
        &tr("Mit Blog-Fassung vergleichen"),
        &if changes == 0 { tr("Keine Unterschiede im Text") } else { tr("− nur im Blog · + nur hier") },
    );
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let dialog = adw::Dialog::builder().content_width(900).content_height(700).build();
    let take_blog = gtk4::Button::builder().label(tr("Blog-Fassung übernehmen")).build();
    take_blog.add_css_class("destructive-action");
    take_blog.add_css_class("pill");
    let buttons = gtk4::Box::builder().spacing(12).halign(gtk4::Align::Center).margin_top(12).margin_bottom(12).build();
    buttons.append(&take_blog);
    let on_choice = Rc::new(on_choice);
    {
        let dialog = dialog.clone();
        let on_choice = on_choice.clone();
        take_blog.connect_clicked(move |_| {
            dialog.close();
            on_choice(Choice::TakeBlog);
        });
    }
    if offer_keep_mine {
        let keep = gtk4::Button::builder().label(tr("Meine Fassung behalten")).build();
        keep.add_css_class("suggested-action");
        keep.add_css_class("pill");
        let dialog = dialog.clone();
        keep.connect_clicked(move |_| {
            dialog.close();
            on_choice(Choice::KeepMine);
        });
        buttons.append(&keep);
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled));
    toolbar.add_bottom_bar(&buttons);
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(diff: &[DiffLine]) -> Vec<String> {
        diff.iter()
            .map(|l| {
                let p = match l.change {
                    Change::Same => ' ',
                    Change::Removed => '-',
                    Change::Added => '+',
                };
                format!("{p}{}", l.text)
            })
            .collect()
    }

    #[test]
    fn identical_texts_have_no_changes() {
        assert!(line_diff("a\nb\n", "a\nb\n").iter().all(|l| l.change == Change::Same));
    }

    #[test]
    fn a_changed_line_shows_as_removed_and_added() {
        assert_eq!(render(&line_diff("Titel\nalt\nEnde\n", "Titel\nneu\nEnde\n")), vec![" Titel", "-alt", "+neu", " Ende"]);
    }

    #[test]
    fn inserted_and_deleted_lines_keep_the_rest_aligned() {
        assert_eq!(render(&line_diff("a\nb\nc\nd\n", "a\nc\nd\ne\n")), vec![" a", "-b", " c", " d", "+e"]);
        assert_eq!(render(&line_diff("", "neu\n")), vec!["+neu"]);
        assert_eq!(render(&line_diff("weg\n", "")), vec!["-weg"]);
    }
}
