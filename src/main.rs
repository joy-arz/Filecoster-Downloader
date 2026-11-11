#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod convert;
mod downloader;
mod dupes;
mod fetch;
mod hls;
mod model;
mod resolver;
#[cfg(test)]
mod tests;

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use fs2::FileExt;
use model::*;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(0xfe, 0x60, 0x81);
const GREEN: Color32 = Color32::from_rgb(0x5c, 0xc8, 0x8a);
const RED: Color32 = Color32::from_rgb(0xe5, 0x5b, 0x5b);

struct Active {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

/// flips `done` even if the worker thread panics
struct DoneGuard(Arc<AtomicBool>);
impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Ready,
    Active,
    Done,
    Failed,
}

struct Row {
    id: u64,
    title: String,
    folder: String,
    status: ItemStatus,
    downloaded: u64,
    size: Option<u64>,
    speed: f64,
    error: String,
    hls: bool,
    hls_done: u32,
    hls_total: u32,
    converting: bool,
}

#[derive(Default)]
struct Counts {
    total: usize,
    ready: usize,
    active: usize,
    done: usize,
    failed: usize,
}

struct Snap {
    rows: Vec<Row>,
    links: Vec<Link>,
    counts: Counts,
    speed: f64,
    remaining: u64,
    settings: Settings,
    fetching: usize,
    new_links: usize,
    unconverted: usize,
    converting: usize,
}

enum Act {
    Queue(u64),
    Pause(u64),
    Remove(u64),
    RetryLink(u64),
    RemoveLink(u64),
}

enum Bulk {
    DownloadAll,
    PauseAll,
    RetryFailed,
    ClearDone,
}

struct App {
    shared: Arc<Shared>,
    active: HashMap<u64, Active>,
    /// the batch that turns the finished .ts files into .mp4
    convert: Option<Active>,
    running: bool,
    input: String,
    filter: Filter,
    show_settings: bool,
    notice: Option<String>,
    /// ffmpeg is missing: offer to install it
    ask_ffmpeg: bool,
    last_save: Instant,
    _lock: Option<File>,
}

