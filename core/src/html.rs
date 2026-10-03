//! HTML mail without a browser engine: the document becomes a list of styled text runs
//! (bold, italic, links, headings, quotes, lists, monospace) and images, which the reader
//! lays out in a text view. Scripts, styles and hidden parts are dropped; nothing is fetched
//! here. The same runs flattened give the text used for search and for quoting.

use scraper::{Html, Node};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub small: bool,
    /// 1..=6, 0 for none
    pub heading: u8,
    /// blockquote depth
    pub quote: u8,
    /// list depth
    pub indent: u8,
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Run {
    Text(String, Style),
    Image { src: String, alt: String, width: Option<i32>, height: Option<i32>, link: Option<String> },
    Rule,
}

struct W {
    out: Vec<Run>,
    space: bool,
    line_start: bool,
    newlines: u8,
    pre: u32,
}

impl W {
    fn push(&mut self, s: &str, st: &Style) {
        if s.is_empty() {
            return;
        }
        if let Some(Run::Text(t, ps)) = self.out.last_mut() {
            if ps == st {
                t.push_str(s);
                return;
            }
        }
        self.out.push(Run::Text(s.to_string(), st.clone()));
    }

    fn text(&mut self, s: &str, st: &Style) {
        if self.pre > 0 {
            let s = s.replace("\r\n", "\n");
            if !s.is_empty() {
                self.push(&s, st);
                self.newlines = if s.ends_with('\n') { 1 } else { 0 };
                self.line_start = s.ends_with('\n');
                self.space = false;
            }
            return;
        }
        let mut buf = String::new();
        for c in s.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                self.space = true;
                continue;
            }
            if c == '\u{200c}' || c == '\u{200b}' || c == '\u{feff}' || c == '\u{34f}' || c == '\u{ad}' {
                continue;
            }
            if self.space && !self.line_start {
                if buf.is_empty() {
                    // the space between two runs belongs to neither link nor underline
                    let plain = Style { link: None, underline: false, strike: false, ..st.clone() };
                    self.push(" ", &plain);
                } else {
                    buf.push(' ');
                }
            }
            self.space = false;
            self.line_start = false;
            self.newlines = 0;
            buf.push(if c == '\u{a0}' { ' ' } else { c });
        }
        // spaces between runs of different style must not be lost
        self.push(&buf, st);
    }

    fn brk(&mut self, n: u8) {
        if self.out.is_empty() {
            self.line_start = true;
            self.space = false;
            return;
        }
        while self.newlines < n {
            self.push("\n", &Style::default());
            self.newlines += 1;
        }
        self.line_start = true;
        self.space = false;
    }

    fn hard_break(&mut self) {
        self.push("\n", &Style::default());
        self.newlines = (self.newlines + 1).min(2);
        self.line_start = true;
        self.space = false;
    }
}

fn hidden(el: &scraper::node::Element) -> bool {
    if el.attr("hidden").is_some() || el.attr("aria-hidden") == Some("true") && el.name() == "div" {
        return true;
    }
    if let Some(style) = el.attr("style") {
        let s: String = style.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        if s.contains("display:none") || s.contains("mso-hide:all") || s.contains("visibility:hidden") {
            return true;
        }
        if (s.contains("max-height:0") || s.contains("height:0")) && s.contains("overflow:hidden") {
            return true;
        }
        if s.contains("font-size:0") && s.contains("opacity:0") {
            return true;
        }
    }
    false
}

fn dim(v: Option<&str>) -> Option<i32> {
    v.and_then(|s| s.trim().trim_end_matches("px").parse::<f32>().ok()).map(|f| f as i32)
}

