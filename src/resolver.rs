//! Everything that knows what filecoster.com pages look like.
//! If the site changes, this is the only file that should need touching.

use crate::model::{sanitize, LinkKind, VIDEO_EXTS};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;
use url::Url;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// One HTTP agent for everything (shares cookies). No total timeout, so big downloads are fine,
/// but a connection that stops delivering data for 30 s fails and gets retried.
pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(UA)
        .redirects(10)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(30))
        .build()
}

/// Sends the request; any HTTP status (even 404/416) comes back as a response, only
/// connection-level problems are errors.
pub fn send(req: ureq::Request) -> Result<ureq::Response, String> {
    match req.call() {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(_, r)) => Ok(r),
        Err(e) => Err(format!("network: {e}")),
    }
}

// ───────────────────────── link parsing ─────────────────────────

#[derive(Debug)]
pub struct Parsed {
    pub kind: LinkKind,
    pub code: String,
    pub url: String,
}

/// Pulls every filecoster link out of any pasted text.
///   /d/<code>, /s/<code> and /e/<code> -> single video
///   /f/<code>                          -> folder of videos
pub fn parse_links(text: &str) -> Vec<Parsed> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)(?:https?://)?(?:[a-z0-9-]+\.)*filecoster\.com/(d|s|e|f)/([A-Za-z0-9_-]+)")
            .unwrap()
    });
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for c in re.captures_iter(text) {
        let seg = c[1].to_ascii_lowercase();
        // the codes on the site are lower case: /D/ABC and /d/abc are the same video
        let code = c[2].to_ascii_lowercase();
        if !seen.insert((seg.clone(), code.clone())) {
            continue;
        }
        out.push(Parsed {
            kind: if seg == "f" {
                LinkKind::Folder
            } else {
                LinkKind::Single
            },
            url: format!("https://filecoster.com/{seg}/{code}"),
            code,
        });
    }
    out
}

fn classify(url: &Url) -> Option<(LinkKind, String)> {
    let mut seg = url.path_segments()?;
    let kind = match seg.next()? {
        "d" | "s" | "e" => LinkKind::Single,
        "f" => LinkKind::Folder,
        _ => return None,
    };
    let code = seg.next()?;
    if code.is_empty() {
        return None;
    }
    Some((kind, code.to_string()))
}

// ───────────────────────── http helpers ─────────────────────────

