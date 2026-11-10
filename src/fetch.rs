//! Phase 1: turn pasted links into saved queue entries (no video data is downloaded here).

use crate::model::*;
use crate::resolver;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;

/// Starts up to 3 background workers that process every link still marked New.
pub fn spawn_fetchers(shared: &Arc<Shared>) {
    let n = shared
        .lock()
        .links
        .iter()
        .filter(|l| l.status == LinkStatus::New)
        .count()
        .min(3);
    for _ in 0..n {
        let s = shared.clone();
        std::thread::spawn(move || fetch_loop(s));
    }
}

fn fetch_loop(sh: Arc<Shared>) {
    loop {
        let job = {
            let mut st = sh.lock();
            let group = st.settings.group_folders;
            st.links
                .iter_mut()
                .find(|l| l.status == LinkStatus::New)
                .map(|l| {
                    l.status = LinkStatus::Fetching;
                    (l.id, l.url.clone(), l.kind, l.code.clone(), group)
                })
        };
        let Some((id, url, kind, code, group)) = job else {
            break;
        };
        sh.touch();

        let res = catch_unwind(AssertUnwindSafe(|| process(&sh, &url, kind, &code, group)))
            .unwrap_or_else(|_| Err("internal error".to_string()));

        {
            let mut st = sh.lock();
            if let Some(l) = st.links.iter_mut().find(|l| l.id == id) {
                match res {
                    Ok(note) => {
                        l.status = LinkStatus::Done;
                        l.note = note;
                    }
                    Err(e) => l.status = LinkStatus::Error(e),
                }
            }
        }
        sh.touch();
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn process(
    sh: &Shared,
    url: &str,
    kind: LinkKind,
    code: &str,
    group: bool,
) -> Result<String, String> {
    match kind {
        LinkKind::Single => {
            let m = resolver::resolve_media(&sh.agent, url, &sh.paths.debug_dir(), code)
                .map_err(|e| e.msg)?;
            let added = sh.lock().add_item(
                code,
                url,
                &m.title,
                "",
                Some(m.probe.url),
                m.probe.size,
                &m.probe.ext,
                Some(m.probe.referer),
                m.probe.hls,
            );
            Ok(if added {
                "1 video".into()
            } else {
                "already in the list".into()
            })
        }
        LinkKind::Folder => {
            let (files, warnings) = resolver::crawl_folder(&sh.agent, url, group)?;
            if files.is_empty() {
                return Err("no videos found in this folder".into());
            }
            let mut st = sh.lock();
            let mut n = 0;
            for f in files {
                if st.add_item(
                    &f.code,
                    &f.page_url,
                    &f.title,
                    &f.subdir,
                    None,
                    None,
                    "mp4",
                    None,
                    false,
                ) {
                    n += 1;
                }
            }
            let mut note = format!("{n} new video(s)");
            if !warnings.is_empty() {
                note.push_str(&format!(
                    ", {} page(s) couldn't be read: use Retry to fill the gaps",
                    warnings.len()
                ));
            }
            Ok(note)
        }
    }
}
