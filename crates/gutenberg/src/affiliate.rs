//! Affiliate links: a plain Markdown link to a partner program goes up
//! with `rel="sponsored nofollow noopener"`, the marking search engines
//! expect for paid links and the hook a blog theme can hang a visible
//! "Affiliate-Link" badge on (`a[rel~='sponsored']`). Markdown has no way
//! to set `rel`, so the target decides: an Amazon address carrying a
//! partner tag (`?tag=…`) or an Amazon short link (`amzn.to`).
//!
//! Opening such a post again turns the link back into plain Markdown, as
//! long as it carries exactly this `rel` - anything else stays HTML.

use pulldown_cmark::{Event, Tag, TagEnd};

/// What an affiliate link gets.
pub const REL: &str = "sponsored nofollow noopener";

/// Whether `url` is an affiliate link by its address alone.
pub fn is_affiliate_url(url: &str) -> bool {
    let Some(host) = host(url) else { return false };
    if matches!(host.as_str(), "amzn.to" | "amzn.eu") {
        return true;
    }
    is_amazon_host(&host) && query_param(url, "tag").is_some_and(|tag| !tag.is_empty())
}

/// An Amazon product link in its short, clean form with `tag`:
/// `https://www.amazon.de/dp/B0D8JCN5P4?tag=…`. Takes a product address
/// in any of Amazon's shapes (`/dp/`, `/gp/product/`, with title slug,
/// tracking parameters, another country's shop) or a bare ASIN, which
/// lands on amazon.de. `None` when no ASIN can be found.
pub fn amazon_link(input: &str, tag: &str) -> Option<String> {
    let input = input.trim();
    let (host, asin) = if is_asin(input) {
        ("www.amazon.de".to_string(), input.to_uppercase())
    } else {
        let host = host(input).filter(|h| is_amazon_host(h))?;
        (if host.starts_with("www.") { host } else { format!("www.{host}") }, asin_in(input)?)
    };
    let tag = tag.trim();
    Some(if tag.is_empty() { format!("https://{host}/dp/{asin}") } else { format!("https://{host}/dp/{asin}?tag={tag}") })
}

/// The partner tag in an Amazon address, if any.
pub fn amazon_tag(url: &str) -> Option<String> {
    host(url).filter(|h| is_amazon_host(h))?;
    query_param(url, "tag").filter(|t| !t.is_empty())
}

/// Wraps affiliate links in `<a … rel>` - the rest of the events pass
/// through for pulldown-cmark's own HTML writer.
pub(crate) fn mark_links<'a>(events: impl IntoIterator<Item = Event<'a>>) -> Vec<Event<'a>> {
    // Per open link: whether it was replaced (its end must be too).
    let mut open: Vec<bool> = Vec::new();
    events
        .into_iter()
        .map(|event| match event {
            Event::Start(Tag::Link { dest_url, title, .. }) if is_affiliate_url(&dest_url) => {
                open.push(true);
                let title = if title.is_empty() { String::new() } else { format!(" title=\"{}\"", crate::escape_html(&title)) };
                Event::InlineHtml(format!("<a href=\"{}\"{title} rel=\"{REL}\">", crate::escape_html(&dest_url)).into())
            }
            event @ Event::Start(Tag::Link { .. }) => {
                open.push(false);
                event
            }
            Event::End(TagEnd::Link) if open.pop() == Some(true) => Event::InlineHtml("</a>".into()),
            event => event,
        })
        .collect()
}

fn host(url: &str) -> Option<String> {
    let rest = url.trim().strip_prefix("https://").or_else(|| url.trim().strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.split('@').next_back()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// `amazon.de`, `www.amazon.co.uk`, `smile.amazon.com` ...
fn is_amazon_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.iter().position(|l| *l == "amazon").is_some_and(|i| {
        let tld = &labels[i + 1..];
        (1..=2).contains(&tld.len()) && tld.iter().all(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphabetic()))
    })
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| value.to_string())
    })
}

fn is_asin(s: &str) -> bool {
    s.len() == 10 && s.chars().all(|c| c.is_ascii_alphanumeric()) && s.chars().any(|c| c.is_ascii_digit())
}

/// The ASIN after `/dp/`, `/gp/product/`, `/gp/aw/d/` or `/exec/obidos/ASIN/`.
fn asin_in(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let parts: Vec<&str> = path.split('/').collect();
    parts.windows(2).find_map(|w| {
        let marker = matches!(w[0], "dp" | "product" | "d" | "ASIN");
        (marker && is_asin(w[1])).then(|| w[1].to_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_partner_links() {
        assert!(is_affiliate_url("https://www.amazon.de/dp/B0D8JCN5P4?tag=linuxundich-21"));
        assert!(is_affiliate_url("https://amazon.co.uk/gp/product/B0D8JCN5P4/?ref=x&tag=abc-21"));
        assert!(is_affiliate_url("https://amzn.to/3xYz"));
        assert!(!is_affiliate_url("https://www.amazon.de/dp/B0D8JCN5P4"), "no tag, no partner link");
        assert!(!is_affiliate_url("https://www.amazon.de/dp/B0D8JCN5P4?tag="));
        assert!(!is_affiliate_url("https://amazon.example.org/?tag=x"));
        assert!(!is_affiliate_url("https://linuxundich.de/?tag=amazon"));
    }

    #[test]
    fn normalizes_amazon_addresses() {
        let tag = "linuxundich-21";
        let want = "https://www.amazon.de/dp/B0D8JCN5P4?tag=linuxundich-21";
        assert_eq!(amazon_link("B0D8JCN5P4", tag).as_deref(), Some(want));
        assert_eq!(amazon_link("https://www.amazon.de/Raspberry-Pi-5/dp/B0D8JCN5P4/ref=sr_1_1?crid=X&keywords=pi", tag).as_deref(), Some(want));
        assert_eq!(amazon_link("https://amazon.de/gp/product/B0D8JCN5P4?tag=christophlang-21", tag).as_deref(), Some(want));
        assert_eq!(amazon_link("https://www.amazon.com/dp/B0D8JCN5P4", tag).as_deref(), Some("https://www.amazon.com/dp/B0D8JCN5P4?tag=linuxundich-21"));
        assert_eq!(amazon_link("https://www.amazon.de/s?k=raspberry", tag), None);
        assert_eq!(amazon_link("Raspberry", tag), None);
    }

    #[test]
    fn reads_the_tag() {
        assert_eq!(amazon_tag("https://www.amazon.de/dp/B0D8JCN5P4?tag=linuxundich-21").as_deref(), Some("linuxundich-21"));
        assert_eq!(amazon_tag("https://www.amazon.de/dp/B0D8JCN5P4"), None);
    }
}
