//! Finding videos that were downloaded twice.
//!
//! The same video often shows up under different codes (the site uploads it more than once),
//! and because two videos with the same title may not share a file name, the second one gets
//! " [code]" added to its name. These are the files that can be thrown away.
//!
//! Two videos are only treated as the same when their contents really match: the size and a
//! fingerprint of the beginning and the end of the file have to be equal.

use crate::model::*;
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The name of the video without the " [code]" that is added when two videos share a name.
pub fn base_name(stem: &str) -> &str {
    match stem.rfind(" [") {
        Some(i) if stem.ends_with(']') => &stem[..i],
        _ => stem,
    }
}

/// The size and a fingerprint of the file (the beginning and the end are enough).
pub fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 {
        return Some((0, 0));
    }
    let mut f = File::open(path).ok()?;
    let mut h = DefaultHasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    let n = f.read(&mut buf).ok()?;
    h.write(&buf[..n]);
    if len > buf.len() as u64 {
        f.seek(SeekFrom::End(-(buf.len() as i64))).ok()?;
        let n = f.read(&mut buf).ok()?;
        h.write(&buf[..n]);
    }
    Some((len, h.finish()))
}

/// What the check found.
pub struct Report {
    /// the redundant files that were deleted
    pub removed_files: usize,
    /// the bytes that were freed
    pub freed: u64,
    /// the videos that were taken out of the queue
    pub removed_queue: usize,
    /// the names that had more than one video
    pub groups: usize,
}

/// Looks for videos that were downloaded twice and throws the redundant ones away.
pub fn check(sh: &Arc<Shared>, stop: &AtomicBool) -> Report {
    let mut report = Report {
        removed_files: 0,
        freed: 0,
        removed_queue: 0,
        groups: 0,
    };

    // 1. the finished videos: same name and the same content -> the one with the extra
    //    " [code]" in its name is redundant
    let done: Vec<(u64, String, String, String)> = {
        let st = sh.lock();
        st.items
            .iter()
            .filter(|i| i.status == ItemStatus::Done)
            .map(|i| (i.id, i.subdir.clone(), i.stem.clone(), i.ext.clone()))
            .collect()
    };
    let mut by_name: Vec<(String, Vec<(u64, String)>)> = Vec::new();
    for (id, subdir, stem, _) in &done {
        let key = format!("{subdir}\u{1}{}", base_name(stem));
        match by_name.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push((*id, stem.clone())),
            None => by_name.push((key, vec![(*id, stem.clone())])),
        }
    }
    for (_, list) in by_name.iter() {
        if list.len() < 2 {
            continue;
        }
        report.groups += 1;
        // the one without the extra name stays, the others go
        let mut keep: Option<(u64, String)> = None;
        let mut extra: Vec<(u64, String)> = Vec::new();
        for (id, stem) in list {
            if stem.contains(" [") && stem.ends_with(']') {
                extra.push((*id, stem.clone()));
            } else if keep.is_none() {
                keep = Some((*id, stem.clone()));
            } else {
                extra.push((*id, stem.clone()));
            }
        }
        let Some((_, keep_stem)) = keep else { continue };
        for (id, stem) in extra {
            if stop.load(Ordering::Relaxed) {
                return report;
            }
            let item = { sh.lock().items.iter().find(|i| i.id == id).cloned() };
            let Some(item) = item else { continue };
            let dir = sh.item_dir(&item);
            let path = dir.join(file_name_of(&stem, &item.ext));
            let keep_path = dir.join(file_name_of(&keep_stem, &item.ext));
            // only when the contents are really the same
            let same = match (fingerprint(&path), fingerprint(&keep_path)) {
                (Some(a), Some(b)) => a == b && a.0 > 0,
                _ => false,
            };
            if !same {
                continue;
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(&path).is_ok() {
                report.removed_files += 1;
                report.freed += size;
            }
            sh.lock().items.retain(|i| i.id != id);
        }
    }

    // 2. the videos that are still in the queue: the same link twice is a duplicate
    let queued: Vec<(u64, String, Option<String>)> = {
        let st = sh.lock();
        st.items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Queued | ItemStatus::Ready))
            .map(|i| (i.id, i.stem.clone(), i.direct_url.clone()))
            .collect()
    };
    let mut seen: Vec<(String, u64)> = Vec::new();
    for (id, stem, direct) in queued {
        if stop.load(Ordering::Relaxed) {
            return report;
        }
        // the same name and the same file behind it
        let key = format!(
            "{}\u{1}{}",
            base_name(&stem),
            direct.as_deref().unwrap_or("")
        );
        if seen.iter().any(|(k, _)| *k == key) {
            sh.lock().items.retain(|i| i.id != id);
            report.removed_queue += 1;
        } else {
            seen.push((key, id));
        }
    }

    sh.touch();
    report
}
