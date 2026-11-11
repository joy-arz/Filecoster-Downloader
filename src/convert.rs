//! Turning the finished HLS streams (.ts) into ordinary .mp4 files with ffmpeg.
//!
//! MPEG-TS is fine for streaming, but many players do not open it and the file is bigger than
//! the same video in mp4. So after a stream is downloaded it is re-encoded with H.264 (quality
//! barely changes) and saved as .mp4, and the .ts is removed.
//!
//! The finished videos of earlier runs can be converted with the "Convert unconverted" button.

use crate::model::*;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The name of the ffmpeg program (ffmpeg.exe on Windows).
fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// Where is ffmpeg? The setting first, then next to the app, then the PATH, then the usual
/// places.
pub fn find_ffmpeg(sh: &Shared) -> Option<String> {
    let saved = sh.lock().settings.ffmpeg.clone();
    if !saved.trim().is_empty() && Path::new(saved.trim()).is_file() {
        return Some(saved.trim().to_string());
    }
    // right next to the app (you can drop a static ffmpeg there)
    let next = sh.paths.exe_dir.join(exe_name());
    if next.is_file() {
        return Some(next.display().to_string());
    }
    // the PATH
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(if cfg!(windows) { ';' } else { ':' }) {
            if dir.is_empty() {
                continue;
            }
            let p = Path::new(dir).join(exe_name());
            if p.is_file() {
                return Some(p.display().to_string());
            }
        }
    }
    // the usual places
    for d in [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/opt/local/bin",
        "/usr/bin",
        "/bin",
        "/snap/bin",
    ] {
        let p = Path::new(d).join(exe_name());
        if p.is_file() {
            return Some(p.display().to_string());
        }
    }
    None
}

/// The ffprobe that belongs to this ffmpeg (for the length of the video).
fn ffprobe_of(ffmpeg: &str) -> Option<String> {
    let name = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    let p = Path::new(ffmpeg).with_file_name(name);
    if p.is_file() {
        return Some(p.display().to_string());
    }
    find_program(name)
}

fn find_program(name: &str) -> Option<String> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(if cfg!(windows) { ';' } else { ':' }) {
            if dir.is_empty() {
                continue;
            }
            let p = Path::new(dir).join(name);
            if p.is_file() {
                return Some(p.display().to_string());
            }
        }
    }
    None
}

/// How long is the video (in seconds)?
fn duration(ffmpeg: &str, input: &Path) -> Option<f64> {
    let probe = ffprobe_of(ffmpeg)?;
    let out = Command::new(probe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(input)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<f64>().ok()
}

/// Which encoder to use.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    /// H.264, software: plays everywhere
    H264Software,
    /// H.265, software: the smallest files, but slower
    H265Software,
    /// H.264 with the graphics card (macOS): very fast
    H264Hardware,
    /// H.265 with the graphics card (macOS): very fast and small
    H265Hardware,
}

impl Encoder {
    pub fn from_u8(v: u8) -> Encoder {
        match v {
            1 => Encoder::H265Software,
            2 => Encoder::H264Hardware,
            3 => Encoder::H265Hardware,
            _ => Encoder::H264Software,
        }
    }
    /// the graphics card encoders only exist on macOS
    pub fn needs_mac(&self) -> bool {
        matches!(self, Encoder::H264Hardware | Encoder::H265Hardware)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Encoder::H264Software => "H.264 (software) - plays everywhere",
            Encoder::H265Software => "H.265 (software) - smallest files, slower",
            Encoder::H264Hardware => "H.264 (graphics card) - fastest",
            Encoder::H265Hardware => "H.265 (graphics card) - fastest and smallest",
        }
    }
}

/// "quality 1..100" (higher is better) becomes a CRF (lower is better).
fn crf_for(quality: u32, offset: u32) -> u32 {
    let q = quality.clamp(1, 100);
    (32 - (q * 14) / 100 + offset).clamp(14, 34)
}

