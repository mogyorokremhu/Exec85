//! The Exec85 browser's engine: addresses, the list of allowed sites, a small HTML reader and layout, and the fetching.
//!
//! It is deliberately small. It shows text pages (headings, paragraphs, links, lists, tables, code) and downloads files.
//! There is no JavaScript, no style sheets and no pictures, so big modern sites do not work, on purpose. It only visits
//! sites on the allowed list (checked again at every redirect), only over HTTPS, and the secure connection is made by
//! `curl`, which checks certificates properly.
use std::path::{Path, PathBuf};
use std::process::Command;

pub const ALLOW_FILE: &str = "/etc/exec85/sites.allow";
pub const MAX_PAGE_BYTES: u64 = 3_000_000;
pub const MAX_HOPS: usize = 5;

// ---- addresses ------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url { pub host: String, pub path: String }

impl Url {
    pub fn text(&self) -> String { format!("https://{}{}", self.host, self.path) }
}

pub fn valid_host(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.contains('.') && !h.starts_with('.') && !h.ends_with('.') && !h.contains("..")
        && h.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && h.split('.').all(|p| !p.is_empty() && !p.starts_with('-') && !p.ends_with('-'))
        && h.split('.').last().map_or(false, |tld| tld.chars().any(|c| c.is_ascii_lowercase())) // not an IP address such as 10.0.0.1
}

/// Parse an address the person typed or a page contained. `http://` is upgraded to `https://`; a bare `example.org/x` is accepted.
pub fn parse_url(s: &str) -> Option<Url> {
    let s = s.trim();
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    if rest.contains("://") { return None; }
    let rest = rest.split('#').next().unwrap_or("");
    let (host, path) = match rest.find(|c| c == '/' || c == '?') { Some(i) => (&rest[..i], &rest[i..]), None => (rest, "/") };
    let host = host.to_ascii_lowercase();
    let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
    if !valid_host(&host) || path.len() > 2000 || path.chars().any(|c| c.is_control() || !c.is_ascii()) { return None; }
    Some(Url { host, path: path.replace(' ', "%20") })
}

/// Turn a link found on a page into an address, relative to the page it was on. None for things we do not follow (mailto:, javascript: ...).
pub fn resolve(base: &Url, href: &str) -> Option<Url> {
    let h = href.trim();
    let h = h.split('#').next().unwrap_or("");
    if h.is_empty() { return Some(base.clone()); }
    if let Some(i) = h.find(':') { if h[..i].chars().all(|c| c.is_ascii_alphabetic()) && !h.starts_with("//") { return if matches!(&h[..i], "http" | "https") { parse_url(h) } else { None }; } }
    if let Some(r) = h.strip_prefix("//") { return parse_url(r); }
    let path = if h.starts_with('/') { h.to_string() }
        else if h.starts_with('?') { format!("{}{}", base.path.split('?').next().unwrap_or("/"), h) }
        else {
            let dir = &base.path.split('?').next().unwrap_or("/");
            let dir = &dir[..dir.rfind('/').map_or(0, |i| i + 1)];
            format!("{dir}{h}")
        };
    // squash "." and ".." segments
    let (p, q) = match path.find('?') { Some(i) => (&path[..i], &path[i..]), None => (path.as_str(), "") };
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/').skip(1) { match seg { "." => {} ".." => { out.pop(); } s => out.push(s) } }
    let joined = format!("/{}{}", out.join("/"), q);
    parse_url(&format!("{}{}", base.host, joined))
}

// ---- the allowed sites ----------------------------------------------------------------------

pub fn parse_allow(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text.lines().map(|l| l.split('#').next().unwrap_or("").trim().to_ascii_lowercase()).filter(|l| valid_host(l)).collect();
    v.sort(); v.dedup(); v
}

/// Add or remove one domain in the text of the allowed-sites file (comments and order are kept; a new one goes last).
pub fn edit_allow(text: &str, domain: &str, add: bool) -> Result<String, String> {
    let d = domain.trim().to_ascii_lowercase();
    if !valid_allow_entry(&d) { return Err("Type just the site's name, like example.org.".into()); }
    let has = |l: &str| l.split('#').next().unwrap_or("").trim().eq_ignore_ascii_case(&d);
    let present = text.lines().any(has);
    if add {
        if present { return Err(format!("{d} is already allowed.")); }
        let mut t = text.to_string();
        if !t.is_empty() && !t.ends_with('\n') { t.push('\n'); }
        t.push_str(&d); t.push('\n');
        Ok(t)
    } else {
        if !present { return Err(format!("{d} is not on the list.")); }
        Ok(text.lines().filter(|l| !has(l)).map(|l| format!("{l}\n")).collect())
    }
}

pub fn allowed(host: &str, list: &[String]) -> bool { list.iter().any(|d| host == d || host.ends_with(&format!(".{d}"))) }

/// A name that may go on the list: a plain domain, without a scheme, a path or a wildcard.
pub fn valid_allow_entry(d: &str) -> bool { valid_host(d) && d.split('.').count() >= 2 }

// ---- the page reader --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style { Normal, Heading, Code, Dim, Quote }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span { pub text: String, pub style: Style, pub link: Option<usize> }

#[derive(Default, Debug)]
pub struct Doc { pub title: String, pub lines: Vec<Vec<Span>>, pub links: Vec<String> }