fn get_html(agent: &ureq::Agent, url: &str, referer: Option<&str>) -> Result<String, String> {
    let mut last = String::from("unknown error");
    for attempt in 0..3u64 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(900 * attempt));
        }
        let mut rq = agent
            .get(url)
            .set("Accept", "text/html,application/xhtml+xml,*/*;q=0.8");
        if let Some(r) = referer {
            rq = rq.set("Referer", r);
        }
        match send(rq) {
            Ok(resp) => {
                let st = resp.status();
                if (200..300).contains(&st) {
                    return resp.into_string().map_err(|e| format!("network: {e}"));
                }
                last = format!("HTTP {st}");
                match st {
                    404 | 410 => break,
                    429 => std::thread::sleep(Duration::from_secs(5)),
                    _ => {}
                }
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn text_of(el: ElementRef) -> String {
    el.text().collect::<String>().trim().to_string()
}

// ───────────────────────── folders (/f/…) ─────────────────────────

pub struct FoundFile {
    pub code: String,
    pub title: String,
    pub page_url: String,
    pub subdir: String,
}

fn folder_title(doc: &Html) -> String {
    for s in ["h1.drive-title", "h1", "title"] {
        if let Some(el) = doc.select(&sel(s)).next() {
            let t = text_of(el);
            if !t.is_empty() {
                return t;
            }
        }
    }
    "Folder".into()
}

/// Walks a folder: every page of it and every sub-folder. Returns the videos found plus warnings
/// for pages that couldn't be read (the root page failing is a hard error).
pub fn crawl_folder(
    agent: &ureq::Agent,
    root: &str,
    group: bool,
) -> Result<(Vec<FoundFile>, Vec<String>), String> {
    let mut files: Vec<FoundFile> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen_folders: HashSet<String> = HashSet::new();
    let mut seen_files: HashSet<String> = HashSet::new();

    let s_file = sel("a.file-name[href]");
    let s_file_fallback = sel(".file-grid a[href], .drive-file-card a[href]");
    let s_folder = sel(".folder-row a[href]");
    let s_page = sel(".drive-pagination a[href]");

    if let Some((_, c)) = Url::parse(root).ok().as_ref().and_then(classify) {
        seen_folders.insert(c);
    }
    // (folder url, parent path, is_root)
    let mut stack: Vec<(String, Vec<String>, bool)> = vec![(root.to_string(), vec![], true)];

    while let Some((url, parent, is_root)) = stack.pop() {
        let first = match get_html(agent, &url, None) {
            Ok(h) => h,
            Err(e) => {
                if is_root {
                    return Err(e);
                }
                warnings.push(format!("{url}: {e}"));
                continue;
            }
        };
        let this_code = Url::parse(&url)
            .ok()
            .as_ref()
            .and_then(classify)
            .map(|x| x.1);
        let mut path = parent.clone();
        if group {
            path.push(sanitize(&folder_title(&Html::parse_document(&first))));
        }
        let subdir = path.join("/");

        let mut pages: Vec<String> = vec![url.clone()];
        let mut visited: HashSet<String> = HashSet::from([url.clone()]);
        let mut idx = 0usize;
        let mut cur = Some(first);

        while idx < pages.len() && idx < 500 {
            let html = match cur.take() {
                Some(h) => h,
                None => {
                    std::thread::sleep(Duration::from_millis(150));
                    match get_html(agent, &pages[idx], Some(&url)) {
                        Ok(h) => h,
                        Err(e) => {
                            warnings.push(format!("{}: {e}", pages[idx]));
                            idx += 1;
                            continue;
                        }
                    }
                }
            };
            let base = match Url::parse(&pages[idx]) {
                Ok(b) => b,
                Err(_) => {
                    idx += 1;
                    continue;
                }
            };
            let doc = Html::parse_document(&html);

            // videos
            let mut anchors: Vec<ElementRef> = doc.select(&s_file).collect();
            if anchors.is_empty() {
                anchors = doc.select(&s_file_fallback).collect();
            }
            for a in anchors {
                let Some(abs) = a.value().attr("href").and_then(|h| base.join(h).ok()) else {
                    continue;
                };
                let Some((LinkKind::Single, code)) = classify(&abs) else {
                    continue;
                };
                if !seen_files.insert(code.clone()) {
                    continue;
                }
                let title = a
                    .value()
                    .attr("title")
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| text_of(a));
                files.push(FoundFile {
                    code,
                    title,
                    page_url: abs.to_string(),
                    subdir: if group { subdir.clone() } else { String::new() },
                });
            }

            // sub-folders
            for a in doc.select(&s_folder) {
                if a.value()
                    .attr("class")
                    .is_some_and(|c| c.contains("back-btn"))
                {
                    continue;
                }
                let Some(abs) = a.value().attr("href").and_then(|h| base.join(h).ok()) else {
                    continue;
                };
                if let Some((LinkKind::Folder, code)) = classify(&abs) {
                    if seen_folders.insert(code) {
                        stack.push((abs.to_string(), path.clone(), false));
                    }
                }
            }

            // more pages of this same folder
            for a in doc.select(&s_page) {
                let Some(mut abs) = a.value().attr("href").and_then(|h| base.join(h).ok()) else {
                    continue;
                };
                abs.set_fragment(None);
                let same_folder = matches!(classify(&abs), Some((LinkKind::Folder, c)) if Some(&c) == this_code.as_ref());
                if same_folder && visited.insert(abs.to_string()) {
                    pages.push(abs.to_string());
                }
            }
            idx += 1;
        }
    }
    Ok((files, warnings))
}

// ───────────────────────── single videos (/d/… and /s/…) ─────────────────────────
//
// What the real /d/ page does: it contains no video. It sets `var SRC = '/xxxx?id=…&t=<token>'`
// and loads that URL in an iframe (the actual player). The page <title> is generic; the real
// title is inside the signed token `t` (base64 JSON, field "ti"). So we:
//   1. read the title from the token,
//   2. follow iframe / SRC urls (up to 3 levels, max 10 fetches),
//   3. look for the media file on every page we reach.

pub struct Probe {
    pub url: String,
    pub size: Option<u64>,
    pub ext: String,
    /// page the link was found on; the CDN may require it as Referer
    pub referer: String,
    /// the link is an HLS playlist: the parts are downloaded and joined
    pub hls: bool,
}

pub struct Media {
    pub title: String,
    pub probe: Probe,
}

pub struct ResolveErr {
    pub msg: String,
    /// true = retrying won't help (page gone, no video on it)
    pub permanent: bool,
}

enum Fetched {
    Html(String),
    Media(Probe),
    /// the page is not there
    Bad,
    /// the server was busy: worth trying again later
    Busy,
}

struct Ctx {
    title: Option<String>,
    seen: HashSet<String>,
    fetched: u32,
    /// every page we looked at, saved for diagnosis when nothing is found
    pages: Vec<(String, String)>,
    /// an HLS (.m3u8) stream was seen somewhere: we can't download those
    saw_hls: bool,
    /// the server was busy (429, 5xx, timeout): it is worth trying again later
    transient: bool,
    /// what the server said about the last candidate ("HTTP 404")
    note: String,
}

fn path_ext(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    let ext = Path::new(u.path())
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase();
    VIDEO_EXTS.contains(&ext.as_str()).then_some(ext)
}

fn is_asset(u: &str) -> bool {
    let l = u
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    [
        ".js", ".css", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico", ".woff", ".woff2",
        ".ttf", ".html",
    ]
    .iter()
    .any(|e| l.ends_with(e))
}

/// If the response is a real video file, describe it (type, size, extension).
fn media_from_response(resp: &ureq::Response, url: &str, referer: &str) -> Option<Probe> {
    let status = resp.status();
    let ct = resp
        .header("content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    if ct.starts_with("text/") || ct.contains("mpegurl") || ct.contains("json") {
        return None;
    }
    let from_path = path_ext(url);
    let is_media = ct.starts_with("video/")
        || ct.contains("octet-stream")
        || ct.contains("matroska")
        || from_path.is_some();
    if !is_media {
        return None;
    }
    let size = if status == 206 {
        resp.header("content-range")
            .and_then(|s| s.rsplit('/').next())
            .and_then(|n| n.trim().parse::<u64>().ok())
    } else {
        resp.header("content-length")
            .and_then(|n| n.trim().parse::<u64>().ok())
    };
    let from_disp = resp
        .header("content-disposition")
        .and_then(|d| d.split("filename").nth(1).map(str::to_string))
        .and_then(|f| {
            let name = f.trim_start_matches(['*', '=', '"', '\'', ' ']);
            let name = name.trim_end_matches(['"', '\'', ';', ' ']);
            Path::new(name)
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
        })
        .filter(|e| VIDEO_EXTS.contains(&e.as_str()));
    let from_ct = match ct.as_str() {
        c if c.contains("webm") => Some("webm".to_string()),
        c if c.contains("matroska") => Some("mkv".to_string()),
        c if c.contains("quicktime") => Some("mov".to_string()),
        _ => None,
    };
    let ext = from_disp
        .or(from_path)
        .or(from_ct)
        .unwrap_or_else(|| "mp4".into());
    Some(Probe {
        url: url.to_string(),
        size,
        ext,
        referer: referer.to_string(),
        hls: false,
    })
}

/// What a one-byte probe found.
enum Probed {
    /// a normal video file
    Video(Probe),
    /// an HLS playlist: fetch it completely to see what it is
    Playlist,
    /// nothing usable
    No,
    /// the server had a problem (timeout, 429, 5xx): worth trying again
    Retry,
}

/// Asks the server for the first byte only: "is this a real video, and how big".
fn probe(agent: &ureq::Agent, url: &str, referer: &str) -> Probed {
    let resp = match send(
        agent
            .get(url)
            .set("Referer", referer)
            .set("Range", "bytes=0-0"),
    ) {
        Ok(r) => r,
        Err(_) => return Probed::Retry,
    };
    let st = resp.status();
    if st == 429 || (500..600).contains(&st) {
        return Probed::Retry;
    }
    if st != 200 && st != 206 {
        return Probed::No;
    }
    let ct = resp
        .header("content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    if ct.contains("mpegurl") || ct.contains("m3u8") {
        return Probed::Playlist;
    }
    match media_from_response(&resp, url, referer) {
        Some(p) => Probed::Video(p),
        None => Probed::No,
    }
}

/// Looks at a full .m3u8 playlist: how long is it and is it a fragmented mp4?
/// Returns the error text when the server refused.
fn probe_hls(agent: &ureq::Agent, url: &str, referer: &str) -> Result<Option<Probe>, String> {
    let resp = send(agent.get(url).set("Referer", referer))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    let text = match resp.into_string() {
        Ok(t) => t,
        Err(e) => return Err(format!("network: {e}")),
    };
    if !text.trim_start().starts_with("#EXTM3U") {
        return Ok(None);
    }
    // a fragmented mp4 has an init segment and is saved as .mp4, everything else is MPEG-TS
    let ext = if text.contains("#EXT-X-MAP:") {
        "mp4"
    } else {
        "ts"
    };
    Ok(Some(Probe {
        url: url.to_string(),
        size: None,
        ext: ext.into(),
        referer: referer.to_string(),
        hls: true,
    }))
}

/// Does the URL look like an HLS playlist?
fn is_hls_url(url: &str) -> bool {
    let path = Url::parse(url)
        .ok()
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_else(|| url.to_ascii_lowercase());
    path.ends_with(".m3u8")
}

/// Checks one candidate. A CDN that hiccups for a moment (timeout, 429, 503) must not make the
/// whole video look broken, so the check is repeated a few times.
fn check_candidate(agent: &ureq::Agent, url: &str, referer: &str) -> (Option<Probe>, bool, String) {
    let mut busy = false;
    for attempt in 0..3u32 {
        match probe(agent, url, referer) {
            Probed::Video(p) => return (Some(p), busy, String::new()),
            Probed::Playlist => {
                return match probe_hls(agent, url, referer) {
                    Ok(Some(p)) => (Some(p), busy, String::new()),
                    Ok(None) => (None, busy, String::new()),
                    Err(e) => (None, busy, e),
                }
            }
            Probed::Retry if attempt < 2 => {
                busy = true; // the server had a problem
                std::thread::sleep(Duration::from_millis(800 * (attempt as u64 + 1)));
            }
            Probed::Retry => return (None, true, "the server did not answer".into()),
            Probed::No => break,
        }
    }
    // the playlist may be served as plain text
    if is_hls_url(url) {
        return match probe_hls(agent, url, referer) {
            Ok(Some(p)) => (Some(p), busy, String::new()),
            Ok(None) => (None, busy, String::new()),
            Err(e) => (None, busy, e),
        };
    }
    (None, busy, String::new())
}

/// One request that tells us whether a URL is a page (returns its text) or already the video.
fn fetch_page(agent: &ureq::Agent, url: &str, referer: &str) -> Fetched {
    let rq = agent.get(url).set("Referer", referer).set(
        "Accept",
        "text/html,application/xhtml+xml,application/json,*/*;q=0.8",
    );
    let Ok(resp) = send(rq) else {
        return Fetched::Busy;
    };
    let st = resp.status();
    if st == 429 || (500..600).contains(&st) {
        return Fetched::Busy;
    }
    if !(200..300).contains(&st) {
        return Fetched::Bad;
    }
    let ct = resp
        .header("content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    if ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("javascript")
        || ct.contains("xml")
    {
        return match resp.into_string() {
            Ok(t) => Fetched::Html(t),
            Err(_) => Fetched::Bad,
        };
    }
    match media_from_response(&resp, url, referer) {
        Some(p) => Fetched::Media(p),
        None => Fetched::Bad,
    }
}

fn page_title(doc: &Html) -> String {
    if let Some(m) = doc.select(&sel("meta[property='og:title']")).next() {
        if let Some(c) = m.value().attr("content") {
            if !c.trim().is_empty() {
                return c.trim().to_string();
            }
        }
    }
    for s in ["h1", "title"] {
        if let Some(el) = doc.select(&sel(s)).next() {
            let t = text_of(el);
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

/// The player URL carries `t=<base64 json>.<signature>`; the JSON's "ti" is the video title.
/// The base64 part is the part before the dot; we try URL-safe first, then standard.
pub fn title_from_token(text: &str) -> Option<String> {
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    use base64::Engine;
    static RE: OnceLock<Regex> = OnceLock::new();
    // the base64 alphabet has no '.', so group 1 stops right before the signature
    let re = RE.get_or_init(|| {
        Regex::new(r"[?&]t=([A-Za-z0-9_\-+/=]{4,})(?:\.([0-9A-Fa-f]{4,}))?").unwrap()
    });
    for c in re.captures_iter(text) {
        let raw = c[1].trim_end_matches('='); // padded base64 would fail the NoPad engines
        if raw.is_empty() {
            continue;
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .or_else(|_| STANDARD_NO_PAD.decode(raw))
            .ok();
        let Some(bytes) = bytes else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let Some(t) = v.get("ti").and_then(|t| t.as_str()) {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    None
}

fn media_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)https?:(?:\\?/){2}[^\s"'<>]+?\.(?:mp4|webm|mkv|mov|m4v|m3u8)(?:\?[^\s"'<>]*)?"#,
        )
        .unwrap()
    })
}

fn rel_media_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)["']((?:https?:)?/[^"'\s]+?\.(?:mp4|webm|mkv|mov|m4v|m3u8)(?:\?[^"'\s]*)?)["']"#,
        )
        .unwrap()
    })
}

