//! HLS (.m3u8) support.
//!
//! The rest of the app works on whole files, but a stream is a list of small files. So this
//! module reads the playlist, downloads the parts one by one and appends them to the `.part`
//! file of the video, exactly like a normal download does. When the app is closed in the
//! middle, the finished parts stay on disk and the next run continues after them.
//!
//! Supported: master playlists (the best variant is used), media playlists, EXT-X-MAP
//! (fragmented mp4), EXT-X-BYTERANGE and AES-128 encryption.

use crate::downloader::{Fail, Outcome};
use crate::model::*;
use crate::resolver;
use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use url::Url;

type Aes128Cbc = cbc::Decryptor<aes::Aes128>;

// ───────────────────────── playlist ─────────────────────────

/// One part of the stream: a normal segment, or the init segment of a fragmented mp4.
#[derive(Clone, Debug)]
pub struct Part {
    pub url: String,
    /// AES-128 key for this part (None = not encrypted)
    pub key: Option<Key>,
    /// byte range inside the segment file (EXT-X-BYTERANGE)
    pub range: Option<(u64, u64)>,
    /// media sequence number (the IV of an encrypted part is built from it)
    pub seq: u64,
}

#[derive(Clone, Debug)]
pub struct Key {
    pub method: String,
    pub uri: String,
    pub iv: Option<[u8; 16]>,
}

pub struct Playlist {
    pub parts: Vec<Part>,
    /// total length in seconds
    pub duration: f64,
    /// fMP4 streams have an init segment and are saved as .mp4
    pub fragmented: bool,
    /// bitrate of the chosen variant in bytes per second (only known for a master playlist)
    pub bitrate: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub url: String,
    pub bandwidth: u64,
}

/// Splits `BANDWIDTH=1,CODECS="a,b"` into its key/value pairs (quotes are respected).
fn split_attrs(list: &str) -> Vec<(String, String)> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in list.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => chunks.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    chunks.push(cur);
    chunks
        .into_iter()
        .filter_map(|s| {
            let s = s.trim();
            let (k, v) = s.split_once('=')?;
            Some((
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_string(),
            ))
        })
        .collect()
}

fn attr(list: &str, key: &str) -> Option<String> {
    split_attrs(list)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

/// The variants of a master playlist.
pub fn parse_master(text: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    let mut pending: Option<u64> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = attr(rest, "BANDWIDTH").and_then(|v| v.parse().ok());
        } else if line.starts_with('#') || line.is_empty() {
            continue;
        } else {
            out.push(Variant {
                url: line.to_string(),
                bandwidth: pending.unwrap_or(0),
            });
            pending = None;
        }
    }
    out
}

/// The parts of a media playlist. `base` is the URL the playlist came from.
pub fn parse_media(text: &str, base: &str) -> Playlist {
    let mut parts: Vec<Part> = Vec::new();
    let mut key: Option<Key> = None;
    let mut init: Option<String> = None;
    let mut duration = 0.0f64;
    let mut pending_dur = 0.0f64;
    let mut pending_range: Option<(u64, u64)> = None;
    let mut next_offset = 0u64;
    let mut seq = 0u64;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending_dur = rest
                .split(',')
                .next()
                .unwrap_or("0")
                .trim()
                .parse()
                .unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            key = parse_key(rest, base);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri) = attr(rest, "URI") {
                init = Some(resolve(base, &uri));
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(parse_byterange(rest, next_offset));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = rest.trim().parse().unwrap_or(0);
        } else if line.starts_with('#') {
            continue;
        } else {
            let range = pending_range.take();
            if let Some((off, len)) = range {
                next_offset = off + len;
            }
            parts.push(Part {
                url: resolve(base, line),
                key: key.clone(),
                range,
                seq,
            });
            seq += 1;
            duration += pending_dur;
            pending_dur = 0.0;
        }
    }

    let fragmented = init.is_some();
    if let Some(u) = init {
        // the init segment comes first in the file
        parts.insert(
            0,
            Part {
                url: u,
                key: key.clone(),
                range: None,
                seq: 0,
            },
        );
    }
    Playlist {
        parts,
        duration,
        fragmented,
        bitrate: None,
    }
}

fn parse_key(rest: &str, base: &str) -> Option<Key> {
    let method = attr(rest, "METHOD")?;
    if method.eq_ignore_ascii_case("NONE") {
        return None;
    }
    let uri = attr(rest, "URI")?;
    let iv = attr(rest, "IV").and_then(|v| {
        let hex = v.trim_start_matches("0x").trim_start_matches("0X");
        if hex.len() != 32 {
            return None;
        }
        let mut iv = [0u8; 16];
        for (i, b) in iv.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(iv)
    });
    Some(Key {
        method,
        uri: resolve(base, &uri),
        iv,
    })
}

fn parse_byterange(s: &str, next: u64) -> (u64, u64) {
    let s = s.trim();
    match s.split_once('@') {
        Some((len, off)) => (
            off.trim().parse().unwrap_or(next),
            len.trim().parse().unwrap_or(0),
        ),
        None => (next, s.parse().unwrap_or(0)),
    }
}

