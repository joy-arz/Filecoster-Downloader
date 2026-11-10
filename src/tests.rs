//! Engine tests. The network ones talk to a local mock of filecoster (see README: tests/mock_server.py).
//! Run:  python3 tests/mock_server.py &   then   cargo test

use crate::model::*;
use crate::{downloader, resolver};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn mock() -> String {
    std::env::var("MOCK").unwrap_or_else(|_| "http://127.0.0.1:8765".into())
}

fn expected(code: &str, n: usize) -> Vec<u8> {
    let seed: usize = code.chars().map(|c| c as usize).sum();
    (0..n).map(|i| ((i * 7 + seed) % 256) as u8).collect()
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("fcdl-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn shared(dir: &Path) -> Arc<Shared> {
    let paths = Paths {
        exe_dir: dir.to_path_buf(),
        data_dir: dir.join(DATA_DIR),
        vol_root: None,
        portable: true,
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    Arc::new(Shared {
        state: Mutex::new(State::default()),
        dirty: AtomicBool::new(false),
        paths,
        base: Mutex::new(dir.to_path_buf()),
        agent: resolver::agent(),
        ctx: eframe::egui::Context::default(),
    })
}

fn run_item(sh: &Arc<Shared>, id: u64) {
    sh.lock()
        .items
        .iter_mut()
        .find(|i| i.id == id)
        .unwrap()
        .status = ItemStatus::Downloading;
    downloader::run(sh.clone(), id, Arc::new(AtomicBool::new(false)));
}

#[test]
fn parses_all_link_types_from_messy_text() {
    let t = "see https://filecoster.com/d/nqpg0lpzfgbe, and\nhttps://filecoster.com/s/2t0l9p9e9gv8\nfilecoster.com/f/abc123 https://www.filecoster.com/D/nqpg0lpzfgbe https://i.filecoster.com/image/a.jpg";
    let p = resolver::parse_links(t);
    assert_eq!(p.len(), 3, "duplicates and non-video urls are dropped");
    assert_eq!(p[0].kind, LinkKind::Single);
    assert_eq!(p[1].kind, LinkKind::Single);
    assert_eq!(p[2].kind, LinkKind::Folder);
    assert_eq!(p[2].url, "https://filecoster.com/f/abc123");
}

/// /d/, /s/ and /e/ are the same thing: one video.
#[test]
fn the_three_video_links_are_the_same() {
    let p = resolver::parse_links(
        "https://filecoster.com/d/aaa https://filecoster.com/s/aaa https://filecoster.com/e/aaa",
    );
    assert_eq!(p.len(), 3, "they are three different links");
    for x in &p {
        assert_eq!(x.kind, LinkKind::Single);
        assert_eq!(x.code, "aaa");
    }
    // each keeps the way it was written
    assert_eq!(p[0].url, "https://filecoster.com/d/aaa");
    assert_eq!(p[1].url, "https://filecoster.com/s/aaa");
    assert_eq!(p[2].url, "https://filecoster.com/e/aaa");

    // a link with another word in front of it is not a video link
    let p = resolver::parse_links("https://filecoster.com/v/aaa https://filecoster.com/x/aaa");
    assert!(p.is_empty(), "{p:?}");

    // in the queue the three ways of the same video are one link
    let mut st = State::default();
    assert!(st.add_link(LinkKind::Single, "aaa", "https://filecoster.com/d/aaa"));
    assert!(!st.add_link(LinkKind::Single, "aaa", "https://filecoster.com/s/aaa"));
    assert!(!st.add_link(LinkKind::Single, "aaa", "https://filecoster.com/e/aaa"));
    assert_eq!(st.links.len(), 1);
    // a folder is something else
    assert!(st.add_link(LinkKind::Folder, "aaa", "https://filecoster.com/f/aaa"));
    assert_eq!(st.links.len(), 2);
}

/// The links do NOT have to be separated by Enter: any separator works, and links that sit
/// in the middle of a sentence are found too.
#[test]
fn links_do_not_need_to_be_separated_by_enter() {
    let variants = [
        (
            "https://filecoster.com/d/aaa\nhttps://filecoster.com/d/bbb",
            2,
        ), // one per line
        (
            "https://filecoster.com/d/aaa https://filecoster.com/d/bbb",
            2,
        ), // spaces
        (
            "https://filecoster.com/d/aaa,https://filecoster.com/d/bbb",
            2,
        ), // commas
        (
            "https://filecoster.com/d/aaa\thttps://filecoster.com/d/bbb",
            2,
        ), // tabs
        ("look at https://filecoster.com/d/aaa thanks", 1), // in a sentence
        ("https://filecoster.com/d/aaa.", 1),               // trailing dot
    ];
    for (text, count) in variants {
        let p = resolver::parse_links(text);
        let codes: Vec<&str> = p.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(codes.len(), count, "not found in {text:?}");
        assert_eq!(p[0].code, "aaa");
        assert_eq!(p[0].url, "https://filecoster.com/d/aaa");
    }
    // the scheme and "www." are optional, and the same link twice is only taken once
    let p = resolver::parse_links("filecoster.com/d/aaa https://www.filecoster.com/d/aaa");
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].url, "https://filecoster.com/d/aaa");
}

