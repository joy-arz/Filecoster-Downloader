use eframe::egui;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Name of the folder videos are saved into (found on the drive, or created).
pub const APP_FOLDER: &str = "Funny Vid Filecoaster";
/// Queue + settings live in this folder next to the app, so they travel with the drive.
pub const DATA_DIR: &str = "filecoster-data";
/// The video extensions we know about.
pub const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mkv", "mov", "m4v", "avi", "flv", "ts"];

// ───────────────────────── links & items ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum LinkKind {
    /// /d/<code> or /s/<code>: one video
    Single,
    /// /f/<code>: a folder of videos
    Folder,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum LinkStatus {
    New,
    Fetching,
    Done,
    Error(String),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Link {
    pub id: u64,
    pub url: String,
    pub kind: LinkKind,
    pub code: String,
    pub status: LinkStatus,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ItemStatus {
    /// fetched and saved, waiting for you to press Download
    Ready,
    Queued,
    Downloading,
    Paused,
    Done,
    Failed,
}

fn default_ext() -> String {
    "mp4".into()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub code: String,
    pub page_url: String,
    /// file name without extension
    pub stem: String,
    /// sub-folder inside the download folder ("" or "Folder name/Sub")
    #[serde(default)]
    pub subdir: String,
    #[serde(default = "default_ext")]
    pub ext: String,
    /// direct media URL if already resolved (re-resolved automatically when it expires)
    #[serde(default)]
    pub direct_url: Option<String>,
    /// page the direct link was found on (sent as Referer when downloading)
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub status: ItemStatus,
    #[serde(default)]
    pub error: String,
    /// the link is an HLS playlist: the parts are downloaded and joined
    #[serde(default)]
    pub hls: bool,
    /// how many parts of the stream are already in the .part file
    #[serde(default)]
    pub hls_done: u32,
    /// how many parts the stream has in total
    #[serde(default)]
    pub hls_total: u32,
    /// bytes of the finished parts (the .part file is cut back to this when we continue)
    #[serde(default)]
    pub hls_bytes: u64,
    #[serde(skip)]
    pub downloaded: u64,
    #[serde(skip)]
    pub speed: f64,
}

impl Item {
    /// "video.mp4" – but not "video.mp4.mp4" when the title already ends with the extension
    pub fn file_name(&self) -> String {
        self.file_name_with(&self.ext)
    }

    pub fn file_name_with(&self, ext: &str) -> String {
        let stem = self.with_right_ext(ext);
        if has_ext(&stem, ext) {
            stem
        } else {
            format!("{stem}.{ext}")
        }
    }

    /// the file that is being downloaded
    pub fn part_name(&self) -> String {
        self.part_name_with(&self.ext)
    }

    pub fn part_name_with(&self, ext: &str) -> String {
        let stem = self.with_right_ext(ext);
        if has_ext(&stem, ext) {
            format!("{stem}.part")
        } else {
            format!("{stem}.{ext}.part")
        }
    }

    /// A title that ends in another video extension ("video.mp4") is saved with the extension
    /// the file really has ("video.ts").
    fn with_right_ext(&self, ext: &str) -> String {
        let lower = self.stem.to_ascii_lowercase();
        let want = ext.to_ascii_lowercase();
        for e in VIDEO_EXTS {
            if *e == want {
                continue;
            }
            if lower.ends_with(&format!(".{e}")) {
                return self.stem[..self.stem.len() - e.len() - 1].to_string();
            }
        }
        self.stem.clone()
    }
}

/// Does the name already end with this extension?
fn has_ext(stem: &str, ext: &str) -> bool {
    let stem = stem.to_ascii_lowercase();
    let ext = ext.to_ascii_lowercase();
    !ext.is_empty() && stem.len() > ext.len() + 1 && stem.ends_with(&format!(".{ext}"))
}

// ───────────────────────── settings & state ─────────────────────────

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredPath {
    /// true = relative to the drive the app lives on (survives a different drive letter / mount point)
    pub relative: bool,
    pub path: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub base: Option<StoredPath>,
    pub max_active: usize,
    pub retries: u32,
    pub auto_resume: bool,
    pub group_folders: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            base: None,
            max_active: 3,
            retries: 6,
            auto_resume: true,
            group_folders: true,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub next_id: u64,
    pub links: Vec<Link>,
    pub items: Vec<Item>,
    pub settings: Settings,
}

impl State {
    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn load(paths: &Paths) -> State {
        let f = paths.state_file();
        let mut st: State = match fs::read(&f) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(s) => s,
                Err(_) => {
                    // keep the unreadable file around instead of silently losing the queue
                    let _ = fs::copy(&f, f.with_extension("json.broken"));
                    State::default()
                }
            },
            Err(_) => State::default(),
        };
        // anything that was mid-flight when the app closed is picked up again
        for it in &mut st.items {
            if it.status == ItemStatus::Downloading {
                it.status = ItemStatus::Queued;
            }
        }
        for l in &mut st.links {
            if l.status == LinkStatus::Fetching {
                l.status = LinkStatus::New;
            }
        }
        st.settings.max_active = st.settings.max_active.clamp(1, 8);
        st.settings.retries = st.settings.retries.min(20);
        st
    }

    /// Returns false if this link is already in the list (an errored one is re-armed instead).
    pub fn add_link(&mut self, kind: LinkKind, code: &str, url: &str) -> bool {
        if let Some(l) = self
            .links
            .iter_mut()
            .find(|l| l.kind == kind && l.code == code)
        {
            if matches!(l.status, LinkStatus::Error(_)) {
                l.status = LinkStatus::New;
                return true;
            }
            return false;
        }
        let id = self.new_id();
        self.links.push(Link {
            id,
            url: url.to_string(),
            kind,
            code: code.to_string(),
            status: LinkStatus::New,
            note: String::new(),
        });
        true
    }

    /// Returns false if a video with this code is already in the list.
    #[allow(clippy::too_many_arguments)]
    pub fn add_item(
        &mut self,
        code: &str,
        page_url: &str,
        title: &str,
        subdir: &str,
        direct: Option<String>,
        size: Option<u64>,
        ext: &str,
        referer: Option<String>,
        hls: bool,
    ) -> bool {
        if self.items.iter().any(|i| i.code == code) {
            return false;
        }
        let mut stem = sanitize(if title.trim().is_empty() { code } else { title });
        if self
            .items
            .iter()
            .any(|i| i.subdir == subdir && i.stem.eq_ignore_ascii_case(&stem))
        {
            stem = format!("{stem} [{code}]");
        }
        let id = self.new_id();
        self.items.push(Item {
            id,
            code: code.to_string(),
            page_url: page_url.to_string(),
            stem,
            subdir: subdir.to_string(),
            ext: ext.to_string(),
            direct_url: direct,
            referer,
            size,
            status: ItemStatus::Ready,
            error: String::new(),
            hls,
            hls_done: 0,
            hls_total: 0,
            hls_bytes: 0,
            downloaded: 0,
            speed: 0.0,
        });
        true
    }
}