/// `file: "https://…"`, `src = '//cdn/…'`, `file:"/media/xyz"` etc. (extension-less signed links)
fn js_url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(?:file|src|source|url|video_?url|stream)\b["']?\s*[:=]\s*["']((?:https?:)?//[^"'\s]+|/[^"'\s]*)["']"#).unwrap()
    })
}

/// `var SRC = '/path?id=…&t=…'` or `el.src = '…'`: how the /d/ page hands over the player URL.
fn embed_var_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(?:(?:var|let|const)\s+\w+|\.src)\s*=\s*['"]((?:https?:)?/[^'"\s]*\?[^'"\s]*)['"]"#).unwrap()
    })
}

/// (likely direct media URLs, pages that might lead to the media), best guess first.
fn candidates(doc: &Html, html: &str, base: &Url) -> (Vec<String>, Vec<String>) {
    let norm = html
        .replace("\\/", "/")
        .replace("\\u0026", "&")
        .replace("&amp;", "&");
    // a URL may show up in both lists (e.g. `var SRC = '/embed?…'` is a "src" assignment *and*
    // a follow-up page), so each list gets its own "already seen" set.
    let mut seen_direct: HashSet<String> = HashSet::new();
    let mut seen_pages: HashSet<String> = HashSet::new();
    let mut direct: Vec<String> = Vec::new();
    let mut pages: Vec<String> = Vec::new();

    let mut push =
        |raw: &str, list_direct: bool, direct: &mut Vec<String>, pages: &mut Vec<String>| {
            let raw = raw.trim();
            if raw.is_empty()
                || raw.starts_with('#')
                || raw.starts_with("javascript:")
                || raw.starts_with("data:")
                || raw.starts_with("blob:")
            {
                return;
            }
            if let Ok(u) = base.join(raw) {
                let s = u.to_string();
                let seen = if list_direct {
                    &mut seen_direct
                } else {
                    &mut seen_pages
                };
                if seen.insert(s.clone()) {
                    if list_direct {
                        direct.push(s);
                    } else {
                        pages.push(s);
                    }
                }
            }
        };

    // 1. explicit media elements
    for (s, attr) in [
        ("video source[src]", "src"),
        ("video[src]", "src"),
        ("source[src]", "src"),
        ("meta[property='og:video']", "content"),
        ("meta[property='og:video:url']", "content"),
        ("meta[property='og:video:secure_url']", "content"),
        ("meta[name='twitter:player:stream']", "content"),
        ("a[download][href]", "href"),
        ("[data-video]", "data-video"),
        ("[data-src]", "data-src"),
        ("[data-url]", "data-url"),
    ] {
        for el in doc.select(&sel(s)) {
            if let Some(v) = el.value().attr(attr) {
                if !is_asset(v) {
                    push(v, true, &mut direct, &mut pages);
                }
            }
        }
    }
    // 2. media file URLs written in scripts / JSON
    for m in media_regex().find_iter(&norm) {
        push(m.as_str(), true, &mut direct, &mut pages);
    }
    for c in rel_media_regex().captures_iter(&norm) {
        push(&c[1], true, &mut direct, &mut pages);
    }
    // 3. extension-less links assigned to file/src/url in scripts
    for c in js_url_regex().captures_iter(&norm) {
        if !is_asset(&c[1]) {
            push(&c[1], true, &mut direct, &mut pages);
        }
    }
    // 4. the player iframe (the real /d/ pages work this way)
    for el in doc.select(&sel("iframe[src], iframe[data-src], embed[src]")) {
        for attr in ["src", "data-src"] {
            if let Some(v) = el.value().attr(attr) {
                push(v, false, &mut direct, &mut pages);
            }
        }
    }
    for c in embed_var_regex().captures_iter(&norm) {
        if !c[1].contains("cdn-cgi") {
            push(&c[1], false, &mut direct, &mut pages);
        }
    }
    // 5. a "Download" button leads to the file or to one more page
    for a in doc.select(&sel("a[href]")) {
        let href = a.value().attr("href").unwrap_or("");
        let label = text_of(a).to_ascii_lowercase();
        let h = href.to_ascii_lowercase();
        if label.contains("download")
            || h.contains("download")
            || h.contains("/dl/")
            || h.contains("/get/")
        {
            push(href, false, &mut direct, &mut pages);
        }
    }
    direct.truncate(15);
    pages.truncate(6);
    (direct, pages)
}