/// The ffmpeg arguments for the video.
fn video_args(enc: Encoder, quality: u32) -> Vec<String> {
    let q = quality.clamp(1, 100);
    let mut v: Vec<String> = Vec::new();
    let mut push = |s: &str| v.push(s.to_string());
    // the software encoder takes every core by itself, so it only gets half of them:
    // the downloads keep going at the same time
    if !enc.needs_mac() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let half = (cores / 2).max(2);
        push("-threads");
        push(&half.to_string());
    }
    match enc {
        Encoder::H264Software => {
            push("-c:v");
            push("libx264");
            push("-preset");
            push("medium");
            push("-crf");
            push(&crf_for(q, 0).to_string());
            push("-pix_fmt");
            push("yuv420p");
        }
        Encoder::H265Software => {
            push("-c:v");
            push("libx265");
            push("-preset");
            push("medium");
            push("-crf");
            push(&crf_for(q, 5).to_string());
            push("-pix_fmt");
            push("yuv420p");
            push("-tag:v");
            push("hvc1");
        }
        Encoder::H264Hardware => {
            push("-c:v");
            push("h264_videotoolbox");
            push("-q:v");
            push(&q.to_string());
            push("-pix_fmt");
            push("yuv420p");
        }
        Encoder::H265Hardware => {
            push("-c:v");
            push("hevc_videotoolbox");
            push("-q:v");
            push(&q.to_string());
            push("-pix_fmt");
            push("yuv420p");
            push("-tag:v");
            push("hvc1");
        }
    }
    v
}

/// Re-encodes one file to .mp4. `on_progress` gets the fraction (0..1).
pub fn convert_one(
    ffmpeg: &str,
    input: &Path,
    output: &Path,
    enc: Encoder,
    quality: u32,
    stop: &AtomicBool,
    mut on_progress: impl FnMut(f64),
) -> Result<String, String> {
    let total = duration(ffmpeg, input);
    // write next to the target and move it into place when it is done
    // (the name keeps the .mp4 ending, so ffmpeg knows the format)
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into());
    let tmp = output.with_file_name(format!("{stem}.converting.mp4"));
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-progress",
            "pipe:1",
            "-nostats",
            "-y",
            "-i",
        ])
        .arg(input)
        .args(video_args(enc, quality))
        .args(["-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"])
        .arg(&tmp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;

    if let Some(out) = child.stdout.take() {
        let reader = BufReader::new(out);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some(v) = line.strip_prefix("out_time_us=") {
                if let (Some(total), Ok(us)) = (total, v.trim().parse::<f64>()) {
                    if total > 0.0 {
                        on_progress((us / 1_000_000.0 / total).clamp(0.0, 1.0));
                    }
                }
            }
            if stop.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&tmp);
                return Err("stopped".into());
            }
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("ffmpeg failed: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = msg.lines().last().unwrap_or("unknown error").to_string();
        return Err(if msg.is_empty() {
            format!("ffmpeg exited with {}", out.status)
        } else {
            msg
        });
    }
    if !tmp.is_file() {
        return Err("ffmpeg did not write the file".into());
    }
    // the new file is ready, but it is not put in place yet: the caller first checks that it
    // is really smaller than the old one
    on_progress(1.0);
    Ok(tmp.to_string_lossy().into_owned())
}

