//! The web as text (one-loop design §1b): a page's words and a search's results, for a model that
//! reads far better than it looks at pixels. Through curl, which the installer puts in, so the
//! executor takes no HTTP library of its own.

use std::process::Command;

/// The page at `url`, as fetched. Only http(s): anything else is a file, and files have read_file.
pub fn fetch(url: &str) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("{url} is not a web address: give one starting with https://"));
    }
    let o = Command::new("curl")
        .args(["-sSL", "--compressed", "--max-time", "30", "--max-filesize", "5000000", "-A", "Mozilla/5.0 (X11; Linux x86_64) ai-os", url])
        .output().map_err(|e| format!("cannot run curl ({e}); the installer puts it in"))?;
    if !o.status.success() { return Err(format!("could not read {url}: {}", String::from_utf8_lossy(&o.stderr).trim())); }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Tags that start a new line of text.
const BLOCKS: [&str; 22] = ["p", "div", "br", "li", "h1", "h2", "h3", "h4", "h5", "h6", "tr", "td", "th",
    "section", "article", "header", "footer", "ul", "ol", "table", "pre", "blockquote"];

/// A page as (title, text): scripts, styles and comments dropped, a link kept as `words <address>`.
/// ponytail: a hand scanner, not an HTML parser; a page built by script shows little, and
/// web_read says what it got — the screen is there for the rest.
pub fn text_of(html: &str) -> (String, String) {
    // ASCII lowercasing keeps every byte where it was, so indexes into `lower` fit `html`.
    let lower = html.to_ascii_lowercase();
    let title = lower.find("<title").and_then(|s| lower[s..].find('>').map(|e| s + e + 1))
        .and_then(|s| lower[s..].find("</title>").map(|e| decode(html[s..s + e].trim())))
        .unwrap_or_default();
    let mut out = String::new();
    let mut href: Option<String> = None;
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        if rest.starts_with("<!--") { i += rest.find("-->").map_or(rest.len(), |e| e + 3); continue; }
        if rest.starts_with('<') {
            let end = rest.find('>').map_or(rest.len(), |e| e + 1);
            let tag = &lower[i..i + end];
            let closing = tag.starts_with("</");
            let name: String = tag.trim_start_matches('<').trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if !closing && ["script", "style", "noscript", "svg", "template", "title"].contains(&name.as_str()) {
                let close = format!("</{name}");
                match lower[i..].find(&close) {
                    Some(e) => i += e + lower[i + e..].find('>').map_or(close.len(), |g| g + 1),
                    None => i = html.len(),
                }
                continue;
            }
            if name == "a" {
                if closing { if let Some(h) = href.take() { out.push_str(&format!(" <{h}>")); } }
                else { href = attr(&html[i..i + end], "href").filter(|h| h.starts_with("http") || h.starts_with('/')); }
            }
            if BLOCKS.contains(&name.as_str()) { out.push('\n'); }
            i += end;
            continue;
        }
        let next = rest.find('<').unwrap_or(rest.len());
        out.push_str(&decode(&rest[..next].replace(['\n', '\r', '\t'], " ")));
        i += next;
    }
    (title, tidy(&out))
}

/// One attribute's value from a tag, quotes or none.
fn attr(tag: &str, name: &str) -> Option<String> {
    let at = tag.to_ascii_lowercase().find(&format!("{name}="))? + name.len() + 1;
    let v = &tag[at..];
    let (q, body) = match v.chars().next()? { c @ ('"' | '\'') => (c, &v[1..]), _ => (' ', v) };
    let end = body.find(|c: char| c == q || (q == ' ' && (c == '>' || c.is_whitespace()))).unwrap_or(body.len());
    Some(decode(&body[..end]))
}