/// Makes a URI from the playlist absolute.
pub fn resolve(base: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_string();
    }
    Url::parse(base)
        .and_then(|b| b.join(uri))
        .map(|u| u.to_string())
        .unwrap_or_else(|_| uri.to_string())
}

/// Does this look like an HLS playlist?
pub fn is_playlist(text: &str) -> bool {
    text.trim_start().starts_with("#EXTM3U")
}

// ───────────────────────── decryption ─────────────────────────

/// AES-128-CBC without touching the padding.
pub fn aes_cbc_decrypt_raw(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return None;
    }
    let dec = Aes128Cbc::new(key.into(), iv.into());
    let mut buf = data.to_vec();
    dec.decrypt_padded_mut::<NoPadding>(&mut buf)
        .ok()
        .map(|v| v.to_vec())
}

/// AES-128-CBC and the PKCS#7 padding of a HLS segment.
pub fn aes_cbc_decrypt_padded(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let plain = aes_cbc_decrypt_raw(key, iv, data)?;
    // some servers do not pad the last block: keep the data if the padding is not valid
    Some(strip_pkcs7(&plain).unwrap_or(plain))
}

/// Removes the PKCS#7 padding.
pub fn strip_pkcs7(data: &[u8]) -> Option<Vec<u8>> {
    let n = *data.last()? as usize;
    if n == 0 || n > 16 || n > data.len() {
        return None;
    }
    if !data[data.len() - n..].iter().all(|&b| b as usize == n) {
        return None;
    }
    Some(data[..data.len() - n].to_vec())
}