fn walk(node: ego_tree::NodeRef<Node>, w: &mut W, st: &Style, list: &mut Vec<Option<u32>>, cell: &mut bool) {
    match node.value() {
        Node::Text(t) => w.text(t, st),
        Node::Element(el) => {
            let name = el.name();
            if matches!(
                name,
                "head" | "style" | "script" | "title" | "meta" | "link" | "template" | "noscript" | "svg" | "object" | "embed" | "iframe" | "input" | "select" | "option" | "textarea" | "map" | "area" | "base"
            ) {
                return;
            }
            if hidden(el) {
                return;
            }
            let mut s = st.clone();
            let mut after = 0u8;
            match name {
                "br" => {
                    w.hard_break();
                    return;
                }
                "hr" => {
                    w.brk(1);
                    w.out.push(Run::Rule);
                    w.newlines = 0;
                    w.brk(1);
                    return;
                }
                "img" => {
                    let width = dim(el.attr("width"));
                    let height = dim(el.attr("height"));
                    if width.map(|x| x <= 2).unwrap_or(false) && height.map(|x| x <= 2).unwrap_or(false) {
                        return; // a tracking pixel
                    }
                    let src = el.attr("src").unwrap_or("").trim().to_string();
                    if src.is_empty() {
                        return;
                    }
                    w.out.push(Run::Image { src, alt: el.attr("alt").unwrap_or("").trim().to_string(), width, height, link: st.link.clone() });
                    w.line_start = false;
                    w.newlines = 0;
                    return;
                }
                "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "dl" | "figure" => {
                    w.brk(2);
                    after = 2;
                }
                "div" | "section" | "article" | "header" | "footer" | "nav" | "aside" | "main" | "center" | "form" | "fieldset" | "address" | "figcaption" | "details" | "summary" | "dt" | "dd" | "caption" | "table" | "tbody" | "thead" | "tfoot" => {
                    w.brk(1);
                    after = 1;
                }
                "pre" => {
                    w.brk(1);
                    after = 1;
                    s.mono = true;
                }
                "ul" | "ol" => {
                    w.brk(1);
                    after = 1;
                }
                "tr" => {
                    w.brk(1);
                    after = 1;
                    *cell = false;
                }
                "td" | "th" => {
                    if *cell && !w.line_start {
                        w.push("  ", st);
                    }
                    *cell = true;
                }
                "li" => {
                    w.brk(1);
                    after = 1;
                }
                _ => {}
            }
            match name {
                "b" | "strong" | "th" => s.bold = true,
                "i" | "em" | "cite" | "dfn" | "var" => s.italic = true,
                "u" | "ins" => s.underline = true,
                "s" | "strike" | "del" => s.strike = true,
                "code" | "tt" | "kbd" | "samp" | "pre" => s.mono = true,
                "small" => s.small = true,
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    s.heading = name[1..].parse().unwrap_or(1);
                    s.bold = true;
                }
                "blockquote" => s.quote += 1,
                "ul" | "ol" | "dd" => s.indent += 1,
                "a" => {
                    if let Some(h) = el.attr("href") {
                        let h = h.trim();
                        if !h.is_empty() && !h.to_lowercase().starts_with("javascript:") && !h.starts_with('#') {
                            s.link = Some(h.to_string());
                        }
                    }
                }
                _ => {}
            }
            if name == "ul" {
                list.push(None);
            } else if name == "ol" {
                list.push(Some(el.attr("start").and_then(|x| x.parse().ok()).unwrap_or(1)));
            } else if name == "li" {
                let marker = match list.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                let ms = Style { indent: s.indent.max(1), ..Style::default() };
                w.push(&marker, &ms);
                w.line_start = true;
                w.space = false;
                w.newlines = 0;
            }
            if name == "pre" {
                w.pre += 1;
            }
            let mut inner_cell = false;
            for c in node.children() {
                if name == "tr" {
                    walk(c, w, &s, list, cell);
                } else if name == "table" || name == "tbody" || name == "thead" || name == "tfoot" {
                    walk(c, w, &s, list, &mut inner_cell);
                } else {
                    walk(c, w, &s, list, cell);
                }
            }
            if name == "pre" {
                w.pre -= 1;
            }
            if name == "ul" || name == "ol" {
                list.pop();
            }
            if after > 0 {
                w.brk(after);
            }
        }
        _ => {
            for c in node.children() {
                walk(c, w, st, list, cell);
            }
        }
    }
}

/// The document as runs. Text runs carry their style; newlines are runs of their own.
pub fn render(html: &str) -> Vec<Run> {
    let doc = Html::parse_document(html);
    let mut w = W { out: Vec::new(), space: false, line_start: true, newlines: 2, pre: 0 };
    let mut list = Vec::new();
    let mut cell = false;
    walk(doc.tree.root(), &mut w, &Style::default(), &mut list, &mut cell);
    // no trailing blank lines
    while let Some(Run::Text(t, _)) = w.out.last() {
        if t.trim().is_empty() {
            w.out.pop();
        } else {
            break;
        }
    }
    if let Some(Run::Text(t, _)) = w.out.last_mut() {
        let trimmed = t.trim_end().to_string();
        *t = trimmed;
    }
    w.out
}