fn main() -> eframe::Result<()> {
    let paths = Paths::detect();

    // one copy of the app per drive at a time (two copies would fight over the same queue)
    let lock = match acquire_lock(&paths) {
        Ok(l) => l,
        Err(()) => {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("Filecoster Downloader")
                .set_description(
                    "Another copy of the downloader is already using this drive's queue.",
                )
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            return Ok(());
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Filecoster Downloader")
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([680.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Filecoster Downloader",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, paths, lock)))),
    )
}

fn acquire_lock(paths: &Paths) -> Result<Option<File>, ()> {
    let f = match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(paths.lock_file())
    {
        Ok(f) => f,
        Err(_) => return Ok(None), // can't lock (read-only drive?): carry on without
    };
    match f.try_lock_exclusive() {
        Ok(()) => Ok(Some(f)),
        Err(_) => Err(()),
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, paths: Paths, lock: Option<File>) -> Self {
        let mut v = egui::Visuals::dark();
        v.selection.bg_fill = ACCENT;
        v.hyperlink_color = ACCENT;
        cc.egui_ctx.set_visuals(v);
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.button_padding = egui::vec2(10.0, 4.0);
        });

        let state = State::load(&paths);
        let (base, notice) = resolve_base(&paths, &state.settings);
        let agent = resolver::agent();
        let has_queued = state.items.iter().any(|i| i.status == ItemStatus::Queued);
        let running = state.settings.auto_resume && has_queued;
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            dirty: AtomicBool::new(false),
            paths,
            base: Mutex::new(base),
            agent,
            ctx: cc.egui_ctx.clone(),
            conversions: AtomicUsize::new(0),
            batch_running: AtomicBool::new(false),
        });
        shared.refresh_progress();
        // the first time (and after every update) we look whether ffmpeg is there
        let ask_ffmpeg = {
            let mut st = shared.lock();
            let first = st.settings.ffmpeg_checked != env!("CARGO_PKG_VERSION");
            st.settings.ffmpeg_checked = env!("CARGO_PKG_VERSION").to_string();
            let want = st.settings.convert_hls;
            (first, want)
        };
        let ask_ffmpeg =
            ask_ffmpeg.0 && ask_ffmpeg.1 && crate::convert::find_ffmpeg(&shared).is_none();
        fetch::spawn_fetchers(&shared); // finish any links that were still waiting

        App {
            shared,
            active: HashMap::new(),
            convert: None,
            running,
            input: String::new(),
            filter: Filter::All,
            show_settings: false,
            notice,
            ask_ffmpeg,
            last_save: Instant::now(),
            _lock: lock,
        }
    }

    fn schedule(&mut self) {
        self.active.retain(|_, a| !a.done.load(Ordering::SeqCst));
        if !self.running {
            return;
        }
        let shared = self.shared.clone();
        let mut st = shared.lock();
        let mut free = st
            .settings
            .max_active
            .max(1)
            .saturating_sub(self.active.len());
        for it in st.items.iter_mut() {
            if free == 0 {
                break;
            }
            if it.status == ItemStatus::Queued && !self.active.contains_key(&it.id) {
                it.status = ItemStatus::Downloading;
                it.error.clear();
                it.speed = 0.0;
                let stop = Arc::new(AtomicBool::new(false));
                let done = Arc::new(AtomicBool::new(false));
                self.active.insert(
                    it.id,
                    Active {
                        stop: stop.clone(),
                        done: done.clone(),
                    },
                );
                let (s2, id) = (shared.clone(), it.id);
                std::thread::spawn(move || {
                    let _guard = DoneGuard(done);
                    downloader::run(s2, id, stop);
                });
                free -= 1;
            }
        }
        // nothing left to run -> idle again (the folder can be changed again)
        let busy = st
            .items
            .iter()
            .any(|i| matches!(i.status, ItemStatus::Queued | ItemStatus::Downloading));
        drop(st);
        if !busy {
            self.running = false;
        }
    }

    fn maybe_save(&mut self) {
        if self.shared.dirty.load(Ordering::Relaxed)
            && self.last_save.elapsed() > Duration::from_millis(1500)
        {
            self.shared.dirty.store(false, Ordering::Relaxed);
            self.shared.save();
            self.last_save = Instant::now();
        }
    }

    fn snapshot(&self) -> Snap {
        let st = self.shared.lock();
        let mut counts = Counts::default();
        let (mut speed, mut remaining) = (0.0, 0u64);
        let mut rows = Vec::new();
        for it in &st.items {
            counts.total += 1;
            match it.status {
                ItemStatus::Ready => counts.ready += 1,
                ItemStatus::Queued | ItemStatus::Downloading | ItemStatus::Paused => {
                    counts.active += 1
                }
                ItemStatus::Done => counts.done += 1,
                ItemStatus::Failed => counts.failed += 1,
            }
            if it.status == ItemStatus::Downloading {
                speed += it.speed;
            }
            if it.status != ItemStatus::Done {
                if let Some(s) = it.size {
                    remaining += s.saturating_sub(it.downloaded);
                }
            }
            let show = match self.filter {
                Filter::All => true,
                Filter::Ready => it.status == ItemStatus::Ready,
                Filter::Active => matches!(
                    it.status,
                    ItemStatus::Queued | ItemStatus::Downloading | ItemStatus::Paused
                ),
                Filter::Done => it.status == ItemStatus::Done,
                Filter::Failed => it.status == ItemStatus::Failed,
            };
            if show {
                rows.push(Row {
                    id: it.id,
                    title: it.stem.clone(),
                    folder: it.subdir.clone(),
                    status: it.status,
                    downloaded: it.downloaded,
                    size: it.size,
                    speed: it.speed,
                    error: it.error.clone(),
                    hls: it.hls,
                    hls_done: it.hls_done,
                    hls_total: it.hls_total,
                    converting: it.converting,
                });
            }
        }
        Snap {
            rows,
            links: st.links.clone(),
            counts,
            speed,
            remaining,
            settings: st.settings.clone(),
            fetching: st
                .links
                .iter()
                .filter(|l| l.status == LinkStatus::Fetching)
                .count(),
            new_links: st
                .links
                .iter()
                .filter(|l| l.status == LinkStatus::New)
                .count(),
            unconverted: st
                .items
                .iter()
                .filter(|i| i.status == ItemStatus::Done && !i.converted)
                .count(),
            converting: st.items.iter().filter(|i| i.converting).count(),
        }
    }

    /// Adds the links of a text. Returns how many were added and how many were skipped
    /// (we already have them).
    fn add_links(&mut self, text: &str) -> (usize, usize) {
        let mut added = 0;
        let mut skipped = 0;
        {
            let mut st = self.shared.lock();
            for p in resolver::parse_links(text) {
                if st.add_link(p.kind, &p.code, &p.url) {
                    added += 1;
                } else {
                    // already in the list, or we already have the video
                    skipped += 1;
                }
            }
        }
        self.shared.touch();
        fetch::spawn_fetchers(&self.shared);
        (added, skipped)
    }

    fn pick_dest(&mut self) {
        let start = self.shared.base_dir();
        if let Some(p) = rfd::FileDialog::new().set_directory(start).pick_folder() {
            let stored = to_stored(&self.shared.paths, &p);
            self.shared.lock().settings.base = Some(stored);
            *self.shared.base.lock().unwrap_or_else(|e| e.into_inner()) = p;
            self.shared.refresh_progress();
            self.shared.touch();
            self.notice = None;
        }
    }

    fn bulk(&mut self, b: Bulk) {
        match b {
            Bulk::DownloadAll => {
                let mut st = self.shared.lock();
                for it in st.items.iter_mut() {
                    if matches!(it.status, ItemStatus::Ready | ItemStatus::Paused) {
                        it.status = ItemStatus::Queued;
                        it.error.clear();
                    }
                }
                drop(st);
                self.running = true;
            }
            Bulk::PauseAll => {
                for a in self.active.values() {
                    a.stop.store(true, Ordering::SeqCst);
                }
                let mut st = self.shared.lock();
                for it in st.items.iter_mut() {
                    if matches!(it.status, ItemStatus::Queued | ItemStatus::Downloading) {
                        it.status = ItemStatus::Paused;
                        it.speed = 0.0;
                    }
                }
                drop(st);
                self.running = false;
            }
            Bulk::RetryFailed => {
                let mut st = self.shared.lock();
                for it in st.items.iter_mut() {
                    if it.status == ItemStatus::Failed {
                        it.status = ItemStatus::Queued;
                        it.error.clear();
                    }
                }
                drop(st);
                self.running = true;
            }
            Bulk::ClearDone => {
                self.shared
                    .lock()
                    .items
                    .retain(|i| i.status != ItemStatus::Done);
            }
        }
        self.shared.touch();
    }

    /// Installs ffmpeg (it is needed to compress the streams).
    fn install_ffmpeg(&mut self) {
        let sh = self.shared.clone();
        std::thread::spawn(move || {
            let msg = match crate::convert::install() {
                Ok(m) => match crate::convert::find_ffmpeg(&sh) {
                    Some(p) => format!("ffmpeg is ready: {p} ({m})"),
                    None => format!("{m}, but ffmpeg was not found afterwards"),
                },
                Err(e) => format!("ffmpeg could not be installed: {e}"),
            };
            sh.lock().notice = Some(msg);
            sh.touch();
        });
    }

    /// Looks for videos that were downloaded twice and throws the redundant ones away.
    fn check_duplicates(&mut self) {
        if self.convert.is_some() {
            self.notice = Some("Wait until the conversion is finished.".into());
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        self.convert = Some(Active {
            stop: stop.clone(),
            done: done.clone(),
        });
        let sh = self.shared.clone();
        self.shared.touch();
        std::thread::spawn(move || {
            let _guard = DoneGuard(done);
            let report = crate::dupes::check(&sh, &stop);
            let mut msg = format!(
                "{} duplicate file(s) removed ({} freed), {} video(s) taken out of the queue.",
                report.removed_files,
                fmt_bytes(report.freed),
                report.removed_queue
            );
            if report.groups > 0 {
                msg.push_str(&format!(
                    " {} name(s) had more than one video.",
                    report.groups
                ));
            }
            sh.lock().notice = Some(msg);
            sh.touch();
        });
    }

    /// Turns every finished video that was not compressed yet into a small .mp4.
    fn convert_all(&mut self) {
        if self.convert.is_some() {
            return;
        }
        let ffmpeg = match crate::convert::find_ffmpeg(&self.shared) {
            Some(p) => p,
            None => {
                self.notice = Some(
                    "ffmpeg was not found. Install it (macOS: brew install ffmpeg) or put the \
                     program next to the app, then try again."
                        .into(),
                );
                return;
            }
        };
        let ids = crate::convert::unconverted(&self.shared);
        if ids.is_empty() {
            self.notice = Some("Every finished video is already compressed.".into());
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        self.convert = Some(Active {
            stop: stop.clone(),
            done: done.clone(),
        });
        let sh = self.shared.clone();
        self.shared.touch();
        std::thread::spawn(move || {
            let _guard = DoneGuard(done);
            crate::convert::batch(sh, ids, &ffmpeg, &stop);
        });
    }

    fn apply(&mut self, act: Act) {
        let shared = self.shared.clone();
        match act {
            Act::Queue(id) => {
                let mut st = shared.lock();
                if let Some(it) = st.items.iter_mut().find(|i| i.id == id) {
                    if matches!(
                        it.status,
                        ItemStatus::Ready | ItemStatus::Paused | ItemStatus::Failed
                    ) {
                        it.status = ItemStatus::Queued;
                        it.error.clear();
                    }
                }
                drop(st);
                self.running = true;
            }
            Act::Pause(id) => {
                if let Some(a) = self.active.get(&id) {
                    a.stop.store(true, Ordering::SeqCst);
                }
                let mut st = shared.lock();
                if let Some(it) = st.items.iter_mut().find(|i| i.id == id) {
                    if matches!(it.status, ItemStatus::Queued | ItemStatus::Downloading) {
                        it.status = ItemStatus::Paused;
                        it.speed = 0.0;
                    }
                }
            }
            Act::Remove(id) => {
                let running_now = self
                    .active
                    .get(&id)
                    .map(|a| a.stop.store(true, Ordering::SeqCst))
                    .is_some();
                let mut st = shared.lock();
                if let Some(pos) = st.items.iter().position(|i| i.id == id) {
                    let it = st.items.remove(pos);
                    if !running_now && it.status != ItemStatus::Done {
                        let _ = fs::remove_file(shared.item_dir(&it).join(it.part_name()));
                    }
                }
            }
            Act::RetryLink(id) => {
                let mut st = shared.lock();
                if let Some(l) = st.links.iter_mut().find(|l| l.id == id) {
                    l.status = LinkStatus::New;
                }
                drop(st);
                fetch::spawn_fetchers(&shared);
            }
            Act::RemoveLink(id) => {
                shared.lock().links.retain(|l| l.id != id);
            }
        }
        shared.touch();
    }
}

fn fmt_speed(bps: f64) -> String {
    format!("{}/s", fmt_bytes(bps as u64))
}

/// "1h 04m", "4m 12s", "9s"
fn fmt_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.schedule();
        self.maybe_save();
        let snap = self.snapshot();
        let shared = self.shared.clone();

        let mut acts: Vec<Act> = Vec::new();
        let mut bulk: Option<Bulk> = None;
        let mut add_text: Option<String> = None;
        let mut fetch_pending = false;
        let mut pick_dest = false;
        let mut import_txt = false;
        let mut reference_txt = false;
        let mut export_links = false;
        let mut forget_used = false;
        let mut convert_all = false;
        let mut stop_convert = false;
        let mut check_dupes = false;
        let mut install_ffmpeg = false;
        let mut paste_clipboard = false;
        let mut open_dir: Option<String> = None;
        let mut settings_edit: Option<Settings> = None;

        // ───── top: destination, add links, controls ─────
        let mut paste_box: Option<egui::Response> = None;
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Filecoster Downloader").size(20.0).strong().color(ACCENT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.show_settings, "Settings").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                });
            });

            if let Some(n) = self.notice.clone() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(n).color(Color32::from_rgb(0xe8, 0xb8, 0x4a)));
                    if ui.small_button("Dismiss").clicked() {
                        self.notice = None;
                    }
                });
            }
            // ffmpeg is needed to compress the streams: offer to install it
            if self.ask_ffmpeg {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(
                            "ffmpeg is needed to compress the downloaded streams into small mp4 files.",
                        )
                        .color(Color32::from_rgb(0xe8, 0xb8, 0x4a)),
                    );
                    if ui.button("Install ffmpeg").clicked() {
                        install_ffmpeg = true;
                        self.ask_ffmpeg = false;
                    }
                    if ui.button("Not now").clicked() {
                        self.ask_ffmpeg = false;
                    }
                });
            }
            // a message from a background job
            let job_notice = self.shared.lock().notice.clone();
            if let Some(n) = job_notice {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(n).color(Color32::from_rgb(0xe8, 0xb8, 0x4a)));
                    if ui.small_button("Dismiss").clicked() {
                        self.shared.lock().notice = None;
                        self.shared.touch();
                    }
                });
            }

            // destination
            let dest = shared.dest_root();
            ui.horizontal(|ui| {
                ui.label("Save to");
                let busy = self.running || !self.active.is_empty();
                if ui
                    .add_enabled(!busy, egui::Button::new("Change..."))
                    .on_disabled_hover_text("Pause the running downloads first")
                    .clicked()
                {
                    pick_dest = true;
                }
                if ui.button("Open").clicked() {
                    let target = if dest.exists() { dest.clone() } else { shared.base_dir() };
                    let _ = opener::open(target);
                }
                let mut shown = dest.display().to_string();
                if !dest.exists() {
                    shown.push_str("   (will be created)");
                }
                ui.add(egui::Label::new(RichText::new(shown).monospace().weak()).truncate());
            });

            if self.show_settings {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    let mut s = snap.settings.clone();
                    let mut changed = false;
                    changed |= ui
                        .add(egui::Slider::new(&mut s.max_active, 1..=8).text("Downloads at the same time"))
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut s.retries, 0..=20).text("Automatic retries per video"))
                        .changed();
                    changed |= ui
                        .checkbox(&mut s.auto_resume, "Continue unfinished downloads when the app opens")
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut s.group_folders,
                            "Put videos from a folder link into their own sub-folder (for newly fetched links)",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut s.convert_hls,
                            "Compress the finished streams into .mp4 (ffmpeg, in the background)",
                        )
                        .changed();
                    if !s.convert_hls {
                        ui.label(
                            RichText::new(
                                "Off: the streams are saved as .ts files. They are bigger, but the \
                                 download is done much faster. You can convert them later with \
                                 \"Convert unconverted\".",
                            )
                            .small()
                            .weak(),
                        );
                    }
                    if s.convert_hls {
                        let encs: Vec<(&str, u8)> = vec![
                            (crate::convert::Encoder::H264Software.name(), 0),
                            (crate::convert::Encoder::H265Software.name(), 1),
                            (crate::convert::Encoder::H264Hardware.name(), 2),
                            (crate::convert::Encoder::H265Hardware.name(), 3),
                        ];
                        let mut picked = encs
                            .iter()
                            .find(|(_, v)| *v == s.video_codec)
                            .map(|(n, _)| *n)
                            .unwrap_or(encs[0].0);
                        if ui
                            .selectable_value(&mut picked, encs[0].0, encs[0].0)
                            .clicked()
                        {
                            s.video_codec = 0;
                            changed = true;
                        }
                        for (name, v) in &encs[1..] {
                            let label = if cfg!(target_os = "macos") {
                                (*name).to_string()
                            } else {
                                format!("{name} (macOS only)")
                            };
                            if ui.selectable_value(&mut picked, *name, label).clicked() {
                                s.video_codec = *v;
                                changed = true;
                            }
                        }
                        changed |= ui
                            .add(egui::Slider::new(&mut s.quality, 1..=100).text("Quality (higher is better)"))
                            .changed();
                        if cfg!(target_os = "macos")
                            && (s.video_codec == 1 || s.video_codec == 0)
                        {
                            ui.label(
                                RichText::new(
                                    "The graphics card is about 3 times faster and leaves the \
                                     processor free for the downloads.",
                                )
                                .small()
                                .weak(),
                            );
                        }
                        let ff = crate::convert::find_ffmpeg(&self.shared);
                        match &ff {
                            Some(p) => {
                                ui.label(
                                    RichText::new(format!("ffmpeg: {p}")).small().weak(),
                                );
                            }
                            None => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        RichText::new("ffmpeg was not found")
                                            .small()
                                            .color(Color32::from_rgb(0xe8, 0xb8, 0x4a)),
                                    );
                                    if ui.button("Install ffmpeg").clicked() {
                                        install_ffmpeg = true;
                                    }
                                    match crate::convert::installer() {
                                        Some(crate::convert::Installer::Command(p, a)) => {
                                            ui.label(
                                                RichText::new(format!("({p} {})", a.join(" ")))
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                        Some(crate::convert::Installer::Manual(how)) => {
                                            ui.label(RichText::new(how).small().weak());
                                        }
                                        None => {}
                                    }
                                });
                            }
                        }
                        if !s.ffmpeg.is_empty() {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut s.ffmpeg).hint_text("path to ffmpeg"))
                                .changed();
                        }
                    }
                    if changed {
                        settings_edit = Some(s);
                    }
                    ui.add_space(2.0);
                    let used = shared.lock().used.len();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "{used} video(s) are on the reference list and are never downloaded again."
                        ));
                        if ui.button("Export my links...").clicked() {
                            export_links = true;
                        }
                        if used > 0 && ui.button("Forget the reference list").clicked() {
                            forget_used = true;
                        }
                    });
                    if !shared.paths.portable {
                        ui.label(
                            RichText::new(
                                "The app's folder is read-only, so the queue is stored on this computer instead.",
                            )
                            .small()
                            .weak(),
                        );
                    }
                    ui.label(
                        RichText::new(format!(
                            "Everything that fails is written to {}",
                            shared.paths.fail_log().display()
                        ))
                        .small()
                        .weak(),
                    );
                    let where_ = shared.paths.data_dir.display().to_string();
                    ui.label(
                        RichText::new(format!(
                            "Filecoster Downloader {}\n{}",
                            env!("CARGO_PKG_VERSION"),
                            where_
                        ))
                        .small()
                        .weak(),
                    );
                });
            }

            // add links
            ui.add_space(2.0);
            paste_box = Some(ui.add(
                egui::TextEdit::multiline(&mut self.input)
                    .hint_text("Paste filecoster links here, as many as you like.")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            ));
            ui.weak(
                "Any format works: one link per line, separated by spaces, or dropped in the middle of a sentence.  /d/, /s/ and /e/ = one video, /f/ = a folder of videos.",
            );
            let parsed = resolver::parse_links(&self.input);
            let folders = parsed.iter().filter(|p| p.kind == model::LinkKind::Folder).count();
            ui.horizontal_wrapped(|ui| {
                let label = if parsed.is_empty() {
                    "Add & fetch".to_string()
                } else {
                    format!(
                        "Add {} link(s) & fetch  ({} folder(s), {} video(s))",
                        parsed.len(),
                        folders,
                        parsed.len() - folders
                    )
                };
                let add = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(ACCENT);
                if ui.add_enabled(!parsed.is_empty(), add).clicked() {
                    add_text = Some(std::mem::take(&mut self.input));
                }
                if ui.button("Paste from clipboard").clicked() {
                    paste_clipboard = true;
                }
                if ui.button("Clear").clicked() {
                    self.input.clear();
                }
                if ui.button("Import .txt...").clicked() {
                    import_txt = true;
                }
                if ui.button("Reference .txt...").clicked() {
                    reference_txt = true;
                }
                if snap.new_links > 0 && ui.button(format!("Fetch pending ({})", snap.new_links)).clicked() {
                    fetch_pending = true;
                }
            });

            // queue controls + filter
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                let c = &snap.counts;
                let go = egui::Button::new(RichText::new("Download all").color(Color32::WHITE)).fill(ACCENT);
                if ui.add_enabled(c.ready + c.active > 0, go).clicked() {
                    bulk = Some(Bulk::DownloadAll);
                }
                if ui.add_enabled(!self.active.is_empty() || self.running, egui::Button::new("Pause all")).clicked() {
                    bulk = Some(Bulk::PauseAll);
                }
                if ui.add_enabled(c.failed > 0, egui::Button::new("Retry failed")).clicked() {
                    bulk = Some(Bulk::RetryFailed);
                }
                if ui.add_enabled(c.done > 0, egui::Button::new("Clear finished")).clicked() {
                    bulk = Some(Bulk::ClearDone);
                }
                if snap.converting > 0 {
                    if ui.button("Stop converting").clicked() {
                        stop_convert = true;
                    }
                } else if snap.unconverted > 0
                    && ui
                        .button(format!("Convert unconverted ({})", snap.unconverted))
                        .clicked()
                {
                    convert_all = true;
                }
                if ui.button("Check duplicates").clicked() {
                    check_dupes = true;
                }
                ui.separator();
                for (f, name, n) in [
                    (Filter::All, "All", c.total),
                    (Filter::Ready, "Ready", c.ready),
                    (Filter::Active, "In queue", c.active),
                    (Filter::Done, "Done", c.done),
                    (Filter::Failed, "Failed", c.failed),
                ] {
                    if ui.selectable_label(self.filter == f, format!("{name} ({n})")).clicked() {
                        self.filter = f;
                    }
                }
            });
            ui.add_space(4.0);
        });

        // ───── bottom: summary ─────
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if snap.fetching > 0 {
                    ui.spinner();
                    ui.label(format!("Fetching {} link(s)...", snap.fetching));
                    ui.separator();
                }
                let c = &snap.counts;
                let eta = if snap.speed > 1024.0 && snap.remaining > 0 {
                    format!(
                        " · about {} left",
                        fmt_duration((snap.remaining as f64 / snap.speed) as u64)
                    )
                } else {
                    String::new()
                };
                ui.label(format!(
                    "{} videos · {} done · {} left to download{eta}",
                    c.total,
                    c.done,
                    fmt_bytes(snap.remaining)
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.active.is_empty() {
                        ui.label(RichText::new(fmt_speed(snap.speed)).strong().color(ACCENT));
                    } else if self.running {
                        ui.weak("waiting...");
                    } else {
                        ui.weak("idle");
                    }
                });
            });
        });

        // ───── centre: links + videos ─────
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::CollapsingHeader::new(format!("Links ({})", snap.links.len()))
                .default_open(false)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .id_salt("links")
                        .show(ui, |ui| {
                            for l in &snap.links {
                                ui.horizontal(|ui| {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("Remove").clicked() {
                                                acts.push(Act::RemoveLink(l.id));
                                            }
                                            if matches!(l.status, LinkStatus::Error(_))
                                                && ui.small_button("Retry").clicked()
                                            {
                                                acts.push(Act::RetryLink(l.id));
                                            }
                                            let (txt, col) = match &l.status {
                                                LinkStatus::New => {
                                                    ("waiting".to_string(), Color32::GRAY)
                                                }
                                                LinkStatus::Fetching => {
                                                    ("fetching...".to_string(), ACCENT)
                                                }
                                                LinkStatus::Done => (l.note.clone(), GREEN),
                                                LinkStatus::Error(e) => {
                                                    (format!("error: {e}"), RED)
                                                }
                                            };
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(txt).color(col).small(),
                                                )
                                                .truncate(),
                                            );
                                            ui.with_layout(
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(match l.kind {
                                                            model::LinkKind::Single => "video ",
                                                            model::LinkKind::Folder => "folder",
                                                        })
                                                        .small()
                                                        .weak(),
                                                    );
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&l.url)
                                                                .monospace()
                                                                .small(),
                                                        )
                                                        .truncate(),
                                                    );
                                                },
                                            );
                                        },
                                    );
                                });
                            }
                        });
                });
            ui.add_space(4.0);

            if snap.rows.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.weak(if snap.counts.total == 0 {
                        "Nothing here yet. Paste some links above and press “Add & fetch”."
                    } else {
                        "No videos in this view."
                    });
                });
                return;
            }

            TableBuilder::new(ui)
                .striped(true)
                .resizable(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::exact(200.0))
                .column(Column::exact(170.0))
                .column(Column::exact(150.0))
                .header(22.0, |mut h| {
                    h.col(|ui| {
                        ui.strong("Video");
                    });
                    h.col(|ui| {
                        ui.strong("Progress");
                    });
                    h.col(|ui| {
                        ui.strong("Status");
                    });
                    h.col(|ui| {
                        ui.strong("");
                    });
                })
                .body(|body| {
                    body.rows(34.0, snap.rows.len(), |mut row| {
                        let r = &snap.rows[row.index()];
                        row.col(|ui| {
                            ui.vertical(|ui| {
                                let resp = ui.add(egui::Label::new(&r.title).truncate());
                                resp.on_hover_text(&r.title);
                                if !r.folder.is_empty() {
                                    let resp = ui.add(
                                        egui::Label::new(RichText::new(&r.folder).small().weak())
                                            .truncate(),
                                    );
                                    if resp.clicked() {
                                        open_dir = Some(r.folder.clone());
                                    }
                                    resp.on_hover_text("Click to open this folder");
                                }
                            });
                        });
                        row.col(|ui| match r.status {
                            ItemStatus::Ready => {
                                ui.weak(
                                    r.size
                                        .map(fmt_bytes)
                                        .unwrap_or_else(|| "size unknown".into()),
                                );
                            }
                            _ => {
                                let frac = match (r.status, r.size) {
                                    (ItemStatus::Done, _) => 1.0,
                                    (_, Some(s)) if s > 0 => {
                                        (r.downloaded as f64 / s as f64) as f32
                                    }
                                    _ => 0.0,
                                };
                                let text = match (r.status, r.size) {
                                    (ItemStatus::Downloading, Some(s)) if s > 0 => {
                                        format!("{:.0}% · {}", frac * 100.0, fmt_speed(r.speed))
                                    }
                                    (ItemStatus::Downloading, _) => format!(
                                        "{} · {}",
                                        fmt_bytes(r.downloaded),
                                        fmt_speed(r.speed)
                                    ),
                                    (ItemStatus::Done, Some(s)) => fmt_bytes(s),
                                    (_, Some(s)) => {
                                        format!("{} / {}", fmt_bytes(r.downloaded), fmt_bytes(s))
                                    }
                                    (_, None) => fmt_bytes(r.downloaded),
                                };
                                // a stream is downloaded part by part: show how many are done
                                let text = if r.hls && r.hls_total > 0 {
                                    format!(
                                        "{} · {}/{} parts · {}",
                                        fmt_bytes(r.downloaded),
                                        r.hls_done,
                                        r.hls_total,
                                        fmt_speed(r.speed)
                                    )
                                } else {
                                    text
                                };
                                ui.add(
                                    egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                                        .desired_width(ui.available_width())
                                        .text(text),
                                );
                            }
                        });
                        row.col(|ui| {
                            let retrying =
                                r.status == ItemStatus::Downloading && r.error.starts_with("retry");
                            let (txt, col) = match r.status {
                                ItemStatus::Ready => ("Ready", Color32::GRAY),
                                ItemStatus::Queued => ("Queued", Color32::LIGHT_GRAY),
                                ItemStatus::Downloading if retrying => {
                                    ("Retrying...", Color32::from_rgb(0xe8, 0xb8, 0x4a))
                                }
                                ItemStatus::Downloading => ("Downloading", ACCENT),
                                ItemStatus::Paused => {
                                    ("Paused", Color32::from_rgb(0xe8, 0xb8, 0x4a))
                                }
                                ItemStatus::Done => ("Done", GREEN),
                                ItemStatus::Failed => ("Failed", RED),
                            };
                            ui.vertical(|ui| {
                                if r.converting {
                                    ui.label(
                                        RichText::new("Converting...")
                                            .color(Color32::from_rgb(0xe8, 0xb8, 0x4a)),
                                    );
                                } else {
                                    ui.label(RichText::new(txt).color(col));
                                }
                                // the reason is shown right here on the row ("retry 2/6: …")
                                if !r.error.is_empty() {
                                    let c = if retrying {
                                        Color32::from_rgb(0xe8, 0xb8, 0x4a)
                                    } else {
                                        RED
                                    };
                                    ui.add(
                                        egui::Label::new(RichText::new(&r.error).small().color(c))
                                            .truncate(),
                                    )
                                    .on_hover_text(&r.error);
                                }
                            });
                        });
                        row.col(|ui| {
                            match r.status {
                                ItemStatus::Ready => {
                                    if ui.small_button("Queue").clicked() {
                                        acts.push(Act::Queue(r.id));
                                    }
                                }
                                ItemStatus::Queued | ItemStatus::Downloading => {
                                    if ui.small_button("Pause").clicked() {
                                        acts.push(Act::Pause(r.id));
                                    }
                                }
                                ItemStatus::Paused => {
                                    if ui.small_button("Resume").clicked() {
                                        acts.push(Act::Queue(r.id));
                                    }
                                }
                                ItemStatus::Failed => {
                                    if ui.small_button("Retry").clicked() {
                                        acts.push(Act::Queue(r.id));
                                    }
                                }
                                ItemStatus::Done => {}
                            }
                            if ui.small_button("Remove").clicked() {
                                acts.push(Act::Remove(r.id));
                            }
                        });
                    });
                });
        });

        // ───── apply what the user did this frame ─────
        if let Some(s) = settings_edit {
            // the whole settings are written back, so that new options are never forgotten
            let mut st = shared.lock();
            st.apply_settings(s);
            drop(st);
            shared.touch();
        }
        if pick_dest {
            self.pick_dest();
        }
        if paste_clipboard {
            // focus the box and let the OS paste into it (works on macOS, Windows and Linux)
            if let Some(r) = paste_box.as_ref() {
                r.request_focus();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        if import_txt {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Text file", &["txt", "csv", "md", "url", "list", "log"])
                .add_filter("All files", &["*"])
                .pick_file()
            {
                match fs::read_to_string(&p) {
                    Ok(text) => {
                        let (n, skipped) = self.add_links(&text);
                        let name = p
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "the file".into());
                        self.notice = Some(format!(
                            "{n} link(s) added from {name}{}.",
                            if skipped > 0 {
                                format!(", {skipped} skipped (already downloaded)")
                            } else {
                                String::new()
                            }
                        ));
                    }
                    Err(e) => {
                        self.notice = Some(format!("Could not read {}: {e}", p.display()));
                    }
                }
            }
        }
        if reference_txt {
            // the links of the videos we already have: they are never downloaded again
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Text file", &["txt", "csv", "md", "url", "list", "log"])
                .add_filter("All files", &["*"])
                .pick_file()
            {
                match fs::read_to_string(&p) {
                    Ok(text) => {
                        let parsed = resolver::parse_links(&text);
                        let videos: Vec<String> = parsed
                            .iter()
                            .filter(|p| p.kind == model::LinkKind::Single)
                            .map(|p| p.code.clone())
                            .collect();
                        let folders: Vec<String> = parsed
                            .iter()
                            .filter(|p| p.kind == model::LinkKind::Folder)
                            .map(|p| p.code.clone())
                            .collect();
                        let n = self.shared.lock().add_used(videos);
                        let nf = self.shared.lock().add_used_folders(folders.clone());
                        // a folder in the reference means "I have everything in it":
                        // look inside it so that single links of that folder are skipped too
                        if !folders.is_empty() {
                            let sh = self.shared.clone();
                            std::thread::spawn(move || {
                                let mut found = 0;
                                for code in folders {
                                    let url = format!("https://filecoster.com/f/{code}");
                                    if let Ok((files, _)) =
                                        resolver::crawl_folder(&sh.agent, &url, false)
                                    {
                                        let codes: Vec<String> =
                                            files.iter().map(|f| f.code.clone()).collect();
                                        found += codes.len();
                                        sh.lock().add_used(codes);
                                    }
                                }
                                sh.lock().notice = Some(format!(
                                    "The reference list now holds {found} video(s): the folders were \
                                     looked through, so single links of those folders are skipped too."
                                ));
                                sh.touch();
                            });
                        }
                        // the videos we already have do not have to be in the queue
                        let removed: Vec<Item> = {
                            let mut st = self.shared.lock();
                            let used = st.used.clone();
                            let mut removed = Vec::new();
                            st.items.retain(|i| {
                                if i.status != ItemStatus::Done && used.contains(&i.code) {
                                    removed.push(i.clone());
                                    false
                                } else {
                                    true
                                }
                            });
                            removed
                        };
                        for it in &removed {
                            let _ = fs::remove_file(self.shared.item_dir(it).join(it.part_name()));
                        }
                        self.shared.touch();
                        self.notice = Some(format!(
                            "{n} video(s) and {nf} folder(s) added to the reference list, {} taken out of the queue. They are skipped from now on.",
                            removed.len()
                        ));
                    }
                    Err(e) => {
                        self.notice = Some(format!("Could not read {}: {e}", p.display()));
                    }
                }
            }
        }
        if export_links {
            // a .txt with the links of everything we already downloaded: it can be uploaded
            // again later as the reference list
            let done: Vec<String> = {
                let st = self.shared.lock();
                st.items
                    .iter()
                    .filter(|i| i.status == ItemStatus::Done)
                    .map(|i| i.page_url.clone())
                    .collect()
            };
            if done.is_empty() {
                self.notice = Some("There is nothing downloaded yet to export.".into());
            } else if let Some(p) = rfd::FileDialog::new()
                .set_file_name("filecoster-links.txt")
                .add_filter("Text file", &["txt"])
                .save_file()
            {
                let text = done.join("\n");
                match fs::write(&p, text) {
                    Ok(()) => {
                        self.notice = Some(format!(
                            "{} link(s) written to {}.",
                            done.len(),
                            p.display()
                        ))
                    }
                    Err(e) => self.notice = Some(format!("Could not write {}: {e}", p.display())),
                }
            }
        }
        if convert_all {
            self.convert_all();
        }
        if check_dupes {
            self.check_duplicates();
        }
        if install_ffmpeg {
            self.install_ffmpeg();
        }
        if stop_convert {
            if let Some(c) = &self.convert {
                c.stop.store(true, Ordering::SeqCst);
            }
        }
        if forget_used {
            let n = self.shared.lock().used.len();
            self.shared.lock().used.clear();
            self.shared.touch();
            self.notice = Some(format!("{n} video(s) removed from the reference list."));
        }
        if let Some(t) = add_text {
            let (n, skipped) = self.add_links(&t);
            self.notice = Some(format!(
                "{n} link(s) added{}.",
                if skipped > 0 {
                    format!(", {skipped} skipped (already downloaded)")
                } else {
                    String::new()
                }
            ));
        }
        if let Some(d) = open_dir {
            let dir = shared.subdir_dir(&d);
            let dir = if dir.is_dir() {
                dir
            } else {
                shared.dest_root()
            };
            let _ = opener::open(dir);
        }
        if fetch_pending {
            fetch::spawn_fetchers(&shared);
        }
        if let Some(b) = bulk {
            self.bulk(b);
        }
        for a in acts {
            self.apply(a);
        }

        if !self.active.is_empty() || snap.fetching > 0 {
            ctx.request_repaint_after(Duration::from_millis(300));
        } else if shared.dirty.load(Ordering::Relaxed) {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for a in self.active.values() {
            a.stop.store(true, Ordering::SeqCst);
        }
        {
            // whatever was running is put back in the queue so it continues next launch
            let mut st = self.shared.lock();
            for it in st.items.iter_mut() {
                if it.status == ItemStatus::Downloading {
                    it.status = ItemStatus::Queued;
                }
            }
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3)
            && self.active.values().any(|a| !a.done.load(Ordering::SeqCst))
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        self.shared.save();
    }
}