#[test]
fn recognises_the_drive_the_app_is_on() {
    // the root of the drive the app lives on: this is what the download path is saved relative to
    let cases: &[(&str, &str)] = &[
        ("/Volumes/SSD/FilecosterDL", "/Volumes/SSD"),
        ("/media/budi/SSD/FilecosterDL", "/media/budi/SSD"),
        ("/run/media/budi/SSD/app", "/run/media/budi/SSD"),
        ("/mnt/SSD/app", "/mnt/SSD"),
    ];
    for (path, want) in cases {
        if cfg!(windows) {
            break; // the Windows branch needs a real drive letter
        }
        assert_eq!(
            volume_root(Path::new(path)).as_deref(),
            Some(Path::new(want)),
            "{path}"
        );
    }
    // a folder that is not on a removable drive has no drive root
    if !cfg!(windows) {
        assert_eq!(volume_root(Path::new("/Applications/FilecosterDL")), None);
    }
}

#[test]
fn folder_rule_and_names() {
    assert_eq!(
        final_dir(Path::new("/Volumes/SSD")),
        PathBuf::from("/Volumes/SSD/Funny Vid Filecoaster")
    );
    assert_eq!(
        final_dir(Path::new("/Volumes/SSD/funny vid filecoaster")),
        PathBuf::from("/Volumes/SSD/funny vid filecoaster")
    );
    assert_eq!(sanitize("## Video Lucu 1"), "Video Lucu 1");
    assert_eq!(sanitize("a/b:c?*"), "a_b_c__");
    assert_eq!(sanitize("CON"), "CON_");
    assert_eq!(sanitize("..."), "video");

    // the extension is not written twice
    let mut it = Item {
        id: 1,
        code: "x".into(),
        page_url: "".into(),
        stem: "adik masih SMA tobrut viral.mp4".into(),
        subdir: "".into(),
        ext: "mp4".into(),
        direct_url: None,
        referer: None,
        size: None,
        status: ItemStatus::Ready,
        error: String::new(),
        hls: false,
        hls_done: 0,
        hls_total: 0,
        hls_bytes: 0,
        downloaded: 0,
        speed: 0.0,
    };
    assert_eq!(it.file_name(), "adik masih SMA tobrut viral.mp4");
    assert_eq!(it.part_name(), "adik masih SMA tobrut viral.mp4.part");
    // a stream is saved as .ts, so the .mp4 of the title is replaced
    it.ext = "ts".into();
    assert_eq!(it.file_name(), "adik masih SMA tobrut viral.ts");
    assert_eq!(it.part_name(), "adik masih SMA tobrut viral.ts.part");
    // but a title that already ends in .ts does not get it twice
    it.stem = "video.ts".into();
    it.ext = "ts".into();
    assert_eq!(it.file_name(), "video.ts");
    assert_eq!(it.part_name(), "video.ts.part");
}

