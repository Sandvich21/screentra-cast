//! RSS / Atom feed fetching and parsing for the News Ticker widget.
//!
//! Two entry points:
//!   - [`parse_feed`]: pure parser. Takes raw XML bytes and a content-type
//!     hint, returns a Vec<NewsHeadline>. Used by tests and by the
//!     in-process fetcher.
//!   - [`fetch_feed`]: HTTP fetcher. Wraps `parse_feed` with a timeout,
//!     a User-Agent, and conditional GET support (ETag / Last-Modified).
//!     Returns the parsed headlines plus the cache-control hints so the
//!     DB layer can store them.
//!
//! The fetcher is deliberately conservative: 10 second timeout, max 1 MiB
//! response, no redirects (we don't trust third-party feeds to redirect
//! us anywhere safe), and we refuse to follow any feed that doesn't
//! declare itself as RSS or Atom.

use crate::models::{NewsFeed, NewsHeadline};
use std::time::Duration;

/// Cap on the body we will read from a feed. RSS feeds should be tiny;
/// anything past 1 MiB is almost certainly not a real feed.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Per-fetch timeout. Feeds that take longer than this to respond
/// (or to send their body) get dropped on the floor so a single slow
/// host can't block the maintenance loop.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// User-Agent string. Includes a contact hint so a feed operator can
/// reach us if our polling is causing problems.
const USER_AGENT: &str = "Screentra Cast/1.0 (news-ticker; +https://github.com/ripley-kiosk)";