// ───────────────────────── portable paths ─────────────────────────

#[derive(Clone)]
pub struct Paths {
    /// folder the app lives in (outside the .app bundle on macOS)
    pub exe_dir: PathBuf,
    pub data_dir: PathBuf,
    /// root of the (external) drive the app is on, when we can tell
    pub vol_root: Option<PathBuf>,
    /// false = the app folder wasn't writable, so data went to the OS user-data folder
    pub portable: bool,
}

impl Paths {
    pub fn detect() -> Paths {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        let mut dir = exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        // macOS: run from inside Something.app -> use the folder that contains the .app
        let outside_bundle = dir
            .ancestors()
            .find(|a| a.extension().is_some_and(|e| e == "app"))
            .and_then(|a| a.parent())
            .map(Path::to_path_buf);
        if let Some(p) = outside_bundle {
            dir = p;
        }

        let wanted = dir.join(DATA_DIR);
        let (data_dir, portable) = if writable(&wanted) {
            (wanted, true)
        } else {
            let fb = dirs::data_dir()
                .unwrap_or_else(|| dir.clone())
                .join("FilecosterDL");
            let _ = fs::create_dir_all(&fb);
            (fb, false)
        };
        let vol_root = volume_root(&dir).filter(|v| !is_system_drive(v));
        Paths {
            exe_dir: dir,
            data_dir,
            vol_root,
            portable,
        }
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }
    pub fn debug_dir(&self) -> PathBuf {
        self.data_dir.join("debug")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.data_dir.join("app.lock")
    }
}

fn writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write_test");
    let ok = fs::write(&probe, b"x").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

#[cfg(windows)]
pub fn volume_root(p: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut it = p.components();
    match (it.next(), it.next()) {
        (Some(Component::Prefix(pre)), Some(Component::RootDir)) => {
            Some(PathBuf::from(pre.as_os_str()).join(std::path::MAIN_SEPARATOR_STR))
        }
        _ => None,
    }
}

#[cfg(not(windows))]
pub fn volume_root(p: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let names: Vec<String> = p
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let n = |i: usize| names.get(i).map(String::as_str);
    match (n(0), n(1), n(2)) {
        (Some("Volumes"), Some(v), _) => Some(PathBuf::from("/Volumes").join(v)),
        (Some("media"), Some(u), Some(v)) => Some(PathBuf::from("/media").join(u).join(v)),
        (Some("run"), Some("media"), _) if names.len() >= 4 => {
            Some(PathBuf::from("/run/media").join(&names[2]).join(&names[3]))
        }
        (Some("mnt"), Some(v), _) => Some(PathBuf::from("/mnt").join(v)),
        _ => None,
    }
}

#[cfg(windows)]
fn is_system_drive(v: &Path) -> bool {
    let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    v.to_string_lossy()
        .to_ascii_lowercase()
        .starts_with(&sys.to_ascii_lowercase())
}

