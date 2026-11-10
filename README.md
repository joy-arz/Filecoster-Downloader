# Filecoster Downloader

Minimal, portable bulk downloader for filecoster.com. One small app (Rust + egui), no installer,
one binary per operating system.

## How it works

The app works in two phases, so nothing is downloaded before you say so.

1. **Fetch** — paste links and press **Add & fetch**:
   - **The links do not have to be separated by Enter.** One per line, separated by spaces or
     commas, or dropped in the middle of a sentence — anything goes. The scheme and `www.` are
     optional and the same link twice is only taken once.
   - `/d/…`, `/s/…` and `/e/…` are single videos (the three are the same thing); they are
     resolved right away (title, size, link)
   - `/f/…` is a folder: every page and every sub-folder is crawled, and its videos are added
     with their title and page URL only — they are looked up when you download them
   - **Paste from clipboard** puts whatever is on the clipboard into the box, **Import .txt…**
     reads a text file (any format, any number of links per line)
   - The queue is saved as JSON, so you can fetch now and download later, here or on another
     computer that has the same drive
2. **Download** — press **Download all** (or **Queue** on a single row). Everything is
   resumable, so closing the app mid-download continues where it stopped on the next launch.

Downloads recover from trouble by themselves: a dropped connection is retried with a growing
back-off (2, 4, 8 … 60 s) and continues from the partial `.part` file; an expired link is looked
up again from the page URL; and a file that was already finished on another computer is not
downloaded twice.

The row shows what is going on: progress, speed, and `retry 2/6: server busy (HTTP 503)` while
waiting. Click the folder name under the title to open it.

## Portable on an external SSD

```
SSD/
├─ Funny Vid Filecoaster/        <- the videos (created, or found if it already exists)
│  ├─ Test Folder/               <- one sub-folder per folder link (can be switched off)
│  │  └─ some video.mp4
└─ FilecosterDL/
   ├─ filecoster-dl-mac          <- macOS binary
   ├─ filecoster-dl-windows.exe  <- Windows binary
   └─ filecoster-data/           <- queue + settings, shared by both computers
      ├─ state.json
      ├─ app.lock
      └─ debug/                  <- pages saved when a video could not be found
```

- The queue and the settings live in `filecoster-data/` next to the app, so the Mac and the
  Windows computer share one queue. If that folder cannot be written to, the queue goes to the
  user-data folder of the OS instead.
- The download location is saved **relative to the drive root**, so the same setting keeps
  working when the drive is `/Volumes/SSD` on macOS and `E:\` on Windows. Drive roots that are
  recognised: `/Volumes/X`, `/media/user/X`, `/run/media/user/X`, `/mnt/X` and the Windows drive
  letter (the system drive is left out). If the saved folder is not on this computer, the app
  falls back to the default and says so.
- **Folder rule:** if the folder you picked is already called `Funny Vid Filecoaster` it is used
  as is; otherwise one with that name is found or created inside it. On the first start the app
  looks for that folder next to itself, then on the drive root, then one level up.
- Names are cleaned up for Windows, macOS and exFAT: no `< > : " / \ | ? *`, no reserved names
  (`CON`, `LPT1`, …), no trailing dots or spaces, at most 120 characters, and no leading `#`.
  Two videos with the same name in the same folder get ` [code]` added. The extension is never
  written twice: a title that already ends in `.mp4` stays `video.mp4`, and a stream that is
  really MPEG-TS becomes `video.ts`.
- Only one copy of the app may use the same queue: the second one is stopped with a message.

## Formatting the SSD as exFAT

exFAT is the one format that macOS and Windows both read *and* write, and it has no 4 GB file
limit (FAT32 does, and NTFS is read-only on macOS).

**macOS** — Disk Utility → select the drive → **Erase** → *Format:* **ExFAT** → *Scheme:*
**GUID Partition Map** → **Erase**. Or from a terminal:

```
diskutil list                 # find the disk number of the USB drive
diskutil eraseDisk ExFAT SSD GPT /dev/disk2
```