impl Doc {
    pub fn line_text(&self, i: usize) -> String { self.lines.get(i).map_or(String::new(), |l| l.iter().map(|s| s.text.as_str()).collect()) }
    /// The first line a link appears on.
    pub fn link_line(&self, link: usize) -> Option<usize> { self.lines.iter().position(|l| l.iter().any(|s| s.link == Some(link))) }
}

#[derive(Debug, PartialEq, Eq)]
enum Tok { Text(String), Open(String, Vec<(String, String)>), Close(String) }

fn decode_entities(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        match tail.find(';').filter(|e| *e <= 8) {
            Some(e) => {
                let name = &tail[..e];
                let rep: Option<String> = match name {
                    "amp" => Some("&".into()), "lt" => Some("<".into()), "gt" => Some(">".into()), "quot" => Some("\"".into()), "apos" => Some("'".into()),
                    "nbsp" => Some(" ".into()), "copy" => Some("(c)".into()), "reg" => Some("(R)".into()), "mdash" => Some("--".into()), "ndash" => Some("-".into()),
                    "hellip" => Some("...".into()), "laquo" => Some("<<".into()), "raquo" => Some(">>".into()), "bull" | "middot" => Some("*".into()),
                    "rarr" => Some("->".into()), "larr" => Some("<-".into()), "lsquo" | "rsquo" => Some("'".into()), "ldquo" | "rdquo" => Some("\"".into()), "trade" => Some("(TM)".into()),
                    n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32).map(|c| c.to_string()),
                    n if n.starts_with('#') => n[1..].parse::<u32>().ok().and_then(char::from_u32).map(|c| c.to_string()),
                    _ => None,
                };
                match rep { Some(r) => { out.push_str(&r); rest = &tail[e + 1..]; } None => { out.push('&'); rest = tail; } }
            }
            None => { out.push('&'); rest = tail; }
        }
    }
    out.push_str(rest);
    out
}

/// The font only has plain ASCII: typographic characters become their plain look-alikes, the rest a question mark.
fn asciify(s: &str) -> String {
    s.chars().map(|c| match c {
        '\u{2018}' | '\u{2019}' | '\u{201B}' => "'".to_string(), '\u{201C}' | '\u{201D}' => "\"".into(), '\u{2013}' => "-".into(), '\u{2014}' => "--".into(),
        '\u{2026}' => "...".into(), '\u{2022}' | '\u{00B7}' => "*".into(), '\u{00A0}' | '\u{200B}' => " ".into(), '\u{00A9}' => "(c)".into(),
        c if c.is_ascii() && (!c.is_control() || c == '\n' || c == '\t') => c.to_string(), _ => "?".into(),
    }).collect()
}

fn tokenize(html: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let b = html;
    let mut i = 0;
    while i < b.len() {
        match b[i..].find('<') {
            None => { toks.push(Tok::Text(b[i..].to_string())); break; }
            Some(off) => {
                if off > 0 { toks.push(Tok::Text(b[i..i + off].to_string())); }
                i += off;
                let rest = &b[i..];
                if rest.starts_with("<!--") { i += rest.find("-->").map_or(rest.len(), |e| e + 3); continue; }
                let Some(end) = rest.find('>') else { toks.push(Tok::Text(rest.to_string())); break; };
                let inner = &rest[1..end];
                i += end + 1;
                if inner.starts_with('!') || inner.starts_with('?') { continue; }
                if let Some(name) = inner.strip_prefix('/') { toks.push(Tok::Close(name.trim().to_ascii_lowercase())); continue; }
                if !inner.chars().next().map_or(false, |c| c.is_ascii_alphabetic()) { toks.push(Tok::Text(format!("<{inner}>"))); continue; }
                let inner = inner.trim_end_matches('/');
                let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
                let name = inner[..name_end].to_ascii_lowercase();
                let attrs = parse_attrs(&inner[name_end..]);
                let raw = matches!(name.as_str(), "script" | "style");
                toks.push(Tok::Open(name.clone(), attrs));
                if raw { // skip everything up to the matching close tag
                    let lower = b[i..].to_ascii_lowercase();
                    match lower.find(&format!("</{name}")) { Some(c) => { i += c; } None => { i = b.len(); } }
                }
            }
        }
    }
    toks
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() { i += 1; }
        let st = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' { i += 1; }
        let name: String = chars[st..i].iter().collect::<String>().to_ascii_lowercase();
        while i < chars.len() && chars[i].is_whitespace() { i += 1; }
        let mut val = String::new();
        if i < chars.len() && chars[i] == '=' {
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() { i += 1; }
            if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
                let q = chars[i]; i += 1; let vs = i;
                while i < chars.len() && chars[i] != q { i += 1; }
                val = chars[vs..i].iter().collect(); i += 1;
            } else { let vs = i; while i < chars.len() && !chars[i].is_whitespace() { i += 1; } val = chars[vs..i].iter().collect(); }
        }
        if !name.is_empty() { out.push((name, decode_entities(&val))); }
    }
    out
}

struct Builder {
    width: usize,
    doc: Doc,
    line: Vec<Span>,
    len: usize,
    indent: usize,
    pending_space: bool,
    blank_before: bool, // a blank line is wanted before the next text
}