#[test]
fn stored_path_is_portable_between_machines() {
    // same SSD, different mount points: Mac /Volumes/SSD vs Windows E:\
    let mac = Paths {
        exe_dir: "/Volumes/SSD/app".into(),
        data_dir: "/Volumes/SSD/app/d".into(),
        vol_root: Some("/Volumes/SSD".into()),
        portable: true,
    };
    let chosen = PathBuf::from("/Volumes/SSD/Videos");
    let stored = to_stored(&mac, &chosen);
    assert!(stored.relative);
    assert_eq!(stored.path, "Videos");
    let tmp = tmp("portable");
    std::fs::create_dir_all(tmp.join("Videos")).unwrap();
    let other = Paths {
        exe_dir: tmp.join("app"),
        data_dir: tmp.join("d"),
        vol_root: Some(tmp.clone()),
        portable: true,
    };
    let s = Settings {
        base: Some(stored),
        ..Settings::default()
    };
    let (resolved, notice) = resolve_base(&other, &s);
    assert_eq!(resolved, tmp.join("Videos"));
    assert!(notice.is_none());
}

#[test]
fn crawls_folder_pages_and_subfolders() {
    let agent = resolver::agent();
    let (files, warn) =
        resolver::crawl_folder(&agent, &format!("{}/f/root", mock()), true).unwrap();
    assert!(warn.is_empty(), "{warn:?}");
    let mut codes: Vec<_> = files.iter().map(|f| f.code.as_str()).collect();
    codes.sort();
    assert_eq!(codes, ["aaa111", "bbb222", "ccc333", "ddd444", "eee555"]);
    let a = files.iter().find(|f| f.code == "aaa111").unwrap();
    assert_eq!(a.title, "## Video Lucu 1");
    assert_eq!(a.subdir, "Test Folder");
    let d = files.iter().find(|f| f.code == "ddd444").unwrap();
    assert_eq!(d.subdir, "Test Folder/Sub Folder");
    let (flat, _) = resolver::crawl_folder(&agent, &format!("{}/f/root", mock()), false).unwrap();
    assert!(flat.iter().all(|f| f.subdir.is_empty()));
}

#[test]
fn finds_video_in_every_page_style() {
    let agent = resolver::agent();
    let dbg = tmp("dbg");
    for (code, title) in [("aaa111", "Lucu 1"), ("ccc333", "Lucu 3"), ("ddd444", "Dl")] {
        let m = resolver::resolve_media(&agent, &format!("{}/d/{code}", mock()), &dbg, code)
            .ok()
            .unwrap_or_else(|| panic!("{code} failed"));
        assert_eq!(m.title, title);
        assert_eq!(m.probe.size, Some(3_000_000));
        assert_eq!(m.probe.ext, "mp4");
    }
    let err = resolver::resolve_media(&agent, &format!("{}/d/eee555", mock()), &dbg, "eee555")
        .err()
        .unwrap();
    assert!(err.permanent);
    assert!(dbg.join("eee555.html").exists(), "page saved for diagnosis");
}

#[test]
fn downloads_resumes_and_relinks() {
    let dir = tmp("dl");
    let sh = shared(&dir);
    let agent = resolver::agent();
    let dbg = tmp("dbg2");

    // a: resolved lazily; the server cuts the connection at 40% the first time -> auto-resume
    // b: link resolved now, then the token expires -> must re-resolve by itself
    // c: JS-embedded url, d: via a "Download" page
    let mut ids = vec![];
    for (code, sub) in [
        ("aaa111", "Test Folder"),
        ("bbb222", "Test Folder"),
        ("ccc333", "Test Folder"),
        ("ddd444", "Test Folder/Sub Folder"),
    ] {
        let url = format!("{}/d/{code}", mock());
        let direct = if code == "bbb222" {
            let m = resolver::resolve_media(&agent, &url, &dbg, code)
                .ok()
                .unwrap();
            Some((m.probe.url, m.probe.size))
        } else {
            None
        };
        {
            let mut st = sh.lock();
            let (d, sz) = match direct.clone() {
                Some((u, s)) => (Some(u), s),
                None => (None, None),
            };
            st.add_item(
                code,
                &url,
                &format!("## {code}"),
                sub,
                d,
                sz,
                "mp4",
                None,
                false,
            );
            ids.push(st.items.last().unwrap().id);
        }
        if code == "bbb222" {
            // somebody (e.g. another device) fetches the page again: old token now invalid
            let _ = resolver::resolve_media(&agent, &url, &dbg, code);
        }
    }
    for id in &ids {
        run_item(&sh, *id);
    }
    let st = sh.lock();
    for it in &st.items {
        assert_eq!(it.status, ItemStatus::Done, "{}: {}", it.code, it.error);
        let p = sh.item_dir(it).join(it.file_name());
        assert_eq!(
            std::fs::read(&p).unwrap(),
            expected(&it.code, 3_000_000),
            "{} content",
            it.code
        );
        assert!(
            !sh.item_dir(it).join(it.part_name()).exists(),
            "no .part left behind"
        );
    }
    assert!(dir
        .join(APP_FOLDER)
        .join("Test Folder")
        .join("aaa111.mp4")
        .exists());
    assert!(
        dir.join(APP_FOLDER)
            .join("Test Folder")
            .join("Sub Folder")
            .exists()
            || dir
                .join(APP_FOLDER)
                .join("Test Folder/Sub Folder/ddd444.mp4")
                .exists()
    );
}