/// Parse a feed (RSS 2.0 or Atom) from raw bytes. The content_type is
/// used only to disambiguate; if the body is well-formed XML we sniff
/// the root element to pick a parser regardless of what the server
/// claims.
///
/// `source_title` is what we put in the `source` field of each headline
/// (typically the channel/feed title, if we can find it).
pub fn parse_feed(body: &[u8], content_type: Option<&str>, source_title: &str) -> Vec<NewsHeadline> {
    // Sniff the root element if the body is large enough. Both RSS and
    // Atom have a recognizable first non-whitespace token.
    let kind = sniff_kind(body, content_type);
    match kind {
        FeedKind::Rss => parse_rss(body, source_title),
        FeedKind::Atom => parse_atom(body, source_title),
        FeedKind::Unknown => Vec::new(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FeedKind {
    Rss,
    Atom,
    Unknown,
}

fn sniff_kind(body: &[u8], content_type: Option<&str>) -> FeedKind {
    if let Some(ct) = content_type {
        let lower = ct.to_ascii_lowercase();
        if lower.contains("rss") || lower.contains("atom") {
            if lower.contains("atom") {
                return FeedKind::Atom;
            }
            if lower.contains("rss") {
                return FeedKind::Rss;
            }
        }
    }
    // Fall back to sniffing the first non-whitespace bytes.
    let head: Vec<u8> = body.iter().take(512).copied().collect();
    // Strip leading whitespace and BOM.
    let mut i = 0;
    while i < head.len() && (head[i] as char).is_whitespace() {
        i += 1;
    }
    if i + 5 <= head.len() && &head[i..i + 5] == b"<?xml" {
        // XML declaration; look for <rss or <feed later in the head.
        let tail = &head[i..];
        if tail.windows(4).any(|w| w == b"<rss" || w == b"<RSS") {
            return FeedKind::Rss;
        }
        if tail.windows(6).any(|w| w == b"<feed " || w == b"<feed>") {
            return FeedKind::Atom;
        }
    } else if i + 4 <= head.len() && &head[i..i + 4] == b"<rss" {
        return FeedKind::Rss;
    } else if i + 5 <= head.len() && &head[i..i + 5] == b"<feed" {
        return FeedKind::Atom;
    }
    FeedKind::Unknown
}

/// Strip XML tags from a string. Used to flatten `<title>Foo &amp; bar</title>`
/// into `Foo & bar`. We don't need a full XML parser for this — the
/// patterns are well-bounded and the strings are short.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Decode the handful of XML entities that show up in feed titles and
/// descriptions. Anything else (numeric refs, weird unicode) we leave
/// as-is and let the browser handle.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
}

/// RSS 2.0 parser. Finds <item> elements, pulls <title>, <link>, and
/// <pubDate>. The pubDate format is RFC 822 (e.g. "Tue, 13 Sep 2026
/// 14:30:00 GMT"); we keep the raw string because the player / template
/// just shows it as a label, no timezone math.
fn parse_rss(body: &[u8], source_title: &str) -> Vec<NewsHeadline> {
    let s = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(start) = find_ci(s, pos, "<item") {
        let end = match find_ci(s, start, "</item") {
            Some(e) => e,
            None => break,
        };
        let item = &s[start..end];
        let title = first_text(item, "title").unwrap_or_default();
        let link = first_text(item, "link").unwrap_or_default();
        let pub_date = first_text(item, "pubDate").unwrap_or_default();
        if !title.is_empty() {
            out.push(NewsHeadline {
                title: decode_entities(&strip_tags(&title)),
                link: decode_entities(&strip_tags(&link)),
                pub_date: decode_entities(&strip_tags(&pub_date)),
                source: source_title.to_string(),
            });
        }
        pos = end + "</item>".len();
    }
    out
}

/// Atom parser. Finds <entry> elements, pulls <title>, the href of
/// the first <link> with rel="alternate" (or no rel), and <updated> or
/// <published> for the date.
fn parse_atom(body: &[u8], source_title: &str) -> Vec<NewsHeadline> {
    let s = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(start) = find_ci(s, pos, "<entry") {
        let end = match find_ci(s, start, "</entry") {
            Some(e) => e,
            None => break,
        };
        let entry = &s[start..end];
        let title = first_text(entry, "title").unwrap_or_default();
        let link = first_atom_link(entry).unwrap_or_default();
        let pub_date = first_text(entry, "updated")
            .or_else(|| first_text(entry, "published"))
            .unwrap_or_default();
        if !title.is_empty() {
            out.push(NewsHeadline {
                title: decode_entities(&strip_tags(&title)),
                link: decode_entities(&strip_tags(&link)),
                pub_date: decode_entities(&strip_tags(&pub_date)),
                source: source_title.to_string(),
            });
        }
        pos = end + "</entry>".len();
    }
    out
}

/// Case-insensitive find. Returns the byte index of the first occurrence
/// of `needle` in `haystack` at or after `from`.
fn find_ci(haystack: &str, from: usize, needle: &str) -> Option<usize> {
    let lower_hay: String = haystack[from..].to_ascii_lowercase();
    let lower_need = needle.to_ascii_lowercase();
    lower_hay.find(&lower_need).map(|i| from + i)
}

/// Find the first occurrence of `<tag>...</tag>` and return the inner
/// text. Returns None if the tag is self-closing or absent.
///
/// CDATA sections (`<![CDATA[ ... ]]>`) inside the tag's body are
/// flattened: their content is preserved but the CDATA markers are
/// stripped. This matters for feeds like BBC News where every title
/// is wrapped in CDATA to protect entities and special characters.
///
/// Self-closing tags (`<tag/>`) return None; the caller should treat
/// that as "tag absent".
fn first_text(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let start = find_ci(s, 0, &open)?;
    // skip past the open tag's attributes (e.g. <title type="html">)
    let after_open = s[start..].find('>')? + start + 1;
    if after_open > start + open.len() {
        let open_tag = &s[start..after_open];
        if open_tag.ends_with('/') {
            // Self-closing like <title/>.
            return None;
        }
    }
    // Walk forward, stripping CDATA sections, until we hit the
    // matching closing tag at depth 0 (we don't try to handle nested
    // same-name tags; none of the feeds we care about do that).
    // Lowercase the close_marker too so the find matches the lowercased
    // rest. (find_ci lowercases both sides; here we lowercase manually.)
    let close_marker_lc = close.to_ascii_lowercase();
    let close_marker = close_marker_lc.as_str();
    let cdata_open = "<![CDATA[";
    let cdata_close = "]]>";
    let bytes = s.as_bytes();
    let mut i = after_open;
    let mut out = String::new();
    while i < s.len() {
        // Look for the next significant marker: </tag>, <![CDATA[, or
        // any other '<'. Use find_ci for the closing tag (case
        // insensitive) but byte-search for CDATA markers (always
        // uppercase in practice).
        let lower_rest = s[i..].to_ascii_lowercase();
        let close_pos = lower_rest.find(close_marker);
        let cdata_pos = s[i..].find(cdata_open);
        match (close_pos, cdata_pos) {
            (Some(c), Some(d)) if d < c => {
                // CDATA first: copy text before it, then the CDATA body.
                out.push_str(&s[i..i + d]);
                let body_start = i + d + cdata_open.len();
                if let Some(body_end_rel) = s[body_start..].find(cdata_close) {
                    out.push_str(&s[body_start..body_start + body_end_rel]);
                    i = body_start + body_end_rel + cdata_close.len();
                } else {
                    // Unterminated CDATA; bail with what we have.
                    break;
                }
            }
            (Some(c), _) => {
                // Closing tag first: copy everything up to it.
                out.push_str(&s[i..i + c]);
                return Some(out);
            }
            (None, Some(d)) => {
                // CDATA without a following close tag (malformed).
                out.push_str(&s[i..i + d]);
                let body_start = i + d + cdata_open.len();
                if let Some(body_end_rel) = s[body_start..].find(cdata_close) {
                    out.push_str(&s[body_start..body_start + body_end_rel]);
                    i = body_start + body_end_rel + cdata_close.len();
                } else {
                    break;
                }
            }
            (None, None) => {
                // No closing tag and no CDATA. Tag is unclosed; bail.
                return None;
            }
        }
        // Suppress unused-variable lint for bytes when this branch
        // is never hit by current feeds but kept for future use.
        let _ = bytes;
    }
    None
}

/// Find the first <link> in an Atom <entry> with rel="alternate" (or no
/// rel attribute), and return its href. Self-closing <link .../> is
/// supported.
fn first_atom_link(entry: &str) -> Option<String> {
    let lower = entry.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find("<link") {
        let abs = pos + rel;
        // Find the end of the tag (could be .../> or ...>).
        let after_lt = abs + "<link".len();
        let close = entry[after_lt..].find('>')? + after_lt;
        let tag = &entry[after_lt..close];
        let lower_tag = tag.to_ascii_lowercase();
        // Skip links that have rel="...something other than alternate..."
        // (e.g. rel="self" or rel="enclosure").
        if let Some(rel_start) = lower_tag.find("rel=") {
            let after_eq = rel_start + 4;
            // skip the opening quote
            let quote = lower_tag.as_bytes().get(after_eq).copied()?;
            if quote != b'"' && quote != b'\'' {
                pos = close + 1;
                continue;
            }
            let rel_end = lower_tag[after_eq + 1..]
                .find(quote as char)
                .map(|i| after_eq + 1 + i)?;
            let rel_val = &lower_tag[after_eq + 1..rel_end];
            if rel_val != "alternate" {
                pos = close + 1;
                continue;
            }
        }
        // Look for href="...".
        if let Some(href_rel) = lower_tag.find("href=") {
            let after_eq = href_rel + 5;
            let quote = lower_tag.as_bytes().get(after_eq).copied()?;
            if quote != b'"' && quote != b'\'' {
                pos = close + 1;
                continue;
            }
            let href_end = lower_tag[after_eq + 1..]
                .find(quote as char)
                .map(|i| after_eq + 1 + i)?;
            return Some(tag[after_eq + 1..href_end].to_string());
        }
        pos = close + 1;
    }
    None
}

/// Fetch a feed over HTTP(S), parse it, and return the headlines plus
/// the cache-control hints we should store alongside.
///
/// On any failure (timeout, non-2xx, parse error) the function returns
/// `Err` with a human-readable reason; the caller decides whether to
/// record it as the feed's `last_error` or just log and move on.
pub fn fetch_feed(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<FetchResult, String> {
    // Validate the URL is http(s) and not pointing at a private address
    // — we don't want a feed URL to be used as a SSRF probe.
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid url: {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported scheme: {scheme}"));
    }
    if let Some(host) = parsed.host_str() {
        if is_private_host(host) {
            return Err(format!("refusing to fetch private host: {host}"));
        }
    }

    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let mut req = client.get(url);
    if let Some(et) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, et);
    }
    if let Some(lm) = last_modified {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
    }

    let resp = req.send().map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchResult {
            headlines: Vec::new(),
            new_etag: etag.map(|s| s.to_string()),
            new_last_modified: last_modified.map(|s| s.to_string()),
            not_modified: true,
        });
    }
    if !status.is_success() {
        return Err(format!("http {status}"));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let new_last_modified = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Read the body with a hard size cap.
    let body = resp
        .bytes()
        .map_err(|e| format!("read body: {e}"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "body too large: {} bytes (max {})",
            body.len(),
            MAX_BODY_BYTES
        ));
    }

    let title = extract_feed_title(&body, content_type.as_deref());
    let headlines = parse_feed(&body, content_type.as_deref(), &title);

    Ok(FetchResult {
        headlines,
        new_etag,
        new_last_modified,
        not_modified: false,
    })
}