impl Builder {
    fn new(width: usize) -> Builder { Builder { width: width.max(10), doc: Doc::default(), line: vec![], len: 0, indent: 0, pending_space: false, blank_before: false } }
    fn flush(&mut self) {
        if self.line.is_empty() { return; }
        while self.line.last().map_or(false, |s| s.text.trim_end().is_empty() && s.link.is_none()) { self.line.pop(); }
        if let Some(l) = self.line.last_mut() { l.text = l.text.trim_end().to_string(); }
        let l = std::mem::take(&mut self.line);
        self.doc.lines.push(l); self.len = 0; self.pending_space = false;
    }
    fn start_line(&mut self) {
        if self.line.is_empty() && self.blank_before && !self.doc.lines.is_empty() { self.doc.lines.push(vec![]); }
        if self.line.is_empty() { self.blank_before = false; if self.indent > 0 { self.line.push(Span { text: " ".repeat(self.indent), style: Style::Normal, link: None }); self.len = self.indent; } }
    }
    /// Start a new block: finish the current line and (for paragraphs, headings ...) ask for a blank line before the next one.
    fn block(&mut self, gap: bool) { self.flush(); if gap { self.blank_before = true; } }
    fn newline(&mut self) { if self.line.is_empty() { self.doc.lines.push(vec![]); } else { self.flush(); } }
    fn push_span(&mut self, text: &str, style: Style, link: Option<usize>) {
        match self.line.last_mut() { Some(l) if l.style == style && l.link == link => l.text.push_str(text), _ => self.line.push(Span { text: text.into(), style, link }) }
        self.len += text.chars().count();
    }
    fn word(&mut self, w: &str, style: Style, link: Option<usize>) {
        let wl = w.chars().count();
        self.start_line();
        let sp = if self.pending_space && self.len > self.indent { 1 } else { 0 };
        if self.len + sp + wl > self.width && self.len > self.indent { self.flush(); self.start_line(); } else if sp == 1 { self.push_span(" ", style, if link.is_some() { link } else { None }); }
        // very long words (addresses) are cut to the width
        let mut w = w.to_string();
        while w.chars().count() > self.width.saturating_sub(self.len).max(1) {
            let take = self.width.saturating_sub(self.len).max(1);
            let head: String = w.chars().take(take).collect(); w = w.chars().skip(take).collect();
            self.push_span(&head, style, link); self.flush(); self.start_line();
        }
        self.push_span(&w, style, link);
        self.pending_space = false;
    }
    fn text(&mut self, t: &str, style: Style, link: Option<usize>, pre: bool) {
        if pre {
            for (i, l) in t.trim_matches('\n').split('\n').enumerate() {
                if i > 0 { self.flush(); }
                self.start_line();
                let l = l.replace('\t', "    ");
                let l: String = l.chars().take(self.width.saturating_sub(self.len)).collect();
                self.push_span(&l, Style::Code, link);
            }
            return;
        }
        let starts_space = t.chars().next().map_or(false, char::is_whitespace);
        if starts_space { self.pending_space = true; }
        for w in t.split_whitespace() { self.word(w, style, link); self.pending_space = true; }
        if !t.chars().next_back().map_or(false, char::is_whitespace) { self.pending_space = false; }
    }
}

/// Lay out HTML as lines of at most `width` characters.
pub fn render(html: &str, width: usize, base: &Url) -> Doc {
    let toks = tokenize(html);
    let mut b = Builder::new(width);
    let (mut skip, mut in_title, mut pre) = (0usize, false, false);
    let (mut heading, mut code, mut quote, mut th) = (0usize, 0usize, 0usize, 0usize);
    let mut link: Option<usize> = None;
    let mut lists: Vec<(bool, usize)> = vec![]; // (ordered, counter)
    let mut title = String::new();
    for t in toks {
        match t {
            Tok::Text(s) => {
                let s = asciify(&decode_entities(&s));
                if in_title { title.push_str(&s); continue; }
                if skip > 0 { continue; }
                let style = if heading > 0 || th > 0 { Style::Heading } else if code > 0 { Style::Code } else if quote > 0 { Style::Quote } else { Style::Normal };
                b.text(&s, style, link, pre);
            }
            Tok::Open(name, attrs) => {
                let attr = |n: &str| attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
                match name.as_str() {
                    "head" | "script" | "style" | "noscript" | "template" | "svg" | "select" | "button" | "textarea" => skip += 1,
                    "title" => in_title = true,
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => { b.block(true); heading += 1; }
                    "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "nav" | "aside" | "form" | "figure" | "figcaption" | "details" | "summary" | "address" | "dl" => b.block(name == "p"),
                    "br" => b.newline(),
                    "hr" => { b.block(false); b.start_line(); let w = b.width.saturating_sub(b.indent); b.push_span(&"-".repeat(w), Style::Dim, None); b.flush(); b.blank_before = true; }
                    "ul" | "ol" => { b.block(lists.is_empty()); lists.push((name == "ol", 0)); b.indent += 2; }
                    "li" => {
                        b.flush();
                        let bullet = match lists.last_mut() { Some((true, n)) => { *n += 1; format!("{}. ", n) } _ => "* ".to_string() };
                        b.indent = b.indent.saturating_sub(2); b.start_line(); b.push_span(&bullet, Style::Dim, None); b.pending_space = false; b.indent += 2;
                    }
                    "dt" => b.block(false),
                    "dd" => { b.block(false); }
                    "pre" => { b.block(true); pre = true; }
                    "code" | "kbd" | "samp" | "tt" => code += 1,
                    "blockquote" => { b.block(true); quote += 1; b.indent += 2; }
                    "table" => b.block(true),
                    "tr" => b.block(false),
                    "td" | "th" => { if name == "th" { th += 1; } if !b.line.is_empty() { b.push_span(" | ", Style::Dim, None); b.pending_space = false; } }
                    "a" => { if let Some(h) = attr("href") { if let Some(u) = resolve(base, &h) { b.doc.links.push(u.text()); link = Some(b.doc.links.len() - 1); } } }
                    "img" => { if skip == 0 { let alt = attr("alt").map(|a| asciify(&a)).unwrap_or_default(); if !alt.trim().is_empty() { b.text(&format!("[image: {}]", alt.trim()), Style::Dim, link, false); } } }
                    _ => {}
                }
            }
            Tok::Close(name) => match name.as_str() {
                "head" | "script" | "style" | "noscript" | "template" | "svg" | "select" | "button" | "textarea" => skip = skip.saturating_sub(1),
                "title" => in_title = false,
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => { heading = heading.saturating_sub(1); b.block(true); }
                "p" => b.block(true),
                "div" | "section" | "article" | "header" | "footer" | "main" | "nav" | "aside" | "form" | "figure" | "figcaption" | "details" | "summary" | "address" | "dl" | "tr" | "table" | "li" => b.block(name == "table"),
                "ul" | "ol" => { b.block(lists.len() == 1); lists.pop(); b.indent = b.indent.saturating_sub(2); }
                "pre" => { pre = false; b.block(true); }
                "code" | "kbd" | "samp" | "tt" => code = code.saturating_sub(1),
                "blockquote" => { quote = quote.saturating_sub(1); b.indent = b.indent.saturating_sub(2); b.block(true); }
                "th" => th = th.saturating_sub(1),
                "a" => link = None,
                _ => {}
            },
        }
    }
    b.flush();
    let mut doc = b.doc;
    while doc.lines.last().map_or(false, |l| l.is_empty()) { doc.lines.pop(); }
    doc.title = asciify(&decode_entities(&title)).split_whitespace().collect::<Vec<_>>().join(" ");
    doc
}