#[test]
fn pause_then_resume_continues_from_partial_file() {
    let dir = tmp("pause");
    let sh = shared(&dir);
    let url = format!("{}/d/slow", mock());
    sh.lock()
        .add_item("slow", &url, "Slow one", "", None, None, "mp4", None, false);
    let id = sh.lock().items[0].id;

    // run, pause as soon as the file has really started to grow
    sh.lock().items[0].status = ItemStatus::Downloading;
    let stop = Arc::new(AtomicBool::new(false));
    let (s2, st2) = (sh.clone(), stop.clone());
    let h = std::thread::spawn(move || downloader::run(s2, id, st2));

    let part = {
        let it = sh.lock().items[0].clone();
        sh.item_dir(&it).join(it.part_name())
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) >= 100_000 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    stop.store(true, Ordering::SeqCst);
    sh.lock().items[0].status = ItemStatus::Paused; // what the UI does
    h.join().unwrap();

    let it = sh.lock().items[0].clone();
    let partial = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    assert!(partial > 0, "partial = {partial}");
    assert!(partial < 6_000_000, "partial = {partial}");
    assert_eq!(it.status, ItemStatus::Paused);

    // "next launch": state is saved + reloaded, then resumed
    sh.save();
    let reloaded = State::load(&sh.paths);
    assert_eq!(reloaded.items.len(), 1);
    *sh.lock() = reloaded;
    run_item(&sh, id); // sets Downloading and runs to the end
    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert_eq!(
        std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap(),
        expected("slow", 6_000_000)
    );
}

#[test]
fn real_style_page_iframe_token_title_and_referer() {
    // page has no video: it hands a player URL (with a signed token holding the title) to an iframe
    let agent = resolver::agent();
    let dbg = tmp("dbg3");
    let url = format!("{}/d/ifr111", mock());
    let m = resolver::resolve_media(&agent, &url, &dbg, "ifr111")
        .ok()
        .expect("resolves via iframe");
    assert_eq!(
        m.title, "Judul Dari Token",
        "title comes from the token, not the generic <title>"
    );
    assert!(
        m.probe.referer.contains("/embed?"),
        "referer = the player page, got {}",
        m.probe.referer
    );
    assert_eq!(m.probe.size, Some(3_000_000));

    // and the download works through the hotlink check (needs that referer)
    let dir = tmp("ifr");
    let sh = shared(&dir);
    sh.lock()
        .add_item("ifr111", &url, &m.title, "", None, None, "mp4", None, false);
    let id = sh.lock().items[0].id;
    run_item(&sh, id);
    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert_eq!(it.stem, "Judul Dari Token");
    assert_eq!(
        std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap(),
        expected("ifr111", 3_000_000)
    );
}