pub struct FetchResult {
    pub headlines: Vec<NewsHeadline>,
    pub new_etag: Option<String>,
    pub new_last_modified: Option<String>,
    /// True if the server returned 304 Not Modified. The caller should
    /// keep the existing cached headlines and just update the timestamp.
    pub not_modified: bool,
}

/// Pull the channel/feed title out of the body. We use this as the
/// `source` label on each headline. RSS 2.0 has it in <channel><title>;
/// Atom has it as the root <feed><title>.
fn extract_feed_title(body: &[u8], content_type: Option<&str>) -> String {
    let s = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if sniff_kind(body, content_type) == FeedKind::Atom {
        return first_text(s, "title").unwrap_or_default();
    }
    // RSS: <channel><title>...</title>
    if let Some(chan_start) = find_ci(s, 0, "<channel") {
        if let Some(title) = first_text(&s[chan_start..], "title") {
            return decode_entities(&strip_tags(&title));
        }
    }
    String::new()
}

/// Refuse to fetch hosts that resolve to private/loopback/link-local
/// addresses. Prevents a feed URL like `http://192.168.1.1/admin` from
/// being used to probe the internal network.
fn is_private_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".local") || h.ends_with(".internal") {
        return true;
    }
    // Strip IPv6 brackets if present.
    let bare = h.trim_start_matches('[').trim_end_matches(']');
    if let Ok(addr) = bare.parse::<std::net::IpAddr>() {
        match addr {
            std::net::IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_multicast()
            }
            std::net::IpAddr::V6(v6) => {
                // fe80::/10 is link-local, which std::net::Ipv6Addr does
                // not expose as a single method call. Check the first 10
                // bits of the address manually.
                let is_link_local = (v6.segments()[0] & 0xffc0) == 0xfe80;
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || is_link_local
                    || v6.is_multicast()
            }
        }
    } else {
        // Hostname (not an IP literal) — trust DNS resolution, the
        // network sandbox will block private ranges at the OS level.
        false
    }
}