/// A plain-text page (text/plain).
pub fn render_text(text: &str, width: usize) -> Doc {
    let mut d = Doc::default();
    for l in asciify(text).lines().take(20000) {
        let mut l = l.replace('\t', "    ");
        loop {
            let head: String = l.chars().take(width.max(10)).collect();
            let n = head.chars().count();
            d.lines.push(vec![Span { text: head, style: Style::Normal, link: None }]);
            if l.chars().count() <= n { break; }
            l = l.chars().skip(n).collect();
        }
    }
    d
}

pub fn home_page(sites: &[String]) -> String {
    let mut h = String::from("<title>Home</title><h1>Exec85 browser</h1><p>This browser opens only the sites on the allowed list, so you can read documentation and download files without the rest of the web. Press G to type an address, Tab to move between links and Enter to open one.</p><h2>Allowed sites</h2><ul>");
    for s in sites { h.push_str(&format!("<li><a href=\"https://{s}/\">{s}</a></li>")); }
    h.push_str("</ul><p>An administrator can allow another site with the + key, or in the terminal: <code>sites add example.org</code>.</p>");
    h
}

// ---- fetching ---------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Response { pub status: u16, pub content_type: String, pub location: Option<String>, pub length: Option<u64>, pub disposition: String, pub body: Vec<u8>, pub too_big: bool }