#[test]
fn token_title_is_decoded_from_both_base64_flavours() {
    // url-safe, no padding
    let t = "eyJpZCI6IngiLCJ0aSI6Ikp1ZHVsIiwiZXhwIjoxfQ";
    assert_eq!(
        resolver::title_from_token(&format!(
            "var SRC = '/embed?id=1&t={t}.a6706bafa54ed83430f8753eafb6a578';"
        )),
        Some("Judul".to_string())
    );
    // standard base64 with padding and a '/' in it
    let t2 = "eyJpZCI6IngiLCJ0aSI6ImE/IiwiZXhwIjoxfQ==";
    assert_eq!(
        resolver::title_from_token(&format!("?t={t2}.a6706baf")),
        Some("a?".to_string())
    );
    // no token at all -> no title
    assert_eq!(
        resolver::title_from_token("<html><title>x</title></html>"),
        None
    );
}

#[test]
fn hls_links_are_reported_instead_of_silently_failing() {
    let agent = resolver::agent();
    let dbg = tmp("dbg4");
    let err = resolver::resolve_media(&agent, &format!("{}/d/eee555", mock()), &dbg, "eee555")
        .err()
        .expect("no direct file");
    assert!(
        err.msg
            .contains("If you use a VPN or Cloudflare WARP, turn it off."),
        "{}",
        err.msg
    );
    assert!(err.msg.contains("page(s) checked"));
    assert!(
        dbg.join("eee555.html").exists(),
        "the page is saved for diagnosis"
    );
}

/// The playlist reader: master playlists, media playlists, keys and byte ranges.
#[test]
fn reads_hls_playlists() {
    use crate::hls::*;

    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\nlow.m3u8\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=2400000,CODECS=\"avc1.4d401f,mp4a.40.2\"\nmedia.m3u8\n";
    let v = parse_master(master);
    assert_eq!(v.len(), 2);
    assert_eq!(v[1].url, "media.m3u8");
    assert_eq!(v[1].bandwidth, 2_400_000);

    let media = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:7\n\
                 #EXTINF:4.0,\nseg1.ts\n#EXTINF:4.0,\nseg2.ts\n#EXT-X-ENDLIST\n";
    let pl = parse_media(media, "https://cdn.example.com/x/media.m3u8");
    assert_eq!(pl.parts.len(), 2);
    assert_eq!(pl.parts[0].url, "https://cdn.example.com/x/seg1.ts");
    assert_eq!(pl.parts[1].seq, 8, "the sequence number goes up");
    assert!(!pl.fragmented);
    assert!(pl.parts[0].key.is_none());

    // an encrypted playlist
    let enc = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090A0B0C0D0E0F\n\
               #EXTINF:4.0,\nseg1.ts\n#EXT-X-ENDLIST\n";
    let pl = parse_media(enc, "https://cdn.example.com/x/media.m3u8");
    let key = pl.parts[0].key.as_ref().unwrap();
    assert_eq!(key.method, "AES-128");
    assert_eq!(key.uri, "https://cdn.example.com/x/key.bin");
    assert_eq!(key.iv.unwrap()[0], 0x00);

    // a fragmented mp4 has an init segment, and it comes first
    let fmp4 = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.0,\nseg1.ts\n#EXT-X-ENDLIST\n";
    let pl = parse_media(fmp4, "https://cdn.example.com/x/media.m3u8");
    assert!(pl.fragmented);
    assert_eq!(pl.parts.len(), 2);
    assert_eq!(pl.parts[0].url, "https://cdn.example.com/x/init.mp4");

    // byte ranges
    let br = "#EXTM3U\n#EXT-X-BYTERANGE:1000@0\n#EXTINF:4.0,\nall.ts\n#EXT-X-ENDLIST\n";
    let pl = parse_media(br, "https://cdn.example.com/x/media.m3u8");
    assert_eq!(pl.parts[0].range, Some((0, 1000)));
}