/// The IV of a part that does not name one: the media sequence number, big endian.
pub fn iv_from_seq(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

// ───────────────────────── downloading ─────────────────────────

fn fetch_text(sh: &Shared, url: &str, referer: &str) -> Result<String, Fail> {
    let resp = resolver::send(sh.agent.get(url).set("Referer", referer)).map_err(Fail::Retry)?;
    if !(200..300).contains(&resp.status()) {
        return Err(Fail::Retry(format!("HTTP {}", resp.status())));
    }
    resp.into_string()
        .map_err(|e| Fail::Retry(format!("network: {e}")))
}

/// Downloads one part of the stream.
fn fetch_part(
    sh: &Shared,
    part: &Part,
    referer: &str,
    keys: &mut HashMap<String, [u8; 16]>,
) -> Result<Vec<u8>, Fail> {
    let mut rq = sh.agent.get(&part.url).set("Referer", referer);
    if let Some((off, len)) = part.range {
        let end = if len > 0 { off + len - 1 } else { off };
        rq = rq.set("Range", &format!("bytes={off}-{end}"));
    }
    let resp = resolver::send(rq).map_err(Fail::Retry)?;
    let status = resp.status();
    match status {
        200 | 206 => {}
        401 | 403 | 404 | 410 => return Err(Fail::Relink(format!("HTTP {status}"))),
        429 | 500..=599 => return Err(Fail::Retry(format!("server busy (HTTP {status})"))),
        _ => return Err(Fail::Retry(format!("HTTP {status}"))),
    }
    let mut data = Vec::new();
    resp.into_reader()
        .read_to_end(&mut data)
        .map_err(|e| Fail::Retry(format!("connection lost: {e}")))?;

    // decrypt, if the playlist says so
    let Some(key) = &part.key else {
        return Ok(data);
    };
    if !key.method.eq_ignore_ascii_case("AES-128") {
        return Err(Fail::Fatal(format!(
            "unsupported encryption {} (only AES-128 is supported)",
            key.method
        )));
    }
    let key_bytes = match keys.get(&key.uri) {
        Some(k) => *k,
        None => {
            let raw = fetch_text(sh, &key.uri, referer)?;
            let raw = raw.as_bytes();
            if raw.len() < 16 {
                return Err(Fail::Fatal("the encryption key is too short".into()));
            }
            let mut k = [0u8; 16];
            k.copy_from_slice(&raw[..16]);
            keys.insert(key.uri.clone(), k);
            k
        }
    };
    let iv = key.iv.unwrap_or_else(|| iv_from_seq(part.seq));
    let plain = aes_cbc_decrypt_padded(&key_bytes, &iv, &data)
        .ok_or_else(|| Fail::Fatal("could not decrypt the segment".into()))?;
    Ok(plain)
}

/// Downloads a whole HLS stream into the `.part` file of the item.
pub fn attempt(
    sh: &Shared,
    id: u64,
    item: &Item,
    stop: &AtomicBool,
    dir: &Path,
    part_out: &mut Option<PathBuf>,
    progressed: &mut bool,
) -> Result<Outcome, Fail> {
    let playlist_url = item
        .direct_url
        .clone()
        .ok_or_else(|| Fail::Relink("no link".into()))?;
    let referer = item.referer.as_deref().unwrap_or(item.page_url.as_str());

    // the playlist itself, and the variant it points to
    let text = fetch_text(sh, &playlist_url, referer)?;
    let (media_text, media_url, bitrate) =
        if is_playlist(&text) && text.contains("#EXT-X-STREAM-INF") {
            let variants = parse_master(&text);
            let best = variants
                .iter()
                .max_by_key(|v| v.bandwidth)
                .ok_or_else(|| Fail::Fatal("the playlist has no variants".into()))?;
            let vurl = resolve(&playlist_url, &best.url);
            let t = fetch_text(sh, &vurl, referer)?;
            (t, vurl, Some(best.bandwidth / 8))
        } else {
            (text, playlist_url.clone(), None)
        };
    let mut pl = parse_media(&media_text, &media_url);
    pl.bitrate = bitrate;
    if pl.parts.is_empty() {
        return Err(Fail::Fatal("the playlist has no parts".into()));
    }

    // a fragmented mp4 is saved as .mp4, everything else as .ts
    let ext = if pl.fragmented && item.ext == "ts" {
        "mp4".to_string()
    } else {
        item.ext.clone()
    };
    let part = dir.join(format!("{}.{}.part", item.stem, ext));
    let fin = dir.join(format!("{}.{}", item.stem, ext));
    *part_out = Some(part.clone());

    // where do we continue? (the state may be newer than the copy we were given)
    let fresh = {
        let st = sh.lock();
        st.items.iter().find(|i| i.id == id).cloned()
    };
    let done = fresh.as_ref().map(|i| i.hls_done as usize).unwrap_or(0);
    let done = done.min(pl.parts.len());
    let known_bytes = fresh.as_ref().map(|i| i.hls_bytes).unwrap_or(0);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false) // the parts that are already there are kept
        .open(&part)
        .map_err(|e| Fail::Fatal(format!("can't write to {}: {e}", part.display())))?;
    // throw away a part that was not finished (the app was closed in the middle of it)
    let len = file
        .metadata()
        .map(|m| m.len())
        .map_err(|e| Fail::Fatal(format!("can't read {}: {e}", part.display())))?;
    if len > known_bytes {
        let _ = file.set_len(known_bytes);
    }
    file.seek(SeekFrom::Start(known_bytes))
        .map_err(|e| Fail::Fatal(format!("can't write to {}: {e}", part.display())))?;

    sh.update(id, |it| {
        it.hls = true;
        it.ext = ext.clone();
        it.hls_total = pl.parts.len() as u32;
        it.hls_done = done as u32;
        it.hls_bytes = known_bytes;
        it.downloaded = known_bytes;
        it.error.clear();
    });

    let mut keys: HashMap<String, [u8; 16]> = HashMap::new();
    let mut written = known_bytes;
    let mut last_sync = std::time::Instant::now();
    let mut win_start = std::time::Instant::now();
    let mut win_bytes = 0u64;

    for (i, p) in pl.parts.iter().enumerate().skip(done) {
        if stop.load(Ordering::Relaxed) {
            let _ = file.flush();
            sh.update(id, |it| it.downloaded = written);
            return Ok(Outcome::Stopped);
        }
        let data = fetch_part(sh, p, referer, &mut keys)?;
        file.write_all(&data)
            .map_err(|e| Fail::Fatal(format!("write failed: {e}")))?;
        written += data.len() as u64;
        win_bytes += data.len() as u64;
        *progressed = true;

        // a rough size, so the progress bar moves
        let done_now = i + 1;
        let avg = if done_now > 0 {
            written / done_now as u64
        } else {
            0
        };
        let by_parts = written + avg * (pl.parts.len() - done_now) as u64;
        let by_time = pl
            .bitrate
            .map(|b| (pl.duration * b as f64) as u64)
            .unwrap_or(0);
        let estimate = by_time.max(by_parts).max(written);

        // the speed of the last few seconds
        let elapsed = win_start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.05 {
            win_bytes as f64 / elapsed
        } else {
            0.0
        };

        sh.update(id, |it| {
            it.hls_done = done_now as u32;
            it.hls_bytes = written;
            it.downloaded = written;
            it.size = Some(estimate);
            it.speed = speed;
        });
        if win_start.elapsed() >= std::time::Duration::from_secs(3) {
            win_start = std::time::Instant::now();
            win_bytes = 0;
        }
        if last_sync.elapsed() >= std::time::Duration::from_secs(3) {
            let _ = file.flush();
            let _ = file.sync_data();
            last_sync = std::time::Instant::now();
        }
    }

    let _ = file.flush();
    file.sync_all()
        .map_err(|e| Fail::Fatal(format!("could not write to disk: {e}")))?;
    drop(file);

    if fin.exists() {
        let _ = std::fs::remove_file(&fin);
    }
    std::fs::rename(&part, &fin).map_err(|e| Fail::Fatal(format!("couldn't finish file: {e}")))?;
    sh.update(id, |it| {
        it.downloaded = written;
        it.size = Some(written);
    });
    Ok(Outcome::Done)
}