/// The entities a page's text actually uses; an unknown one stays as written.
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(a) = rest.find('&') {
        out.push_str(&rest[..a]);
        rest = &rest[a..];
        let semi = rest.char_indices().take(10).find(|(_, c)| *c == ';').map(|(i, _)| i);
        let ch = semi.and_then(|semi| {
            let ent = &rest[1..semi];
            match ent {
                "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'),
                "apos" | "#39" => Some('\''), "nbsp" => Some(' '),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
                _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            }.map(|c| (c, semi))
        });
        match ch {
            Some((c, semi)) => { out.push(c); rest = &rest[semi + 1..]; }
            None => { out.push('&'); rest = &rest[1..]; }
        }
    }
    out.push_str(rest);
    out
}

/// Spaces collapsed, blank lines at most one in a row.
fn tidy(s: &str) -> String {
    let mut lines: Vec<String> = vec![];
    for l in s.lines() {
        let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() && lines.last().map_or(true, |p| p.is_empty()) { continue; }
        lines.push(l);
    }
    lines.join("\n").trim().to_string()
}

/// DuckDuckGo's plain HTML page for `query`.
pub fn search_url(query: &str) -> String {
    let q: String = query.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!("https://html.duckduckgo.com/html/?q={q}")
}

/// `%XX` and `+` back to what they stand for; a bad escape stays as written.
pub fn unpercent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v); i += 3; continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Up to 8 (title, address, snippet) from DuckDuckGo's HTML page.
/// ponytail: scraped from one engine's page; a changed page gives none, and the caller says so.
pub fn results(html: &str) -> Vec<(String, String, String)> {
    let mut out = vec![];
    for p in html.split("result__a").skip(1) {
        let Some(gt) = p.find('>') else { continue };
        let Some(href) = attr(&p[..gt], "href") else { continue };
        let url = match href.find("uddg=") { Some(a) => unpercent(href[a + 5..].split('&').next().unwrap_or("")), None => href };
        let title = p[gt + 1..].split("</a>").next().map(|t| text_of(t).1).unwrap_or_default();
        let snippet = p.find("result__snippet").and_then(|s| {
            let r = &p[s..];
            r.find('>').map(|g| r[g + 1..].split("</a>").next().unwrap_or("").to_string())
        }).map(|t| text_of(&t).1).unwrap_or_default();
        if !title.is_empty() { out.push((title, url, snippet)); }
        if out.len() == 8 { break; }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_becomes_its_words_with_links() {
        let html = r#"<html><head><title>Flappy &amp; Friends</title><style>p{color:red}</style></head>
<body><script>var x = "<p>no</p>";</script><h1>Birds</h1><p>Glide   far,
flap <b>hard</b>.</p><!-- hidden --><p>See <a href="https://example.org/more">more birds</a> &lt;here&gt;&nbsp;now</p></body></html>"#;
        let (title, text) = text_of(html);
        assert_eq!(title, "Flappy & Friends");
        assert!(text.contains("Birds"), "{text}");
        assert!(text.contains("Glide far, flap hard."), "{text}");
        assert!(text.contains("more birds <https://example.org/more>"), "{text}");
        assert!(text.contains("<here> now"), "{text}");
        assert!(!text.contains("color:red") && !text.contains("var x") && !text.contains("hidden"), "{text}");
    }

    #[test]
    fn search_results_are_title_address_and_snippet() {
        let html = r#"<div class="result"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fflappy%3Fa%3D1&amp;rut=abc">Flappy <b>Bird</b> clone</a>
<a class="result__snippet" href="x">Build a <b>flappy</b> game in JS.</a></div>
<div class="result"><a class="result__a" href="https://plain.example/">Plain</a></div>"#;
        let r = results(html);
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(r[0], ("Flappy Bird clone".to_string(), "https://example.org/flappy?a=1".to_string(), "Build a flappy game in JS.".to_string()));
        assert_eq!(r[1].1, "https://plain.example/");
    }

    #[test]
    fn a_query_is_escaped_for_the_address() {
        assert_eq!(search_url("flappy bird & more"), "https://html.duckduckgo.com/html/?q=flappy%20bird%20%26%20more");
        assert_eq!(unpercent("a%2Fb+c%zz"), "a/b c%zz");
    }

    #[test]
    fn only_web_addresses_are_fetched() {
        assert!(fetch("file:///etc/passwd").unwrap_err().contains("https://"));
    }
}