/// AES-128-CBC against the test vector of NIST SP 800-38A (F.2.1).
#[test]
fn aes128_matches_the_standard() {
    let key: [u8; 16] = [
        0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f,
        0x3c,
    ];
    let iv: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    let cipher: Vec<u8> = vec![
        0x76, 0x49, 0xab, 0xac, 0x81, 0x19, 0xb2, 0x46, 0xce, 0xe9, 0x8e, 0x9b, 0x12, 0xe9, 0x19,
        0x7d, 0x50, 0x86, 0xcb, 0x9b, 0x50, 0x72, 0x19, 0xee, 0x95, 0xdb, 0x11, 0x3a, 0x91, 0x76,
        0x78, 0xb2, 0x73, 0xbe, 0xd6, 0xb8, 0xe3, 0xc1, 0x74, 0x3b, 0x71, 0x16, 0xe6, 0x9e, 0x22,
        0x22, 0x95, 0x16, 0x3f, 0xf1, 0xca, 0xa1, 0x68, 0x1f, 0xac, 0x09, 0x12, 0x0e, 0xca, 0x30,
        0x75, 0x86, 0xe1, 0xa7,
    ];
    let plain = crate::hls::aes_cbc_decrypt_raw(&key, &iv, &cipher).expect("decrypts");
    assert_eq!(
        plain,
        vec![
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a, 0xae, 0x2d, 0x8a, 0x57, 0x1e, 0x03, 0xac, 0x9c, 0x9e, 0xb7, 0x6f, 0xac,
            0x45, 0xaf, 0x8e, 0x51, 0x30, 0xc8, 0x1c, 0x46, 0xa3, 0x5c, 0xe4, 0x11, 0xe5, 0xfb,
            0xc1, 0x19, 0x1a, 0x0a, 0x52, 0xef, 0xf6, 0x9f, 0x24, 0x45, 0xdf, 0x4f, 0x9b, 0x17,
            0xad, 0x2b, 0x41, 0x7b, 0xe6, 0x6c, 0x37, 0x10,
        ]
    );

    // the padding of a HLS segment is removed
    let padded = [plain.clone(), vec![0x10u8; 16]].concat();
    assert_eq!(crate::hls::strip_pkcs7(&padded).unwrap(), plain);
}

/// A whole HLS stream is downloaded and the parts are joined.
#[test]
fn downloads_an_hls_stream_and_joins_the_parts() {
    let agent = resolver::agent();
    let dbg = tmp("dbg6");
    let dir = tmp("hls");
    let sh = shared(&dir);

    // the page points at a master playlist
    let url = format!("{}/d/hls111", mock());
    let m = resolver::resolve_media(&agent, &url, &dbg, "hls111")
        .ok()
        .expect("the playlist is found");
    assert!(m.probe.hls, "it is a stream");
    assert_eq!(m.probe.ext, "ts");

    sh.lock().add_item(
        "hls111",
        &url,
        "Stream",
        "",
        Some(m.probe.url),
        m.probe.size,
        &m.probe.ext,
        Some(m.probe.referer),
        m.probe.hls,
    );
    let id = sh.lock().items[0].id;
    run_item(&sh, id);

    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert_eq!(it.hls_total, 3, "three parts");
    assert_eq!(it.hls_done, 3);
    let got = std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap();
    let mut want = expected("seg1", 100_000);
    want.extend(expected("seg2", 200_000));
    want.extend(expected("seg3", 150_000));
    assert_eq!(got, want, "the parts are joined in order");
    assert!(!sh.item_dir(&it).join(it.part_name()).exists());
}

/// A stream that was interrupted continues after the parts that are already on disk.
#[test]
fn an_hls_stream_continues_where_it_stopped() {
    let dir = tmp("hls3");
    let sh = shared(&dir);
    let page = format!("{}/d/hls111", mock());
    let playlist = format!("{}/hls/master.m3u8", mock());

    sh.lock().add_item(
        "hls111",
        &page,
        "Stream",
        "",
        Some(playlist),
        None,
        "ts",
        Some(page.clone()),
        true,
    );
    let id = sh.lock().items[0].id;

    // the first part is already there (the app was closed in the middle)
    let it = sh.lock().items[0].clone();
    let part = sh.item_dir(&it).join(it.part_name());
    std::fs::create_dir_all(part.parent().unwrap()).unwrap();
    std::fs::write(&part, expected("seg1", 100_000)).unwrap();
    {
        let mut st = sh.lock();
        let it = st.items.iter_mut().find(|i| i.id == id).unwrap();
        it.hls_done = 1;
        it.hls_bytes = 100_000;
    }

    run_item(&sh, id);
    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert_eq!(it.hls_done, 3);
    let got = std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap();
    let mut want = expected("seg1", 100_000);
    want.extend(expected("seg2", 200_000));
    want.extend(expected("seg3", 150_000));
    assert_eq!(got, want, "the first part was not downloaded twice");
}