/// Waits until there is room for another conversion.
///
/// Only one at a time: the software encoder (libx264/libx265) uses every core of the
/// processor, so two of them at the same time are twice as slow as one after the other.
fn acquire(sh: &Shared) {
    loop {
        let n = sh.conversions.load(Ordering::SeqCst);
        if n == 0
            && sh
                .conversions
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn release(sh: &Shared) {
    sh.conversions.store(0, Ordering::SeqCst);
}

/// Converts one item of the queue: the .ts becomes a .mp4 and the .ts is deleted.
pub fn convert_item(sh: &Shared, id: u64, ffmpeg: &str, stop: &AtomicBool) -> Result<(), String> {
    let (dir, stem, ext, size) = {
        let st = sh.lock();
        let it = st
            .items
            .iter()
            .find(|i| i.id == id)
            .ok_or("the video is gone")?;
        (
            sh.item_dir(it),
            it.stem.clone(),
            it.ext.clone(),
            it.size.unwrap_or(0),
        )
    };
    let input = dir.join(file_name_of(&stem, &ext));
    let output = dir.join(file_name_of(&stem, "mp4"));
    if !input.is_file() {
        return Err(format!("{} is not there", input.display()));
    }
    let (enc, quality) = {
        let st = sh.lock();
        (
            Encoder::from_u8(st.settings.video_codec),
            st.settings.quality,
        )
    };
    sh.update(id, |it| {
        it.converting = true;
        it.error.clear();
    });
    // the size of the file before the conversion (for an .mp4 it is replaced in place)
    let old_size = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
    acquire(sh);
    let res = convert_one(ffmpeg, &input, &output, enc, quality, stop, |frac| {
        sh.update(id, |it| it.downloaded = (frac * size as f64) as u64);
    });
    release(sh);
    match res {
        Ok(tmp) => {
            let new_size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            if old_size > 0 && new_size >= old_size {
                // the new file is not smaller: the old one stays
                let _ = std::fs::remove_file(&tmp);
                sh.update(id, |it| {
                    it.converting = false;
                    it.downloaded = old_size;
                    it.size = Some(old_size);
                });
                sh.touch();
                return Err("the compressed file was not smaller, the old one was kept".into());
            }
            // the new file is in place, now the old one goes
            if input != output {
                let _ = std::fs::remove_file(&input);
            }
            let _ = std::fs::remove_file(&output);
            if let Err(e) = std::fs::rename(&tmp, &output) {
                sh.update(id, |it| it.converting = false);
                return Err(format!("could not rename the file: {e}"));
            }
            sh.update(id, |it| {
                if it.ext != "mp4" {
                    it.ext = "mp4".into();
                }
                it.size = Some(new_size);
                it.downloaded = new_size;
                it.converting = false;
                it.converted = true; // it is compressed now
            });
            sh.touch();
            Ok(())
        }
        Err(e) => {
            sh.update(id, |it| {
                it.converting = false;
                it.error = format!("could not convert: {e}");
            });
            sh.touch();
            Err(e)
        }
    }
}

/// Converts every finished video that is still a .ts.
pub fn batch(sh: Arc<Shared>, ids: Vec<u64>, ffmpeg: &str, stop: &AtomicBool) {
    sh.batch_running.store(true, Ordering::SeqCst);
    for id in ids {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let _ = convert_item(&sh, id, ffmpeg, stop);
    }
    sh.batch_running.store(false, Ordering::SeqCst);
}

/// How ffmpeg can be installed on this computer.
pub enum Installer {
    /// the command that installs it
    Command(String, Vec<String>),
    /// there is nothing automatic: this is what the user has to do
    Manual(String),
}

/// The way to install ffmpeg here (None when it is already there).
pub fn installer() -> Option<Installer> {
    if cfg!(target_os = "macos") {
        // homebrew installs it without asking for a password
        if find_program("brew").is_some() {
            return Some(Installer::Command(
                "brew".into(),
                vec!["install".into(), "ffmpeg".into()],
            ));
        }
        return Some(Installer::Manual(
            "Install ffmpeg with Homebrew (brew install ffmpeg) or from evermeet.cx".into(),
        ));
    }
    if cfg!(windows) {
        return Some(Installer::Command(
            "winget".into(),
            vec![
                "install".into(),
                "--id".into(),
                "Gyan.FFmpeg".into(),
                "-e".into(),
                "--accept-package-agreements".into(),
                "--accept-source-agreements".into(),
            ],
        ));
    }
    // linux needs a password, so the desktop helper is used when it is there
    if find_program("pkexec").is_some() {
        return Some(Installer::Command(
            "pkexec".into(),
            vec![
                "apt-get".into(),
                "install".into(),
                "-y".into(),
                "ffmpeg".into(),
            ],
        ));
    }
    Some(Installer::Manual(
        "Install ffmpeg with your package manager (for example: sudo apt install ffmpeg)".into(),
    ))
}

/// Installs ffmpeg. Returns what it did.
pub fn install() -> Result<String, String> {
    match installer() {
        None => Ok("ffmpeg is already there".into()),
        Some(Installer::Manual(how)) => Err(how),
        Some(Installer::Command(program, args)) => {
            let out = Command::new(&program)
                .args(&args)
                .output()
                .map_err(|e| format!("could not run {program}: {e}"))?;
            if out.status.success() {
                Ok(format!("{program} {} finished", args.join(" ")))
            } else {
                let msg = String::from_utf8_lossy(&out.stderr);
                let msg = msg.lines().last().unwrap_or("").to_string();
                Err(if msg.is_empty() {
                    format!("{program} did not finish")
                } else {
                    msg
                })
            }
        }
    }
}

/// The videos that were not compressed with ffmpeg yet: the finished .ts files of the
/// streams and the .mp4 files that came straight from the server.
pub fn unconverted(sh: &Shared) -> Vec<u64> {
    let st = sh.lock();
    st.items
        .iter()
        .filter(|i| i.status == ItemStatus::Done && !i.converted)
        .map(|i| i.id)
        .collect()
}