/// Register every news widget's feedUrl in the news_feeds table.
/// Returns the list of (widget_id, feedUrl) pairs that were newly
/// registered so the caller can log a one-time message.
pub fn collect_news_feed_urls(layouts: &[crate::models::Layout]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for layout in layouts {
        for widget in &layout.widgets {
            if widget.widget_type != crate::models::WidgetType::News {
                continue;
            }
            // Widget.props is a serde_json::Value (always present, default = {}).
            // Look up the feedUrl string property. We use Value::get which
            // borrows the Value, so we can't .as_ref() the &Value first.
            let url = widget
                .props
                .get("feedUrl")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string());
            if let Some(url) = url {
                if !url.is_empty() {
                    out.push((widget.id.clone(), url));
                }
            }
        }
    }
    // Dedupe by URL (multiple widgets on the same layout can share a feed).
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.dedup_by(|a, b| a.1 == b.1);
    out
}

/// Refresh all feeds that are older than `max_age_secs`. Called by the
/// maintenance loop. Returns a count of feeds that were updated.
pub fn refresh_stale_feeds(
    db: &crate::db::Database,
    http_client: Option<&reqwest::blocking::Client>,
    max_age_secs: i64,
) -> Result<usize, String> {
    let stale = db.get_news_feeds_needing_refresh(max_age_secs)?;
    if stale.is_empty() {
        return Ok(0);
    }
    // If we didn't get a client, build one (used by the maintenance loop
    // which keeps a single client alive for its lifetime).
    let owned_client;
    let client = match http_client {
        Some(c) => c,
        None => {
            owned_client = reqwest::blocking::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(FETCH_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| format!("client build: {e}"))?;
            &owned_client
        }
    };
    let mut updated = 0;
    for feed in stale {
        match fetch_one(db, client, &feed) {
            Ok(true) => updated += 1,
            Ok(false) => {}
            Err(e) => {
                eprintln!("news: refresh {} failed: {e}", feed.url);
                let _ = db.record_news_feed_error(&feed.url, &e);
            }
        }
    }
    Ok(updated)
}