/// Plain text: what the runs say, images as their alt text.
pub fn to_text(html: &str) -> String {
    runs_text(&render(html))
}

pub fn runs_text(runs: &[Run]) -> String {
    let mut out = String::new();
    for r in runs {
        match r {
            Run::Text(t, _) => out.push_str(t),
            Run::Image { alt, .. } => {
                if !alt.is_empty() {
                    out.push('[');
                    out.push_str(alt);
                    out.push(']');
                }
            }
            Run::Rule => out.push_str("\n――――――――\n"),
        }
    }
    let mut res = String::new();
    let mut nl = 0;
    for c in out.chars() {
        if c == '\n' {
            nl += 1;
            if nl <= 2 {
                res.push(c);
            }
        } else {
            nl = 0;
            res.push(c);
        }
    }
    res.trim().to_string()
}

/// Whether the document wants anything from the network (http images).
pub fn has_remote(runs: &[Run]) -> bool {
    runs.iter().any(|r| matches!(r, Run::Image { src, .. } if src.starts_with("http://") || src.starts_with("https://") || src.starts_with("//")))
}

/// A copy for the browser: scripts out, a CSP that blocks everything but inline styles and
/// data: images (and the network for images when [online]); cid: images become data: URIs.
pub fn sandboxed(html: &str, cid: &dyn Fn(&str) -> Option<(String, Vec<u8>)>, online: bool) -> String {
    use base64::Engine as _;
    let script = regex::Regex::new(r"(?is)<script\b.*?</script\s*>").unwrap();
    let handlers = regex::Regex::new(r#"(?i)\son[a-z]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap();
    let meta_refresh = regex::Regex::new(r#"(?i)<meta[^>]+http-equiv\s*=\s*["']?refresh[^>]*>"#).unwrap();
    let cids = regex::Regex::new(r#"(?i)(src\s*=\s*["']?)cid:([^"'\s>]+)"#).unwrap();
    let mut s = script.replace_all(html, "").into_owned();
    s = handlers.replace_all(&s, "").into_owned();
    s = meta_refresh.replace_all(&s, "").into_owned();
    s = cids
        .replace_all(&s, |c: &regex::Captures| match cid(&c[2]) {
            Some((mime, data)) => format!("{}data:{};base64,{}", &c[1], mime, base64::engine::general_purpose::STANDARD.encode(data)),
            None => format!("{}about:blank", &c[1]),
        })
        .into_owned();
    let img = if online { "data: https: http:" } else { "data:" };
    let csp = format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src {img}; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'\"><meta name=\"referrer\" content=\"no-referrer\">"
    );
    let head = regex::Regex::new(r"(?i)<head[^>]*>").unwrap();
    if let Some(m) = head.find(&s) {
        let at = m.end();
        s.insert_str(at, &csp);
        s
    } else {
        format!("<!doctype html><html><head><meta charset=\"utf-8\">{csp}</head><body>{s}</body></html>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_links_lists() {
        let runs = render("<html><head><style>p{}</style></head><body><p>Hello <b>you</b>,</p><p>see <a href='https://x.org/a'>this</a>.</p><ul><li>one</li><li>two</li></ul><div style='display:none'>secret preheader</div><img src='https://t.co/p.gif' width=1 height=1></body></html>");
        let t = runs_text(&runs);
        assert_eq!(t, "Hello you,\n\nsee this.\n\n• one\n• two");
        assert!(runs.iter().any(|r| matches!(r, Run::Text(t, s) if t.trim() == "you" && s.bold)));
        assert!(runs.iter().any(|r| matches!(r, Run::Text(t, s) if t == "this" && s.link.as_deref() == Some("https://x.org/a"))));
        assert!(!has_remote(&runs));
    }

    #[test]
    fn tables_and_breaks() {
        let t = to_text("<table><tr><td>a</td><td>b</td></tr><tr><td>c<br>d</td></tr></table><pre>x  y\n z</pre>");
        assert_eq!(t, "a  b\nc\nd\nx  y\n z");
    }

    #[test]
    fn sandbox() {
        let s = sandboxed("<html><head></head><body onload='x()'><script>alert(1)</script><img src=\"cid:a1\"></body></html>", &|c| if c == "a1" { Some(("image/png".into(), vec![1, 2])) } else { None }, false);
        assert!(!s.contains("<script"));
        assert!(!s.contains("onload"));
        assert!(s.contains("data:image/png;base64,AQI="));
        assert!(s.contains("default-src 'none'"));
    }
}
