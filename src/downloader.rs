//! Phase 2: the resumable downloader. One call of `run` = one video, on its own thread.

use crate::hls;
use crate::model::*;
use crate::resolver;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum Outcome {
    Done,
    Stopped,
}

pub enum Fail {
    /// transient: wait, then resume where we left off
    Retry(String),
    /// the direct link is dead or expired: look it up again from the page
    Relink(String),
    /// no point trying again (disk full, can't write…)
    Fatal(String),
}

pub fn run(sh: Arc<Shared>, id: u64, stop: Arc<AtomicBool>) {
    let mut part: Option<PathBuf> = None;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        download(&sh, id, &stop, &mut part)
    }))
    .unwrap_or_else(|_| Err("internal error".to_string()));
    {
        let mut st = sh.lock();
        match st.items.iter_mut().find(|i| i.id == id) {
            Some(it) => {
                it.speed = 0.0;
                // if the user paused/removed meanwhile, their choice wins
                if it.status == ItemStatus::Downloading {
                    match res {
                        Ok(Outcome::Done) => {
                            it.status = ItemStatus::Done;
                            it.error.clear();
                            if let Some(s) = it.size {
                                it.downloaded = s;
                            }
                        }
                        Ok(Outcome::Stopped) => it.status = ItemStatus::Queued,
                        Err(ref e) => {
                            it.status = ItemStatus::Failed;
                            it.error = e.clone();
                        }
                    }
                }
            }
            None => {
                // removed while running: clean up the half-downloaded file
                if let Some(p) = part {
                    let _ = fs::remove_file(p);
                }
            }
        }
    }
    // one line in the failure log, with the html files of this video
    if let Err(e) = &res {
        let (code, page) = {
            let st = sh.lock();
            match st.items.iter().find(|i| i.id == id) {
                Some(it) => (it.code.clone(), it.page_url.clone()),
                None => (String::new(), String::new()),
            }
        };
        if !code.is_empty() {
            let html = sh.paths.debug_files(&code);
            log_failure(&sh.paths.debug_dir(), &code, &page, e, &html);
        }
    }
    sh.touch();
}

fn download(
    sh: &Shared,
    id: u64,
    stop: &AtomicBool,
    part_out: &mut Option<PathBuf>,
) -> Result<Outcome, String> {
    let (mut item, retries) = {
        let st = sh.lock();
        let it = st
            .items
            .iter()
            .find(|i| i.id == id)
            .ok_or("item removed")?
            .clone();
        (it, st.settings.retries)
    };
    let dir = sh.item_dir(&item);
    fs::create_dir_all(&dir).map_err(|e| format!("can't create folder {}: {e}", dir.display()))?;

    // the name of the file must not be used by another video
    let stem = sh.unique_stem(&item);
    if stem != item.stem {
        item.stem = stem.clone();
        sh.update(id, |it| it.stem = stem.clone());
    }

    let mut attempt: u32 = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(Outcome::Stopped);
        }

        // make sure we have a working direct link
        if item.direct_url.is_none() {
            match resolver::resolve_media(
                &sh.agent,
                &item.page_url,
                &sh.paths.debug_dir(),
                &item.code,
            ) {
                Ok(m) => {
                    item.direct_url = Some(m.probe.url.clone());
                    item.size = m.probe.size.or(item.size);
                    item.ext = m.probe.ext.clone();
                    item.referer = Some(m.probe.referer.clone());
                    item.hls = m.probe.hls;
                    let (u, s, e, r, h) = (
                        m.probe.url,
                        item.size,
                        item.ext.clone(),
                        m.probe.referer,
                        m.probe.hls,
                    );
                    sh.update(id, |it| {
                        it.direct_url = Some(u);
                        it.size = s;
                        it.ext = e;
                        it.referer = Some(r);
                        it.hls = h;
                    });
                    sh.touch();
                }
                Err(e) => {
                    attempt += 1;
                    if e.permanent || attempt > retries {
                        return Err(e.msg);
                    }
                    note_retry(sh, id, attempt, retries, &e.msg);
                    if backoff(stop, attempt) {
                        return Ok(Outcome::Stopped);
                    }
                    continue;
                }
            }
        }

        let mut progressed = false;
        let r = if item.hls {
            // an HLS stream: the parts are downloaded one by one and joined
            hls::attempt(sh, id, &item, stop, &dir, part_out, &mut progressed)
        } else {
            attempt_once(sh, id, &item, stop, &dir, part_out, &mut progressed)
        };
        if progressed {
            attempt = 0; // a flaky connection that keeps making progress never gives up
        }
        match r {
            Ok(o) => return Ok(o),
            Err(Fail::Fatal(e)) => return Err(e),
            Err(Fail::Relink(e)) => {
                attempt += 1;
                if attempt > retries {
                    return Err(e);
                }
                note_retry(sh, id, attempt, retries, &e);
                item.direct_url = None;
                sh.update(id, |it| it.direct_url = None);
            }
            Err(Fail::Retry(e)) => {
                attempt += 1;
                if attempt > retries {
                    return Err(e);
                }
                note_retry(sh, id, attempt, retries, &e);
                if backoff(stop, attempt) {
                    return Ok(Outcome::Stopped);
                }
            }
        }
    }
}