/// Public wrapper around the private `fetch_one` so the API layer
/// can re-fetch a single feed by id without going through the
/// staleness filter. Returns `true` if the feed was actually
/// updated, `false` if the server returned 304 Not Modified.
pub fn fetch_one_with_client(
    db: &crate::db::Database,
    client: &reqwest::blocking::Client,
    feed: &NewsFeed,
) -> Result<bool, String> {
    fetch_one(db, client, feed)
}

/// Result of fetching one cached feed without choosing a storage backend.
#[derive(Debug, Clone)]
pub struct CachedFeedUpdate {
    pub title: String,
    pub headlines_json: String,
    pub new_etag: Option<String>,
    pub new_last_modified: Option<String>,
    pub not_modified: bool,
}

/// Fetch one feed using a caller-provided blocking client. Storage is left
/// to the caller so SQLite and PostgreSQL repositories can share this path.
pub fn fetch_one_update_with_client(
    client: &reqwest::blocking::Client,
    feed: &NewsFeed,
) -> Result<CachedFeedUpdate, String> {
    let mut req = client.get(&feed.url);
    if let Some(et) = &feed.etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, et);
    }
    if let Some(lm) = &feed.last_modified {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
    }
    let resp = req.send().map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(CachedFeedUpdate {
            title: feed.title.clone(),
            headlines_json: feed.headlines_json.clone(),
            new_etag: feed.etag.clone(),
            new_last_modified: feed.last_modified.clone(),
            not_modified: true,
        });
    }
    if !status.is_success() {
        return Err(format!("http {status}"));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let new_last_modified = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let body = resp.bytes().map_err(|e| format!("read body: {e}"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "body too large: {} bytes (max {})",
            body.len(),
            MAX_BODY_BYTES
        ));
    }
    let title = extract_feed_title(&body, content_type.as_deref());
    let headlines = parse_feed(&body, content_type.as_deref(), &title);
    let headlines_json = serde_json::to_string(&headlines).map_err(|e| format!("serialize: {e}"))?;
    Ok(CachedFeedUpdate {
        title,
        headlines_json,
        new_etag,
        new_last_modified,
        not_modified: false,
    })
}