#[cfg(not(windows))]
fn is_system_drive(_v: &Path) -> bool {
    false
}

/// Where the video folder gets looked for first, in order: next to the app, on the drive root, one level up.
pub fn default_base(paths: &Paths) -> PathBuf {
    let roots: Vec<PathBuf> = [
        Some(paths.exe_dir.clone()),
        paths.vol_root.clone(),
        paths.exe_dir.parent().map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    .collect();
    for r in &roots {
        let c = r.join(APP_FOLDER);
        if c.is_dir() {
            return c;
        }
    }
    paths
        .vol_root
        .clone()
        .unwrap_or_else(|| paths.exe_dir.clone())
}

fn stored_root(paths: &Paths) -> PathBuf {
    paths
        .vol_root
        .clone()
        .unwrap_or_else(|| paths.exe_dir.clone())
}

pub fn to_stored(paths: &Paths, p: &Path) -> StoredPath {
    if let Ok(rel) = p.strip_prefix(stored_root(paths)) {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        return StoredPath {
            relative: true,
            path: parts.join("/"),
        };
    }
    StoredPath {
        relative: false,
        path: p.to_string_lossy().into_owned(),
    }
}

/// Turns the saved location into a real path on *this* computer.
pub fn resolve_base(paths: &Paths, s: &Settings) -> (PathBuf, Option<String>) {
    if let Some(sp) = &s.base {
        let p = if sp.relative {
            let mut p = stored_root(paths);
            for c in sp.path.split('/').filter(|c| !c.is_empty()) {
                p.push(c);
            }
            p
        } else {
            PathBuf::from(&sp.path)
        };
        if p.is_dir() || p.parent().is_some_and(|x| x.is_dir()) {
            return (p, None);
        }
        return (
            default_base(paths),
            Some(format!(
                "The saved download location wasn't found on this computer ({}). Using the default; change it any time.",
                p.display()
            )),
        );
    }
    (default_base(paths), None)
}

/// Rule: if the chosen folder is already called "Funny Vid Filecoaster", use it as is;
/// otherwise use (or create) a "Funny Vid Filecoaster" folder inside it.
pub fn final_dir(base: &Path) -> PathBuf {
    let same = base
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(APP_FOLDER));
    if same {
        base.to_path_buf()
    } else {
        base.join(APP_FOLDER)
    }
}

// ───────────────────────── shared runtime ─────────────────────────

pub struct Shared {
    pub state: Mutex<State>,
    pub dirty: AtomicBool,
    pub paths: Paths,
    pub base: Mutex<PathBuf>,
    pub agent: ureq::Agent,
    pub ctx: egui::Context,
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// state changed in a way worth saving
    pub fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
        self.ctx.request_repaint();
    }

    pub fn base_dir(&self) -> PathBuf {
        self.base.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn dest_root(&self) -> PathBuf {
        final_dir(&self.base_dir())
    }

    pub fn item_dir(&self, it: &Item) -> PathBuf {
        self.subdir_dir(&it.subdir)
    }

    /// folder of a sub-folder ("" = the download folder itself)
    pub fn subdir_dir(&self, subdir: &str) -> PathBuf {
        let mut p = self.dest_root();
        for part in subdir.split('/').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }

    /// cheap, UI-only progress update (not saved to disk)
    pub fn update<F: FnOnce(&mut Item)>(&self, id: u64, f: F) {
        let mut st = self.lock();
        if let Some(it) = st.items.iter_mut().find(|i| i.id == id) {
            f(it);
        }
        drop(st);
        self.ctx.request_repaint();
    }

    /// Re-read how much of each video is already on disk (after launch or after changing folder).
    pub fn refresh_progress(&self) {
        let mut st = self.lock();
        for it in st.items.iter_mut() {
            let dir = self.item_dir(it);
            it.downloaded = if it.status == ItemStatus::Done {
                it.size.unwrap_or(0)
            } else {
                fs::metadata(dir.join(it.part_name()))
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
        }
    }

    pub fn save(&self) {
        let bytes = {
            let st = self.lock();
            serde_json::to_vec_pretty(&*st)
        };
        if let Ok(bytes) = bytes {
            let f = self.paths.state_file();
            let tmp = f.with_extension("json.tmp");
            if fs::write(&tmp, &bytes).is_ok() {
                let _ = fs::rename(&tmp, &f);
            }
        }
    }
}

// ───────────────────────── helpers ─────────────────────────

/// Makes a title safe as a file name on Windows, macOS and Linux (and exFAT drives).
pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    s = s
        .trim()
        .trim_start_matches('#')
        .trim()
        .trim_end_matches(['.', ' '])
        .to_string();
    if s.chars().count() > 120 {
        s = s
            .chars()
            .take(120)
            .collect::<String>()
            .trim_end()
            .to_string();
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&s.to_uppercase().as_str()) {
        s.push('_');
    }
    if s.is_empty() {
        s = "video".into();
    }
    s
}

pub fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e12 {
        format!("{:.2} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}