fn note_retry(sh: &Shared, id: u64, attempt: u32, retries: u32, msg: &str) {
    // "retry n/max: reason" – shown on the row while we wait
    sh.update(id, |it| {
        it.error = format!("retry {attempt}/{retries}: {msg}");
        it.speed = 0.0;
    });
}

/// Waits 2, 4, 8… up to 60 s. Returns true if the user stopped us meanwhile.
fn backoff(stop: &AtomicBool, attempt: u32) -> bool {
    let secs = 2u64.saturating_pow(attempt.min(6)).min(60);
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    stop.load(Ordering::Relaxed)
}

fn parse_content_range(resp: &ureq::Response) -> (Option<u64>, Option<u64>) {
    // "bytes START-END/TOTAL"
    let Some(v) = resp.header("content-range") else {
        return (None, None);
    };
    let v = v.trim().trim_start_matches("bytes").trim();
    let (range, total) = v.split_once('/').unwrap_or((v, ""));
    let start = range
        .split('-')
        .next()
        .and_then(|s| s.trim().parse::<u64>().ok());
    (start, total.trim().parse::<u64>().ok())
}

fn finalize(part: &Path, fin: &Path) -> Result<(), Fail> {
    if fin.exists() {
        let _ = fs::remove_file(fin);
    }
    fs::rename(part, fin).map_err(|e| Fail::Fatal(format!("couldn't finish file: {e}")))
}