fn find_media(
    agent: &ureq::Agent,
    page_url: &str,
    html: &str,
    depth: u8,
    ctx: &mut Ctx,
) -> Option<Probe> {
    let base = Url::parse(page_url).ok()?;
    if ctx.title.is_none() {
        ctx.title = title_from_token(&html.replace("\\/", "/").replace("&amp;", "&"));
    }
    if html.to_ascii_lowercase().contains(".m3u8") {
        ctx.saw_hls = true;
    }
    let (direct, pages) = {
        let doc = Html::parse_document(html);
        candidates(&doc, html, &base)
    };
    for u in &direct {
        let (p, busy, note) = check_candidate(agent, u, page_url);
        if busy {
            ctx.transient = true;
        }
        if !note.is_empty() && ctx.note.is_empty() {
            ctx.note = note;
        }
        if let Some(p) = p {
            return Some(p);
        }
    }
    if depth < 3 {
        for u in &pages {
            if u.to_ascii_lowercase().contains(".m3u8") {
                ctx.saw_hls = true;
            }
            if ctx.fetched >= 10 || !ctx.seen.insert(u.clone()) {
                continue;
            }
            ctx.fetched += 1;
            match fetch_page(agent, u, page_url) {
                Fetched::Media(p) => return Some(p),
                Fetched::Html(h) => {
                    ctx.pages.push((u.clone(), h.clone()));
                    if let Some(p) = find_media(agent, u, &h, depth + 1, ctx) {
                        return Some(p);
                    }
                }
                Fetched::Busy => ctx.transient = true,
                Fetched::Bad => {}
            }
        }
    }
    None
}