pub trait Fetcher {
    fn get(&self, url: &str) -> Result<Response, String>;
    fn head(&self, url: &str) -> Result<Response, String>;
    fn download(&self, url: &str, dest: &Path) -> Result<u64, String>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Nav { Page { url: String, html: String }, Text { url: String, text: String }, File { url: String, name: String, size: Option<u64> } }

fn refused(host: &str) -> String { format!("{host} is not on the list of allowed sites. An administrator can allow it with the + key.") }

/// Follow redirects by hand (each stop must be allowed) and say what is at the end.
pub fn navigate(f: &dyn Fetcher, list: &[String], start: &str) -> Result<Nav, String> {
    let mut url = parse_url(start).ok_or("That is not a web address I can open (it needs a name like example.org).")?;
    for _ in 0..=MAX_HOPS {
        if !allowed(&url.host, list) { return Err(refused(&url.host)); }
        let r = f.get(&url.text())?;
        if matches!(r.status, 301 | 302 | 303 | 307 | 308) {
            let loc = r.location.ok_or("The site sent me somewhere without saying where.")?;
            url = resolve(&url, &loc).ok_or("The site sent me to an address I cannot open.")?;
            continue;
        }
        if r.status >= 400 { return Err(format!("The site answered {} ({}).", r.status, if r.status == 404 { "page not found" } else if r.status == 403 { "not allowed" } else { "error" })); }
        let ct = r.content_type.to_ascii_lowercase();
        let attachment = r.disposition.to_ascii_lowercase().contains("attachment");
        let html = ct.contains("text/html") || ct.contains("application/xhtml");
        if !attachment && !r.too_big && (html || ct.starts_with("text/")) {
            let body = String::from_utf8_lossy(&r.body).into_owned();
            return Ok(if html { Nav::Page { url: url.text(), html: body } } else { Nav::Text { url: url.text(), text: body } });
        }
        return Ok(Nav::File { name: file_name(&url, &r.disposition), size: r.length.or(if r.too_big { None } else { Some(r.body.len() as u64) }), url: url.text() });
    }
    Err("Too many redirects.".into())
}

pub fn safe_file_name(n: &str) -> String {
    let n: String = n.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
    let n = n.trim_start_matches('.').to_string();
    let n: String = n.chars().take(80).collect();
    if n.is_empty() { "download".into() } else { n }
}

pub fn file_name(url: &Url, disposition: &str) -> String {
    if let Some(i) = disposition.find("filename=") {
        let v = disposition[i + 9..].split(';').next().unwrap_or("").trim().trim_matches('"');
        if !v.is_empty() { return safe_file_name(v.rsplit('/').next().unwrap_or(v)); }
    }
    let p = url.path.split('?').next().unwrap_or("");
    let last = p.rsplit('/').next().unwrap_or("");
    safe_file_name(if last.is_empty() { &url.host } else { last })
}

fn unique(dir: &Path, name: &str) -> PathBuf {
    let mut p = dir.join(name);
    let mut n = 1;
    while p.exists() { let (stem, ext) = match name.rfind('.') { Some(i) if i > 0 => (&name[..i], &name[i..]), _ => (name, "") }; p = dir.join(format!("{stem}-{n}{ext}")); n += 1; }
    p
}

/// Download a file: every redirect stop must be on the allowed list; the file lands in `dir` (never overwriting).
pub fn download(f: &dyn Fetcher, list: &[String], start: &str, dir: &Path) -> Result<String, String> {
    let mut url = parse_url(start).ok_or("That is not an address I can download from.")?;
    let mut disposition = String::new();
    let mut ok = false;
    for _ in 0..=MAX_HOPS {
        if !allowed(&url.host, list) { return Err(refused(&url.host)); }
        let r = f.head(&url.text())?;
        if matches!(r.status, 301 | 302 | 303 | 307 | 308) { let loc = r.location.ok_or("The site sent me somewhere without saying where.")?; url = resolve(&url, &loc).ok_or("The site sent me to an address I cannot open.")?; continue; }
        if r.status >= 400 { return Err(format!("The site answered {} for that file.", r.status)); }
        disposition = r.disposition; ok = true; break;
    }
    if !ok { return Err("Too many redirects.".into()); }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = unique(dir, &file_name(&url, &disposition));
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let n = match f.download(&url.text(), &part) { Ok(n) => n, Err(e) => { let _ = std::fs::remove_file(&part); return Err(e); } };
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    Ok(format!("Saved {} ({}) in {}", dest.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()), human_size(n), dir.display()))
}

pub fn human_size(n: u64) -> String {
    if n >= 1 << 30 { format!("{:.1} GB", n as f64 / (1u64 << 30) as f64) } else if n >= 1 << 20 { format!("{:.1} MB", n as f64 / (1u64 << 20) as f64) } else if n >= 1 << 10 { format!("{} KB", n >> 10) } else { format!("{n} bytes") }
}

// curl does the secure connection (it checks certificates; the BusyBox downloader does not)
pub struct Curl;

fn parse_headers(text: &str) -> Response {
    let mut r = Response::default();
    // the last header block counts
    let block = text.split("\r\n\r\n").filter(|b| b.starts_with("HTTP/")).last().unwrap_or("");
    for (i, l) in block.lines().enumerate() {
        if i == 0 { r.status = l.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0); continue; }
        if let Some((k, v)) = l.split_once(':') {
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() { "content-type" => r.content_type = v.to_string(), "location" => r.location = Some(v.to_string()), "content-length" => r.length = v.parse().ok(), "content-disposition" => r.disposition = v.to_string(), _ => {} }
        }
    }
    r
}