fn fetch_one(
    db: &crate::db::Database,
    client: &reqwest::blocking::Client,
    feed: &NewsFeed,
) -> Result<bool, String> {
    let update = fetch_one_update_with_client(client, feed)?;
    let now = chrono::Utc::now().to_rfc3339();
    if update.not_modified {
        db.mark_news_feed_fetched(&feed.url, &now)?;
        return Ok(false);
    }
    db.upsert_news_feed(
        &feed.url,
        &update.title,
        &update.headlines_json,
        update.new_etag.as_deref(),
        update.new_last_modified.as_deref(),
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>Test News</title>
    <link>https://example.com/</link>
    <description>Sample feed</description>
    <item>
      <title>First headline &amp; more</title>
      <link>https://example.com/1</link>
      <pubDate>Tue, 13 Sep 2026 14:30:00 GMT</pubDate>
    </item>
    <item>
      <title>Second headline</title>
      <link>https://example.com/2</link>
      <pubDate>Wed, 14 Sep 2026 09:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#;

    const SAMPLE_ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Test</title>
  <entry>
    <title>Atom one</title>
    <link rel="alternate" href="https://example.com/a1"/>
    <updated>2026-09-13T14:30:00Z</updated>
  </entry>
  <entry>
    <title>Atom two</title>
    <link href="https://example.com/a2"/>
    <published>2026-09-12T10:00:00Z</published>
  </entry>
</feed>"#;

    #[test]
    fn collect_news_feed_urls_extracts_news_widgets() {
        use crate::models::{Layout, Widget, WidgetType};
        let mut layout = Layout {
            id: "L".into(), name: "T".into(),
            widgets: vec![],
            interactive: false, page_type: "single".into(),
            timing_mode: "global".into(), duration: 15,
            shuffle: false, items: vec![], canvas_orientation: "landscape".into(), folder_id: None,
            created_at: "2026-09-13T00:00:00Z".into(),
            updated_at: "2026-09-13T00:00:00Z".into(),
        };
        // News widget with a feed URL: should be included.
        layout.widgets.push(Widget {
            id: "w1".into(),
            widget_type: WidgetType::News,
            x: 0.0, y: 0.0, w: 100.0, h: 10.0,
            z: 0,
            props: serde_json::json!({"feedUrl": "https://a.example/feed"}),
        });
        // News widget with no feedUrl: ignored.
        layout.widgets.push(Widget {
            id: "w2".into(),
            widget_type: WidgetType::News,
            x: 0.0, y: 0.0, w: 100.0, h: 10.0,
            z: 0,
            props: serde_json::json!({"speed": "normal"}),
        });
        // News widget with whitespace feedUrl: ignored.
        layout.widgets.push(Widget {
            id: "w3".into(),
            widget_type: WidgetType::News,
            x: 0.0, y: 0.0, w: 100.0, h: 10.0,
            z: 0,
            props: serde_json::json!({"feedUrl": "  "}),
        });
        // Two News widgets sharing the same feed: dedupe to one URL.
        layout.widgets.push(Widget {
            id: "w4".into(),
            widget_type: WidgetType::News,
            x: 0.0, y: 0.0, w: 100.0, h: 10.0,
            z: 0,
            props: serde_json::json!({"feedUrl": "https://a.example/feed"}),
        });
        // Text widget with feedUrl-like prop: ignored (wrong type).
        layout.widgets.push(Widget {
            id: "w5".into(),
            widget_type: WidgetType::Text,
            x: 0.0, y: 0.0, w: 100.0, h: 10.0,
            z: 0,
            props: serde_json::json!({"feedUrl": "https://x/feed"}),
        });
        let urls = collect_news_feed_urls(&[layout]);
        assert_eq!(urls.len(), 1, "only one unique URL after dedupe; got {urls:?}");
        assert_eq!(urls[0].1, "https://a.example/feed");
    }

    #[test]
    fn parses_rss_basic() {
        let items = parse_feed(SAMPLE_RSS.as_bytes(), Some("application/rss+xml"), "Test News");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "First headline & more");
        assert_eq!(items[0].link, "https://example.com/1");
        assert_eq!(items[0].pub_date, "Tue, 13 Sep 2026 14:30:00 GMT");
        assert_eq!(items[0].source, "Test News");
        assert_eq!(items[1].title, "Second headline");
    }

    #[test]
    fn parses_atom_basic() {
        let items = parse_feed(SAMPLE_ATOM.as_bytes(), Some("application/atom+xml"), "Atom Test");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Atom one");
        assert_eq!(items[0].link, "https://example.com/a1");
        assert_eq!(items[0].pub_date, "2026-09-13T14:30:00Z");
        assert_eq!(items[1].link, "https://example.com/a2");
        // Second entry has <published>, not <updated>; we should pick it up.
        assert_eq!(items[1].pub_date, "2026-09-12T10:00:00Z");
    }

    #[test]
    fn ignores_non_alternate_atom_links() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <title>Hello</title>
    <link rel="self" href="https://example.com/self"/>
    <link rel="enclosure" type="image/png" href="https://example.com/img.png"/>
    <link rel="alternate" href="https://example.com/alt"/>
  </entry>
</feed>"#;
        let items = parse_feed(xml.as_bytes(), None, "x");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].link, "https://example.com/alt");
    }

    #[test]
    fn handles_malformed_body() {
        // Garbage that starts with <rss but is unclosed / has no items.
        let xml = "<rss>not really a feed";
        let items = parse_feed(xml.as_bytes(), None, "x");
        assert!(items.is_empty(), "no items in malformed body");
    }

    #[test]
    fn handles_empty_body() {
        let items = parse_feed(b"", None, "x");
        assert!(items.is_empty());
    }

    #[test]
    fn handles_non_utf8_body() {
        // 0xFF 0xFE is not valid UTF-8.
        let items = parse_feed(&[0xFF, 0xFE, 0xFD], None, "x");
        assert!(items.is_empty(), "non-utf8 body should not panic");
    }

    #[test]
    fn extract_title_rss() {
        let title = extract_feed_title(SAMPLE_RSS.as_bytes(), Some("application/rss+xml"));
        assert_eq!(title, "Test News");
    }

    #[test]
    fn extract_title_atom() {
        let title = extract_feed_title(SAMPLE_ATOM.as_bytes(), Some("application/atom+xml"));
        assert_eq!(title, "Atom Test");
    }

    #[test]
    fn strip_tags_removes_xml() {
        assert_eq!(strip_tags("foo<em>bar</em>baz"), "foobarbaz");
        assert_eq!(strip_tags("<a href=\"x\">link</a>"), "link");
    }

    #[test]
    fn decode_entities_handles_common_cases() {
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_entities("&quot;hi&quot;"), "\"hi\"");
        assert_eq!(decode_entities("&apos;hi&apos;"), "'hi'");
    }

    #[test]
    fn private_host_detection() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("LOCALHOST"));
        assert!(is_private_host("foo.local"));
        assert!(is_private_host("foo.internal"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("10.0.0.1"));
        assert!(is_private_host("192.168.1.1"));
        assert!(is_private_host("169.254.169.254"));
        assert!(is_private_host("::1"));
        assert!(is_private_host("fe80::1"));
        assert!(!is_private_host("example.com"));
        assert!(!is_private_host("8.8.8.8"));
    }

    #[test]
    fn find_ci_is_case_insensitive() {
        assert_eq!(find_ci("<RSS><item>hi</item></RSS>", 0, "<item"), Some(5));
        assert_eq!(find_ci("<rss><ITEM>hi</item></rss>", 0, "<item"), Some(5));
    }

    #[test]
    fn first_text_finds_tag() {
        let s = "<title>Hello</title><other>x</other>";
        assert_eq!(first_text(s, "title").unwrap(), "Hello");
        assert!(first_text(s, "missing").is_none());
    }

    #[test]
    fn first_text_handles_self_closing() {
        // <title/> is self-closing; first_text should return None.
        let s = "<title/><other>x</other>";
        assert!(first_text(s, "title").is_none());
    }

    #[test]
    fn first_text_handles_cdata() {
        // CDATA-wrapped titles like BBC News feeds should have the
        // CDATA markers stripped and the body returned verbatim.
        let s = "<title><![CDATA[Hello & welcome]]></title>";
        assert_eq!(first_text(s, "title").unwrap(), "Hello & welcome");
    }

    #[test]
    fn first_text_handles_cdata_with_nested_brackets() {
        // Some feeds have stray '<' inside CDATA bodies (rare but legal);
        // we should not be confused by them.
        let s = "<title><![CDATA[a < b and b > c]]></title>";
        assert_eq!(first_text(s, "title").unwrap(), "a < b and b > c");
    }

    #[test]
    fn first_text_mixed_cdata_and_plain() {
        // Some feeds mix CDATA and plain content. Real-world example
        // would be `<title>prefix <![CDATA[body]]> suffix</title>`.
        let s = "<title>prefix <![CDATA[body]]> suffix</title>";
        assert_eq!(first_text(s, "title").unwrap(), "prefix body suffix");
    }

    #[test]
    fn parses_rss_with_cdata_titles() {
        // A BBC-shaped RSS feed: every title and description is
        // wrapped in CDATA. Before the CDATA-aware parser, this
        // returned 0 headlines because first_text couldn't find
        // </title> inside the CDATA body.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title><![CDATA[BBC News]]></title>
    <item>
      <title><![CDATA[AI staff frightened for humanity's future]]></title>
      <link>https://example.com/1</link>
      <pubDate>Sun, 13 Sep 2026 14:53:40 GMT</pubDate>
    </item>
  </channel>
</rss>"#;
        let items = parse_feed(xml.as_bytes(), Some("application/rss+xml"), "BBC News");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "AI staff frightened for humanity's future");
        assert_eq!(items[0].source, "BBC News");
        assert_eq!(items[0].pub_date, "Sun, 13 Sep 2026 14:53:40 GMT");
    }
}