/// Opens a /d/ or /s/ page and works out the real video file behind it.
/// When it can't, every page it looked at is saved in `debug_dir` so the logic can be adjusted.
pub fn resolve_media(
    agent: &ureq::Agent,
    page_url: &str,
    debug_dir: &Path,
    code: &str,
) -> Result<Media, ResolveErr> {
    let html = get_html(agent, page_url, None).map_err(|e| ResolveErr {
        // only a missing page is permanent: everything else (busy server, timeout) is worth
        // another try later
        permanent: e == "HTTP 404" || e == "HTTP 410",
        msg: e,
    })?;
    let mut ctx = Ctx {
        title: None,
        seen: HashSet::from([page_url.to_string()]),
        fetched: 0,
        pages: Vec::new(),
        saw_hls: false,
        transient: false,
        note: String::new(),
    };
    match find_media(agent, page_url, &html, 0, &mut ctx) {
        Some(probe) => {
            let title = ctx
                .title
                .take()
                .unwrap_or_else(|| page_title(&Html::parse_document(&html)));
            Ok(Media { title, probe })
        }
        None => {
            let _ = std::fs::create_dir_all(debug_dir);
            let save = |name: String, url: &str, body: &str| {
                let _ = std::fs::write(
                    debug_dir.join(name),
                    format!("<!-- fetched from: {url} -->\n{body}"),
                );
            };
            save(format!("{code}.html"), page_url, &html);
            let mut names = vec![format!("{code}.html")];
            for (i, (u, body)) in ctx.pages.iter().enumerate() {
                save(format!("{code}.{}.html", i + 1), u, body);
                names.push(format!("{code}.{}.html", i + 1));
            }
            let hint = if ctx.saw_hls {
                " An HLS (.m3u8) stream was found, but it could not be read."
            } else {
                ""
            };
            let said = if ctx.note.is_empty() {
                String::new()
            } else {
                format!(" The server said: {}.", ctx.note)
            };
            let msg = format!(
                "no video link found ({} page(s) checked).{hint}{said} If you use a VPN or Cloudflare WARP, turn it off. Pages saved in {}",
                1 + ctx.pages.len(),
                debug_dir.display()
            );
            // one line in the failure log, with the html files that belong to it
            crate::model::log_failure(debug_dir, code, page_url, &msg, &names);
            Err(ResolveErr {
                permanent: !ctx.transient,
                msg,
            })
        }
    }
}