fn attempt_once(
    sh: &Shared,
    id: u64,
    item: &Item,
    stop: &AtomicBool,
    dir: &Path,
    part_out: &mut Option<PathBuf>,
    progressed: &mut bool,
) -> Result<Outcome, Fail> {
    let url = item
        .direct_url
        .clone()
        .ok_or_else(|| Fail::Relink("no link".into()))?;
    // the name must be unique, so two videos with the same title never share a file
    let stem = sh.unique_stem(item);
    let part = dir.join(part_name_of(&stem, &item.ext));
    let fin = dir.join(file_name_of(&stem, &item.ext));
    *part_out = Some(part.clone());

    // already there (e.g. finished on another computer before the queue was saved)
    if let Some(sz) = item.size {
        if fs::metadata(&fin).map(|m| m.len() == sz).unwrap_or(false) {
            sh.update(id, |it| it.downloaded = sz);
            return Ok(Outcome::Done);
        }
    }

    let existing = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let referer = item.referer.as_deref().unwrap_or(item.page_url.as_str());
    let mut rq = sh.agent.get(&url).set("Referer", referer);
    if existing > 0 {
        rq = rq.set("Range", &format!("bytes={existing}-"));
    }
    let resp = resolver::send(rq).map_err(Fail::Retry)?;
    let status = resp.status();

    match status {
        200 | 206 => {}
        416 => {
            let (_, total) = parse_content_range(&resp);
            if existing > 0 && total == Some(existing) {
                finalize(&part, &fin)?;
                sh.update(id, |it| it.downloaded = existing);
                return Ok(Outcome::Done);
            }
            let _ = fs::remove_file(&part);
            return Err(Fail::Retry(
                "server refused to resume, starting over".into(),
            ));
        }
        401 | 403 | 404 | 410 => return Err(Fail::Relink(format!("link expired (HTTP {status})"))),
        429 | 500..=599 => return Err(Fail::Retry(format!("server busy (HTTP {status})"))),
        _ => return Err(Fail::Retry(format!("HTTP {status}"))),
    }
    let ct = resp
        .header("content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    if ct.starts_with("text/html") {
        return Err(Fail::Relink("got a web page instead of a video".into()));
    }

    // where does this response start, and how big is the whole file?
    let (offset, total) = if status == 206 {
        let (start, total) = parse_content_range(&resp);
        if start != Some(existing) {
            let _ = fs::remove_file(&part);
            return Err(Fail::Retry(
                "resume position mismatch, starting over".into(),
            ));
        }
        (existing, total.or(item.size))
    } else {
        // 200: the server ignored our Range request, so this is the whole file again
        (
            0,
            resp.header("content-length")
                .and_then(|n| n.trim().parse::<u64>().ok())
                .or(item.size),
        )
    };

    if let Some(t) = total {
        let need = t.saturating_sub(offset);
        if let Ok(avail) = fs2::available_space(dir) {
            if avail < need + 64 * 1024 * 1024 {
                return Err(Fail::Fatal(format!(
                    "not enough free space (need {}, free {})",
                    fmt_bytes(need),
                    fmt_bytes(avail)
                )));
            }
        }
    }

    let file: File = if offset > 0 {
        OpenOptions::new().append(true).open(&part)
    } else {
        File::create(&part)
    }
    .map_err(|e| Fail::Fatal(format!("can't write to {}: {e}", part.display())))?;

    sh.update(id, |it| {
        it.size = total.or(it.size);
        it.downloaded = offset;
        it.error.clear();
    });

    let mut reader = resp.into_reader();
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let mut buf = vec![0u8; 256 * 1024];
    let mut written = offset;
    let (mut last_ui, mut win_start, mut win_bytes) = (Instant::now(), Instant::now(), 0u64);

    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = w.flush();
            sh.update(id, |it| it.downloaded = written);
            return Ok(Outcome::Stopped);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = w.flush();
                sh.update(id, |it| it.downloaded = written);
                return Err(Fail::Retry(format!("connection lost: {e}")));
            }
        };
        w.write_all(&buf[..n])
            .map_err(|e| Fail::Fatal(format!("write failed: {e}")))?;
        *progressed = true;
        written += n as u64;
        win_bytes += n as u64;
        if last_ui.elapsed() >= Duration::from_millis(300) {
            let speed = win_bytes as f64 / win_start.elapsed().as_secs_f64().max(0.001);
            sh.update(id, |it| {
                it.downloaded = written;
                it.speed = speed;
            });
            last_ui = Instant::now();
            if win_start.elapsed() > Duration::from_secs(3) {
                win_start = Instant::now();
                win_bytes = 0;
            }
        }
    }

    let file = w
        .into_inner()
        .map_err(|e| Fail::Fatal(format!("write failed: {e}")))?;
    let _ = file.sync_all(); // important on external drives: make sure it's really on the disk
    drop(file);

    if let Some(t) = total.or(item.size) {
        if written != t {
            sh.update(id, |it| it.downloaded = written);
            return Err(Fail::Retry(format!(
                "incomplete ({} of {})",
                fmt_bytes(written),
                fmt_bytes(t)
            )));
        }
    }
    finalize(&part, &fin)?;
    sh.update(id, |it| {
        it.downloaded = written;
        it.size = Some(written);
    });
    Ok(Outcome::Done)
}