impl Curl {
    fn base(&self, max_size: u64, secs: u64) -> Command {
        let mut c = Command::new("curl");
        c.args(["-sS", "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "0", "--connect-timeout", "10", "--max-time", &secs.to_string(), "--max-filesize", &max_size.to_string(), "-A", "Exec85-Browser/0.9"]);
        c
    }
    fn tmp(tag: &str) -> PathBuf { std::env::temp_dir().join(format!("exec85-web-{}-{tag}", std::process::id())) }
    fn run(&self, mut c: Command, hdr: &Path) -> Result<(i32, String, Response), String> {
        let out = c.output().map_err(|e| format!("The secure downloader (curl) is not available: {e}"))?;
        let text = std::fs::read_to_string(hdr).unwrap_or_default();
        let _ = std::fs::remove_file(hdr);
        Ok((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim().to_string(), parse_headers(&text)))
    }
}

impl Fetcher for Curl {
    fn get(&self, url: &str) -> Result<Response, String> {
        let (hdr, body) = (Curl::tmp("h"), Curl::tmp("b"));
        let mut c = self.base(MAX_PAGE_BYTES, 25);
        c.arg("-D").arg(&hdr).arg("-o").arg(&body).arg("--").arg(url);
        let (code, err, mut r) = self.run(c, &hdr)?;
        r.body = std::fs::read(&body).unwrap_or_default();
        let _ = std::fs::remove_file(&body);
        match code { 0 => Ok(r), 63 => { r.too_big = true; r.body.clear(); Ok(r) } _ => Err(curl_error(code, &err)) }
    }
    fn head(&self, url: &str) -> Result<Response, String> {
        let hdr = Curl::tmp("hh");
        let mut c = self.base(u64::MAX / 2, 25);
        c.arg("-I").arg("-D").arg(&hdr).arg("-o").arg("/dev/null").arg("--").arg(url);
        let (code, err, r) = self.run(c, &hdr)?;
        if code == 0 { Ok(r) } else { Err(curl_error(code, &err)) }
    }
    fn download(&self, url: &str, dest: &Path) -> Result<u64, String> {
        let hdr = Curl::tmp("dh");
        let mut c = self.base(4u64 << 30, 3600);
        c.arg("-D").arg(&hdr).arg("-o").arg(dest).arg("--").arg(url);
        let (code, err, r) = self.run(c, &hdr)?;
        if code != 0 { return Err(curl_error(code, &err)); }
        if r.status >= 400 { return Err(format!("The site answered {} for that file.", r.status)); }
        Ok(std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0))
    }
}

fn curl_error(code: i32, err: &str) -> String {
    match code {
        6 => "I could not find that site (name lookup failed). Is the network connected?".into(),
        7 => "I could not connect to that site.".into(),
        28 => "The site took too long to answer.".into(),
        35 | 51 | 58 | 60 | 77 | 83 => "The secure connection failed: the site's certificate could not be trusted.".into(),
        _ => format!("The download failed ({}).", err.lines().last().unwrap_or("unknown error")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn base() -> Url { parse_url("https://docs.example.org/guide/index.html").unwrap() }
    fn allow() -> Vec<String> { vec!["example.org".into(), "files.example.net".into()] }

    #[test]
    fn addresses_are_checked_strictly() {
        assert_eq!(parse_url("https://Example.org").unwrap().text(), "https://example.org/");
        assert_eq!(parse_url("http://example.org/a b").unwrap().text(), "https://example.org/a%20b", "http is upgraded");
        assert_eq!(parse_url("example.org/x?y=1").unwrap().path, "/x?y=1");
        assert_eq!(parse_url("example.org?q=1").unwrap().path, "/?q=1");
        for bad in ["", "localhost", "http://127.0.0.1/", "https://10.0.0.1/x", "https://user@example.org/", "https://example.org:8080/", "ftp://example.org/", "https://exa mple.org/", "https://-bad.org/", "https://a..b.org/", "javascript:alert(1)", "https://example.org/\u{e9}"] {
            assert!(parse_url(bad).is_none(), "{bad:?}");
        }
        assert!(parse_url("https://example.org/#frag").unwrap().path == "/");
    }

    #[test]
    fn links_are_resolved_against_the_page() {
        let b = base();
        let r = |h: &str| resolve(&b, h).map(|u| u.text());
        assert_eq!(r("page2.html").as_deref(), Some("https://docs.example.org/guide/page2.html"));
        assert_eq!(r("../up.html").as_deref(), Some("https://docs.example.org/up.html"));
        assert_eq!(r("/root").as_deref(), Some("https://docs.example.org/root"));
        assert_eq!(r("//other.org/x").as_deref(), Some("https://other.org/x"));
        assert_eq!(r("https://x.example.org/a#top").as_deref(), Some("https://x.example.org/a"));
        assert_eq!(r("?page=2").as_deref(), Some("https://docs.example.org/guide/index.html?page=2"));
        assert_eq!(r("#section").as_deref(), Some("https://docs.example.org/guide/index.html"));
        assert_eq!(r("../../../../x").as_deref(), Some("https://docs.example.org/x"), "cannot climb above the root");
        for no in ["mailto:a@b.org", "javascript:void(0)", "tel:123", "data:text/html,x", "ftp://example.org/f"] { assert_eq!(r(no), None, "{no}"); }
    }

    #[test]
    fn the_allowed_list_can_be_edited() {
        let t = "# my sites\nexample.org\n";
        let t2 = edit_allow(t, "Docs.Example.net", true).unwrap();
        assert_eq!(t2, "# my sites\nexample.org\ndocs.example.net\n");
        assert!(edit_allow(&t2, "docs.example.net", true).unwrap_err().contains("already"));
        assert_eq!(edit_allow(&t2, "example.org", false).unwrap(), "# my sites\ndocs.example.net\n");
        assert!(edit_allow(&t2, "nothere.org", false).unwrap_err().contains("not on the list"));
        for bad in ["org", "*.x.org", "http://a.org", "a b.org", "10.0.0.1", ""] { assert!(edit_allow(t, bad, true).is_err(), "{bad}"); }
        assert_eq!(edit_allow("", "a.org", true).unwrap(), "a.org\n");
        assert_eq!(edit_allow("a.org", "b.org", true).unwrap(), "a.org\nb.org\n", "a missing final newline is repaired");
    }

    #[test]
    fn only_allowed_sites_and_their_subdomains_pass() {
        let l = allow();
        assert!(allowed("example.org", &l) && allowed("docs.example.org", &l) && allowed("files.example.net", &l));
        assert!(!allowed("badexample.org", &l), "a name that merely ends the same way is not a subdomain");
        assert!(!allowed("example.org.evil.com", &l) && !allowed("example.net", &l) && !allowed("net", &l));
        assert_eq!(parse_allow("# sites\nExample.org\n\nbad entry\n*.evil.com\nexample.org # twice\nlocalhost\n10.1.2.3\n"), vec!["example.org".to_string()]);
        assert!(valid_allow_entry("example.org") && !valid_allow_entry("org") && !valid_allow_entry("*.example.org") && !valid_allow_entry("http://example.org"));
    }

    fn lines(html: &str, w: usize) -> Vec<String> { let d = render(html, w, &base()); (0..d.lines.len()).map(|i| d.line_text(i)).collect() }

    #[test]
    fn headings_paragraphs_and_wrapping() {
        let l = lines("<html><head><title> My   page </title><style>p{color:red}</style></head><body><h1>Hello</h1><p>one two three four five six seven eight</p><p>second</p></body></html>", 20);
        assert_eq!(l, vec!["Hello", "", "one two three four", "five six seven eight", "", "second"]);
        let d = render("<title>My &amp; page</title><p>x</p>", 40, &base());
        assert_eq!(d.title, "My & page");
        assert_eq!(l.iter().map(|s| s.chars().count()).max().unwrap() <= 20, true);
    }

    #[test]
    fn scripts_styles_and_comments_never_show() {
        let l = lines("<p>a<script>var x = '<b>';</script>b<!-- hidden --><style>.c{}</style>c</p>", 40);
        assert_eq!(l, vec!["abc"]);
        assert_eq!(lines("<p>x</p><script>never ending", 40), vec!["x"]);
    }

    #[test]
    fn links_lists_code_and_tables() {
        let d = render("<ul><li>First <a href='/a'>link a</a></li><li>Second</li></ul><ol><li>one</li><li>two</li></ol><pre>  keep\n    this</pre><table><tr><th>Name</th><th>Size</th></tr><tr><td>x</td><td>1 MB</td></tr></table><p><a href='https://other.org/'>other</a> <a href='mailto:a@b.c'>mail</a></p>", 40, &base());
        let t: Vec<String> = (0..d.lines.len()).map(|i| d.line_text(i)).collect();
        assert!(t.contains(&"  * First link a".to_string()) || t.contains(&"* First link a".to_string()), "{t:?}");
        assert!(t.iter().any(|l| l.trim() == "1. one") && t.iter().any(|l| l.trim() == "2. two"), "{t:?}");
        assert!(t.iter().any(|l| l.contains("  keep")) && t.iter().any(|l| l.contains("    this")), "pre keeps its spaces: {t:?}");
        assert!(t.iter().any(|l| l == "Name | Size") && t.iter().any(|l| l == "x | 1 MB"), "{t:?}");
        assert_eq!(d.links, vec!["https://docs.example.org/a".to_string(), "https://other.org/".to_string()], "mailto links are not links");
        assert_eq!(d.link_line(0).map(|i| d.line_text(i).contains("link a")), Some(true));
    }

    #[test]
    fn entities_and_non_ascii_text_become_plain_ascii() {
        let l = lines("<p>5 &lt; 6 &amp; &quot;q&quot; &#65;&#x42; &copy; caf\u{e9} \u{201C}hi\u{201D} a\u{2014}b &nosuch; &amp</p>", 80);
        assert_eq!(l, vec!["5 < 6 & \"q\" AB (c) caf? \"hi\" a--b &nosuch; &amp"]);
    }

    #[test]
    fn very_long_words_and_broken_html_do_not_break_the_reader() {
        let long = "x".repeat(100);
        let l = lines(&format!("<p>{long}</p>"), 30);
        assert!(l.iter().all(|s| s.chars().count() <= 30) && l.len() == 4, "{l:?}");
        for junk in ["<", "<<<>>>", "<a href=\"x", "</p></p></div>", "<ul><li>x</ul></ul></ul>", "&", "<p", "<!--", "<table><td>", ""] { let _ = render(junk, 20, &base()); }
        let _ = render(&"<div>".repeat(5000), 20, &base());
        let _ = render_text(&"y".repeat(5000), 20);
    }

    #[test]
    fn plain_text_pages_wrap_without_losing_anything() {
        let d = render_text("short\nabcdefghijklmnopqrstuvwxyz\n", 10);
        let t: Vec<String> = (0..d.lines.len()).map(|i| d.line_text(i)).collect();
        assert_eq!(t, vec!["short", "abcdefghij", "klmnopqrst", "uvwxyz"]);
    }

    struct Fake { pages: HashMap<String, Response>, asked: RefCell<Vec<String>>, files: RefCell<Vec<String>> }
    fn page(ct: &str, body: &str) -> Response { Response { status: 200, content_type: ct.into(), body: body.as_bytes().to_vec(), ..Default::default() } }
    fn redirect(to: &str) -> Response { Response { status: 302, location: Some(to.into()), ..Default::default() } }
    impl Fetcher for Fake {
        fn get(&self, url: &str) -> Result<Response, String> { self.asked.borrow_mut().push(url.into()); self.pages.get(url).cloned().ok_or_else(|| "no such page".into()) }
        fn head(&self, url: &str) -> Result<Response, String> { self.asked.borrow_mut().push(format!("HEAD {url}")); self.pages.get(url).cloned().map(|mut r| { r.body.clear(); r }).ok_or_else(|| "no such page".into()) }
        fn download(&self, url: &str, dest: &Path) -> Result<u64, String> { self.files.borrow_mut().push(url.into()); std::fs::write(dest, b"file contents").map_err(|e| e.to_string())?; Ok(13) }
    }
    fn fake(pages: Vec<(&str, Response)>) -> Fake { Fake { pages: pages.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), asked: RefCell::new(vec![]), files: RefCell::new(vec![]) } }

    #[test]
    fn a_page_is_fetched_and_a_forbidden_site_is_never_contacted() {
        let f = fake(vec![("https://example.org/", page("text/html; charset=utf-8", "<p>hi</p>")), ("https://evil.com/", page("text/html", "x"))]);
        assert_eq!(navigate(&f, &allow(), "example.org").unwrap(), Nav::Page { url: "https://example.org/".into(), html: "<p>hi</p>".into() });
        let e = navigate(&f, &allow(), "https://evil.com/").unwrap_err();
        assert!(e.contains("evil.com is not on the list"), "{e}");
        assert!(!f.asked.borrow().iter().any(|a| a.contains("evil")), "{:?}", f.asked.borrow());
        assert!(navigate(&f, &allow(), "10.0.0.1").unwrap_err().contains("web address"));
    }

    #[test]
    fn every_redirect_stop_must_be_allowed() {
        let f = fake(vec![("https://example.org/a", redirect("/b")), ("https://example.org/b", redirect("https://files.example.net/c")), ("https://files.example.net/c", page("text/plain", "plain")),
                          ("https://example.org/x", redirect("https://evil.com/steal")), ("https://evil.com/steal", page("text/html", "bad")), ("https://example.org/loop", redirect("/loop"))]);
        assert_eq!(navigate(&f, &allow(), "https://example.org/a").unwrap(), Nav::Text { url: "https://files.example.net/c".into(), text: "plain".into() });
        assert!(navigate(&f, &allow(), "https://example.org/x").unwrap_err().contains("evil.com"));
        assert!(!f.asked.borrow().iter().any(|a| a.contains("evil.com")), "the forbidden stop was not even contacted");
        assert!(navigate(&f, &allow(), "https://example.org/loop").unwrap_err().contains("Too many"));
    }

    #[test]
    fn files_are_offered_for_download_and_errors_are_explained() {
        let mut r = page("application/octet-stream", ""); r.length = Some(5_000_000);
        let f = fake(vec![("https://example.org/tool.iso", r), ("https://example.org/gone", Response { status: 404, ..Default::default() }),
                          ("https://example.org/big", Response { status: 200, content_type: "text/html".into(), too_big: true, length: Some(9_000_000), ..Default::default() })]);
        assert_eq!(navigate(&f, &allow(), "https://example.org/tool.iso").unwrap(), Nav::File { url: "https://example.org/tool.iso".into(), name: "tool.iso".into(), size: Some(5_000_000) });
        assert!(navigate(&f, &allow(), "https://example.org/gone").unwrap_err().contains("404"));
        assert!(matches!(navigate(&f, &allow(), "https://example.org/big").unwrap(), Nav::File { .. }), "a huge 'page' is offered as a download");
    }

    #[test]
    fn downloads_go_to_the_folder_never_overwrite_and_check_every_stop() {
        let dir = std::env::temp_dir().join(format!("exec85-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut ok = page("application/octet-stream", ""); ok.disposition = "attachment; filename=\"../../evil name.iso\"".into();
        let f = fake(vec![("https://example.org/get", redirect("https://files.example.net/f")), ("https://files.example.net/f", ok), ("https://example.org/bad", redirect("https://evil.com/f")), ("https://evil.com/f", page("x/y", ""))]);
        let m = download(&f, &allow(), "https://example.org/get", &dir).unwrap();
        assert!(m.starts_with("Saved evil_name.iso (13 bytes)"), "{m}");
        assert_eq!(std::fs::read(dir.join("evil_name.iso")).unwrap(), b"file contents");
        let m2 = download(&f, &allow(), "https://example.org/get", &dir).unwrap();
        assert!(m2.contains("evil_name-1.iso"), "{m2}");
        assert!(download(&f, &allow(), "https://example.org/bad", &dir).unwrap_err().contains("evil.com"));
        assert_eq!(f.files.borrow().len(), 2, "the forbidden file was never downloaded");
        assert!(!std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".part")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd".replace("_.._", "_.._"));
        assert_eq!(safe_file_name(".hidden"), "hidden");
        assert_eq!(safe_file_name(""), "download");
        assert_eq!(safe_file_name("a b&c.txt"), "a_b_c.txt");
        assert_eq!(file_name(&parse_url("https://example.org/dir/file.tar.gz?x=1").unwrap(), ""), "file.tar.gz");
        assert_eq!(file_name(&parse_url("https://example.org/").unwrap(), ""), "example.org");
        assert_eq!(human_size(1536), "1 KB"); assert_eq!(human_size(5 << 20), "5.0 MB");
    }

    #[test]
    fn curl_headers_are_read_from_the_last_block() {
        let r = parse_headers("HTTP/2 302 \r\nlocation: /x\r\n\r\nHTTP/2 200 \r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 42\r\nContent-Disposition: attachment; filename=a.txt\r\n\r\n");
        assert_eq!((r.status, r.content_type.as_str(), r.length, r.location.is_none()), (200, "text/html; charset=utf-8", Some(42), true));
        assert!(r.disposition.contains("a.txt"));
        assert_eq!(parse_headers("").status, 0);
    }

    #[test]
    fn the_home_page_lists_the_allowed_sites_as_links() {
        let d = render(&home_page(&allow()), 60, &parse_url("https://home.local").unwrap_or(base()));
        assert_eq!(d.links, vec!["https://example.org/".to_string(), "https://files.example.net/".to_string()]);
        assert_eq!(d.title, "Home");
    }
}