/// A fragmented mp4 stream keeps its init segment in front.
#[test]
fn downloads_a_fragmented_hls_stream() {
    let agent = resolver::agent();
    let dbg = tmp("dbg7");
    let dir = tmp("hls2");
    let sh = shared(&dir);

    let url = format!("{}/d/hls222", mock());
    let m = resolver::resolve_media(&agent, &url, &dbg, "hls222")
        .ok()
        .expect("the playlist is found");
    assert!(m.probe.hls);
    assert_eq!(m.probe.ext, "mp4", "a fragmented stream is an mp4");

    sh.lock().add_item(
        "hls222",
        &url,
        "Stream mp4",
        "",
        Some(m.probe.url),
        m.probe.size,
        &m.probe.ext,
        Some(m.probe.referer),
        m.probe.hls,
    );
    let id = sh.lock().items[0].id;
    run_item(&sh, id);

    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    let got = std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap();
    let mut want = expected("init", 1_000);
    want.extend(expected("seg1", 100_000));
    want.extend(expected("seg2", 200_000));
    assert_eq!(got, want, "the init segment comes first");
}

/// Checks the real site (needs internet).
/// Run with:  cargo test -- --ignored real_site
#[test]
#[ignore]
fn real_site_resolves_the_video() {
    let agent = resolver::agent();
    let dbg = std::env::temp_dir().join("fcdl-real-debug");
    let code = "0xt47mj32rtp";
    let m = resolver::resolve_media(
        &agent,
        &format!("https://filecoster.com/d/{code}"),
        &dbg,
        code,
    )
    .ok()
    .expect("the real page resolves");
    println!("title : {}", m.title);
    println!("url   : {}", m.probe.url);
    println!("hls   : {}", m.probe.hls);
    println!("ext   : {}", m.probe.ext);
    println!("size  : {:?}", m.probe.size);
    println!("referer: {}", m.probe.referer);
    assert!(m.probe.hls, "this video is an HLS stream");
}

/// Downloads a real stream from the site (needs internet, ~a few MB).
/// Run with:  cargo test -- --ignored real_site
#[test]
#[ignore]
fn real_site_downloads_a_stream() {
    use crate::hls;
    let agent = resolver::agent();
    let dbg = std::env::temp_dir().join("fcdl-real-debug");
    let dir = std::env::temp_dir().join("fcdl-real-download");
    let _ = std::fs::remove_dir_all(&dir);
    let sh = shared(&dir);
    let code = "0xt47mj32rtp";
    let page = format!("https://filecoster.com/d/{code}");

    let m = resolver::resolve_media(&agent, &page, &dbg, code)
        .ok()
        .expect("the real page resolves");
    assert!(m.probe.hls);

    // use the smallest variant, so the test does not download the whole video
    let text = std::fs::read_to_string(dbg.join(format!("{code}.2.html"))).unwrap_or_default();
    let master = resolver::send(agent.get(&m.probe.url).set("Referer", &m.probe.referer))
        .unwrap()
        .into_string()
        .unwrap();
    let variants = hls::parse_master(&master);
    assert!(!variants.is_empty(), "the master playlist has variants");
    let small = hls::resolve(&m.probe.url, &variants[0].url);
    println!("variant : {}", small);

    sh.lock().add_item(
        code,
        &page,
        &m.title,
        "",
        Some(small),
        None,
        "ts",
        Some(m.probe.referer.clone()),
        true,
    );
    let id = sh.lock().items[0].id;
    run_item(&sh, id);

    let it = sh.lock().items[0].clone();
    println!("status  : {:?}", it.status);
    println!("error   : {}", it.error);
    println!("parts   : {}/{}", it.hls_done, it.hls_total);
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);

    let p = sh.item_dir(&it).join(it.file_name());
    let data = std::fs::read(&p).unwrap();
    println!("file    : {} ({} bytes)", p.display(), data.len());
    assert!(data.len() > 100_000, "the file is too small");
    assert_eq!(data[0], 0x47, "it starts with an MPEG-TS sync byte");
    let _ = text;
}