**Windows** — File Explorer → right-click the drive → *Format…* → *File system:* **exFAT** →
*Quick Format* → **Start**. (If exFAT is missing from the list, use the disk management tool:
`diskmgmt.msc` → right-click the unallocated area → *New Simple Volume* → choose **exFAT**.)

Check that it works: copy a 5 GB file to the drive from the Mac, and open it on Windows. Then
put the app and the `filecoster-data` folder on it and start the downloader.

## Build

Needs Rust (https://rustup.rs), stable.

```
cargo build --release
```

Binary: `target/release/filecoster-dl` (`target/release/filecoster-dl.exe` on Windows). The
release profile is `opt-level = 3`, thin LTO, one codegen unit and stripped symbols. On Windows
the console window is hidden in release builds.

Easiest way to get a Mac, a Windows and a Linux binary without owning all three: push this to
GitHub and run the **build** workflow (Actions tab). It produces a universal macOS binary (Intel
+ Apple Silicon), a Windows `.exe` and a Linux binary.

First launch on macOS may need right-click > Open (unsigned app).

## Settings

| Setting | Default | What it does |
|---|---|---|
| Downloads at the same time | 3 | How many videos are downloaded in parallel (1–8) |
| Automatic retries per video | 6 | How often a failing download is tried again |
| Continue unfinished downloads when the app opens | on | Picks up the queue that was still running |
| Put videos from a folder link into their own sub-folder | on | Videos of `/f/…` links go into a sub-folder named after it (nested for sub-folders) |

## How video pages are resolved

A `/d/`, `/s/` or `/e/` page contains no video file. It has a generic `<title>` ("funny vid"), a
hidden iframe and a script that hands a player URL to that iframe:

```js
var SRC = '/ip129jk?id=…&t=<base64 json>.<signature>';
```

The real title is inside the signed token `t` (base64url JSON, field `ti`), so the resolver
(`src/resolver.rs`) does this:

1. reads the title from the token (falling back to `og:title`, `h1` and `<title>`)
2. collects candidates: `<video>`/`<source>`, `og:video`, `a[download]`, `data-*` attributes,
   media URLs in the text (`.mp4 .webm .mkv .mov .m4v`), and extension-less URLs that scripts
   assign to `file`/`src`/`url`/`stream`
3. collects follow-up pages: `iframe[src]`, `var X = '/path?…'`, `.src = '…'` and links that
   say "download" or point at `/dl/` or `/get/` — and follows them (up to 3 levels, 10 fetches)
4. checks every candidate with a 1-byte Range request; only a real video file (200/206 and a
   non-text content type) is accepted
5. remembers the page each link was found on, because the CDN may need that page as `Referer`
   when the video is downloaded — that header is always sent

### HLS streams (.m3u8)

When the page leads to a playlist instead of a file, the stream is downloaded part by part and
the parts are joined into one file:

- master playlists: the variant with the highest bandwidth is used
- media playlists, `EXT-X-MAP` (fragmented mp4 → saved as `.mp4`) and `EXT-X-BYTERANGE`
- AES-128 encrypted streams are decrypted on the fly
- the parts that are already on disk are kept, so an interrupted stream continues where it
  stopped instead of starting over

The row shows `12.4 MB · 42/120 parts · 1.2 MB/s` while a stream is downloading.

Links expire (the token has an `exp`), so a video whose link stopped working is looked up again
automatically from its page URL.

If nothing is found, every page that was visited is saved to
`filecoster-data/debug/<code>.html`, `<code>.1.html`, … (each file starts with a comment holding
the URL it came from). That is all that is needed to adjust the resolver. The error says how many
pages were checked, where they were saved, and: *If you use a VPN or Cloudflare WARP, turn it
off.*

## Tests

The tests run against a small mock of filecoster:

```
python3 tests/mock_server.py &   # fake filecoster on :8765
cargo test
```

There are also two tests that talk to the **real** site (they need internet and are skipped by
default). They are handy when the site changes:

```
cargo test -- --ignored real_site --nocapture
```

They print the title, the link and the Referer that were found, and download one small stream
to check that the whole chain still works. When a download fails, the app saves every page it
visited to `filecoster-data/debug/<code>.html`, `<code>.1.html`, … — start there.