/// The three ways a server can mess up a resume, and what the downloader has to do about it.
#[test]
fn resume_survives_a_server_that_ignores_or_lies_about_ranges() {
    let agent = resolver::agent();
    let dbg = tmp("dbg5");

    // one fresh queue per case (a code can only be in the queue once)
    let setup = |name: &str, code: &str| -> (Arc<Shared>, Item) {
        let dir = tmp(name);
        let sh = shared(&dir);
        let url = format!("{}/d/{code}", mock());
        let m = resolver::resolve_media(&agent, &url, &dbg, code)
            .ok()
            .expect("resolves");
        assert_eq!(m.probe.size, Some(3_000_000));
        sh.lock().add_item(
            code,
            &url,
            code,
            "",
            Some(m.probe.url),
            m.probe.size,
            &m.probe.ext,
            Some(m.probe.referer),
            m.probe.hls,
        );
        let it = sh.lock().items.last().unwrap().clone();
        (sh, it)
    };
    let write_part = |sh: &Arc<Shared>, it: &Item, bytes: &[u8]| {
        let p = sh.item_dir(it).join(it.part_name());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    };

    // 1. the server answers 200 to a Range request -> start over from 0
    let (sh, it) = setup("resume-a", "rng200");
    write_part(&sh, &it, &vec![7u8; 1_000]);
    run_item(&sh, it.id);
    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert_eq!(
        std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap(),
        expected("rng200", 3_000_000),
        "the old bytes are not kept"
    );

    // 2. the server answers 416 and the part is already complete -> finish it
    let (sh, it) = setup("resume-b", "rng416");
    write_part(&sh, &it, &expected("rng416", 3_000_000));
    run_item(&sh, it.id);
    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert!(
        !sh.item_dir(&it).join(it.part_name()).exists(),
        "the part was renamed"
    );
    assert_eq!(
        std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap(),
        expected("rng416", 3_000_000)
    );

    // 3. the server answers 416 but the part is short -> throw the part away and start over
    let (sh, it) = setup("resume-c", "rng416");
    write_part(&sh, &it, &vec![7u8; 1_000]);
    run_item(&sh, it.id);
    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert_eq!(
        std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap(),
        expected("rng416", 3_000_000),
        "the short part was deleted and the file downloaded again"
    );

    // 4. the server answers 206 with a Content-Range that isn't what we asked for -> start over
    let (sh, it) = setup("resume-d", "rngbad");
    write_part(&sh, &it, &vec![7u8; 500]);
    run_item(&sh, it.id);
    let it = sh.lock().items[0].clone();
    assert_eq!(it.status, ItemStatus::Done, "{}", it.error);
    assert_eq!(
        std::fs::read(sh.item_dir(&it).join(it.file_name())).unwrap(),
        expected("rngbad", 3_000_000),
        "the mismatched part was deleted and the file downloaded again"
    );
}

/// Resolves one code from the real site and prints what was found. Handy when a video does not
/// work:  CODE=abc123 cargo test -- --ignored real_site_debug -- --nocapture
/// (CODE can also be a full link, e.g. https://filecoster.com/s/abc123)
#[test]
#[ignore]
fn real_site_debug_one_code() {
    let agent = resolver::agent();
    let dbg = std::env::temp_dir().join("fcdl-real-debug");
    let arg = std::env::var("CODE").unwrap_or_else(|_| "0xt47mj32rtp".into());
    let (page, code) = if arg.starts_with("http") {
        let code = arg.rsplit('/').next().unwrap_or("code").to_string();
        (arg.clone(), code)
    } else {
        (format!("https://filecoster.com/d/{arg}"), arg.clone())
    };
    let res = resolver::resolve_media(&agent, &page, &dbg, &code);
    match res {
        Ok(m) => println!(
            "OK  title={}\n    url={}\n    hls={} ext={} size={:?}\n    referer={}",
            m.title, m.probe.url, m.probe.hls, m.probe.ext, m.probe.size, m.probe.referer
        ),
        Err(e) => println!("ERR {}", e.msg),
    }
}
