# Filecoster Downloader

Minimal, portable bulk downloader for filecoster.com. One small app (Rust + egui), no installer,
one binary per operating system.

## Usage tutorial

**1. Get the app**

Download the binary for your system from the [releases](https://github.com/joy-arz/Filecoster-Downloader/releases):

| File | Platform |
|---|---|
| `filecoster-dl-mac.tar.gz` | macOS (Intel **and** Apple Silicon) |
| `filecoster-dl.exe` | Windows |
| `filecoster-dl` | Linux |

Unpack it and put the program next to a folder called `filecoster-data` (the app makes this
folder itself the first time it starts). On macOS the first start may need right-click → **Open**,
or `xattr -d com.apple.quarantine filecoster-dl-mac`.

**2. Collect the links**

The links come from wherever you found them — a chat, a forum, or X/Twitter. For X/Twitter there
is a ready-made script, see [Collecting links with Tampermonkey](#collecting-links-with-tampermonkey).

**3. Paste and fetch**

Open the app and paste the links into the big box. **The links do not have to be separated by
Enter** — one per line, separated by spaces or commas, or dropped in the middle of a sentence all
work. Or use **Paste from clipboard**, or **Import .txt…** for a file you saved earlier.

Press **Add & fetch**. The app now looks at every link:

- `/d/…`, `/s/…` and `/e/…` are one video each — the title, the size and the link are found now
- `/f/…` is a folder: every page and every sub-folder is crawled and its videos are added
- the queue is saved, so you can close the app and come back later

**4. Download**

Press **Download all** (or **Queue** on a single row). The videos are saved into the folder
`Funny Vid Filecoaster` — inside the folder you chose with **Change…**, or next to the app if
you never changed it. Videos of a folder link go into a sub-folder named after it.

**5. Keep it tidy**

- **Pause all** stops everything, **Resume** / **Queue** starts a single video again
- **Retry failed** tries the failed ones again
- **Clear finished** takes the finished videos out of the list (the files stay where they are)
- **Reference .txt…** marks videos as "I already have them" so they are never fetched again
  (folders in that file are looked through, so their videos are skipped too)
- **Check duplicates** removes the videos that were downloaded twice
- **Convert unconverted (N)** compresses the finished videos into small `.mp4` files
- **Export my links…** (in *Settings*) writes a `.txt` with everything you downloaded

**The filters** (All, Ready, In queue, Done, Failed) sit under the buttons, and the bar at the
bottom shows how much is left, how fast it is going and how long it will take.

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
     reads a text file (any format, any number of links per line), **Reference .txt…** marks the
     videos of a text file as "already have them"
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

## Nothing is downloaded twice

The code of a video is what identifies it, so the same video is only ever downloaded once:

- `/d/…`, `/s/` and `/e/` of the same code are one video (and one queue entry)
- the same video in two different folders is still one queue entry
- the codes are not case sensitive: `/D/ABC` and `/d/abc` are the same
- a file that is already on the disk with the right size is not downloaded again
- two videos with the same name in the same folder get ` [code]` added — even when they are
  downloaded at the same time, so one never overwrites the other

### The reference list

**Reference .txt…** loads a list of the links you already have. Those videos are then never
fetched or downloaded again, no matter which folder or link they turn up in — the app tells you
how many it skipped, and the ones still in the queue are taken out of it.

It works in both directions:

- a `/d/…`, `/s/…` or `/e/…` link in the reference list: that video is skipped, also when it
  turns up inside a folder
- a `/f/…` link in the reference list: the whole folder is skipped — and the app looks inside
  it once, so that the single links of that folder are skipped as well

**Export my links…** (in *Settings*) writes a `.txt` with the links of everything you already
downloaded. That file can be used as the reference list on another computer, so the two do not
download the same videos twice. Both lists are saved in `filecoster-data/state.json`.

## Collecting links with Tampermonkey

For X/Twitter there is a small script that collects the links while you scroll and saves them as a
`.txt` you can import into the app.

**Install it**

1. Install [Tampermonkey](https://www.tampermonkey.net/) (Chrome, Edge, Firefox, Safari)
2. Open the extension → **Add a new script** → delete everything in the editor
3. Paste the script below → **Ctrl+S** (or the save button) → it is active on x.com right away

**Use it**

- scroll through the posts — the links are collected as they come by
- the blue button in the top right corner shows how many unique links there are
- click it to save them as `filecoster-links-<date>.txt`
- in the app: **Import .txt…** and pick that file

The script also understands `tribunvideo.com` links (the same thing under another name), removes
duplicates and asks `t.co` where a shortened link really points.

```javascript
// ==UserScript==
// @name         X → Filecoster Link Collector
// @namespace    https://x.com/
// @version      1.1.0
// @description  Collects tribunvideo.com/filecoster.com links from posts as you scroll on x.com, converts them to filecoster.com, and saves them as a .txt (one per line, duplicates removed).
// @match        https://x.com/*
// @match        https://twitter.com/*
// @run-at       document-idle
// @grant        GM_getValue
// @grant        GM_setValue
// @grant        GM_registerMenuCommand
// @grant        GM_xmlhttpRequest
// @connect      t.co
// ==/UserScript==

(function () {
  'use strict';

  /* ---------------------------- CONFIG ---------------------------- */
  const STORE_KEY = 'fc_collected_links_v1';
  const TARGET_HOST = 'filecoster.com';

  // Link "type" = the segment after the domain (f, e, d, s, ...).
  // {1,2} = 1 or 2 letters. Raise it (e.g. {1,3}) if longer types appear.
  const TYPE_PATTERN = '[A-Za-z]{1,2}';

  // Real codes are 11 chars (/f/) or 12 chars (/d/ /e/ /s/). Anything shorter
  // than this is treated as a cut-off code and resolved through t.co instead.
  const MIN_CODE_LEN = 11;

  // How many t.co lookups may run at the same time (keep low, be polite).
  const TCO_CONCURRENCY = 2;
  /* ---------------------------------------------------------------- */

  // tribunvideo.com/<type>/<code> or filecoster.com/<type>/<code>, with or
  // without https:// and www. Group 3 catches the "…" X adds when it shortens
  // a long link.
  const LINK_RE = new RegExp(
    '(?<![A-Za-z0-9.\\-])(?:https?:\\/\\/)?(?:www\\.)?(?:tribunvideo|filecoster)\\.com\\/(' +
      TYPE_PATTERN +
      ')\\/([A-Za-z0-9_\\-]+)(…|\\.\\.\\.)?',
    'g'
  );
  const DOMAIN_HINT_RE = /tribunvideo\.com|filecoster\.com/i;
  const TCO_RE = /^https?:\/\/t\.co\//i;

  let cache = new Set(GM_getValue(STORE_KEY, []));

  /* ---------------------------- EXTRACTION ---------------------------- */

  // Returns complete, normalised filecoster.com links found in a string.
  // Cut-off links (ending in "…" or with a too-short code) are ignored here.
  function extract(str) {
    const out = [];
    for (const m of str.matchAll(LINK_RE)) {
      if (m[3] || m[2].length < MIN_CODE_LEN) continue;
      out.push('https://' + TARGET_HOST + '/' + m[1] + '/' + m[2]);
    }
    return out;
  }

  function scan() {
    const found = [];
    document.querySelectorAll('article a').forEach((a) => {
      const title = a.getAttribute('title') || '';
      const href = a.getAttribute('href') || '';
      const text = a.textContent || '';

      // title usually holds the full expanded URL; text can be shortened by X.
      // Long anchors (link cards) are skipped for text: their spans are glued together.
      const sources = [title, href];
      if (text.length <= 80) sources.push(text);
      for (const s of sources) {
        if (!s) continue;
        const res = extract(s);
        if (res.length) {
          found.push(...res);
          return;
        }
      }

      // Nothing usable: either X shortened the link, or it is a link card that
      // only shows the domain. Ask t.co where it really points.
      if (TCO_RE.test(href) && DOMAIN_HINT_RE.test(title + ' ' + text + ' ' + (a.getAttribute('aria-label') || ''))) {
        queueResolve(href);
      }
    });
    addLinks(found);
    render();
  }

  /* ---------------------------- t.co RESOLVER ---------------------------- */

  const tcoState = new Map(); // url -> 'queued' | 'done' | 'failed'
  const tcoQueue = [];
  let tcoActive = 0;

  function queueResolve(url) {
    if (typeof GM_xmlhttpRequest !== 'function' || tcoState.has(url)) return;
    tcoState.set(url, 'queued');
    tcoQueue.push(url);
    pump();
  }

  function pump() {
    while (tcoActive < TCO_CONCURRENCY && tcoQueue.length) {
      const url = tcoQueue.shift();
      tcoActive++;
      GM_xmlhttpRequest({
        method: 'GET',
        url: url,
        anonymous: true,
        timeout: 15000,
        onload: (r) => {
          let links = extract(r.finalUrl || '');
          // If t.co did not redirect, its page contains the target URL itself
          if (!links.length && /^https?:\/\/t\.co\//i.test(r.finalUrl || url)) {
            links = extract((r.responseText || '').replace(/\\\//g, '/'));
          }
          links = [...new Set(links)].slice(0, 1);
          finishResolve(url, links, r.status === 200 || links.length > 0);
        },
        onerror: () => finishResolve(url, [], false),
        ontimeout: () => finishResolve(url, [], false),
      });
    }
  }

  function finishResolve(url, links, definitive) {
    tcoActive--;
    if (links.length) {
      tcoState.set(url, 'done');
      addLinks(links);
    } else {
      tcoState.set(url, 'failed');
      // network problem / rate limit -> allow a retry later
      if (!definitive) setTimeout(() => tcoState.delete(url), 30000);
    }
    render();
    pump();
  }

  function pendingCount() {
    return tcoQueue.length + tcoActive;
  }

  function failedCount() {
    let n = 0;
    tcoState.forEach((v) => {
      if (v === 'failed') n++;
    });
    return n;
  }

  /* ------------------------------ STORAGE ------------------------------ */

  function addLinks(list) {
    const fresh = list.filter((l) => !cache.has(l));
    if (!fresh.length) return;
    // merge with storage first, in case another tab collected links meanwhile
    const merged = new Set(GM_getValue(STORE_KEY, []));
    fresh.forEach((l) => merged.add(l));
    cache = merged;
    GM_setValue(STORE_KEY, [...cache]);
  }

  function refreshFromStorage() {
    cache = new Set([...GM_getValue(STORE_KEY, []), ...cache]);
    render();
  }

  /* ------------------------------ ACTIONS ------------------------------ */

  function saveTxt() {
    scan();
    // final duplicate check right before saving
    const unique = [...new Set([...GM_getValue(STORE_KEY, []), ...cache])];
    if (!unique.length) {
      flash('No links yet');
      return;
    }
    cache = new Set(unique);
    GM_setValue(STORE_KEY, unique);

    const blob = new Blob([unique.join('\n')], { type: 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    const d = new Date();
    const p = (n) => String(n).padStart(2, '0');
    const stamp =
      d.getFullYear() + p(d.getMonth() + 1) + p(d.getDate()) + '-' + p(d.getHours()) + p(d.getMinutes()) + p(d.getSeconds());
    a.href = url;
    a.download = 'filecoster-links-' + stamp + '.txt';
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 5000);

    const pending = pendingCount();
    flash('Saved ' + unique.length + (pending ? ' (' + pending + ' still resolving)' : ''));
  }

  function clearAll() {
    if (!cache.size) return;
    if (!confirm('Clear all ' + cache.size + ' collected links?')) return;
    cache = new Set();
    GM_setValue(STORE_KEY, []);
    render();
  }

  /* -------------------------------- UI -------------------------------- */

  const host = document.createElement('div');
  host.style.cssText = 'position:fixed;top:10px;right:10px;z-index:2147483647;';
  const root = host.attachShadow({ mode: 'open' });
  root.innerHTML =
    '<style>' +
    '.wrap{display:flex;gap:6px;font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif}' +
    'button{border:0;cursor:pointer;font:600 13px/1 inherit;color:#fff;border-radius:999px;' +
    'box-shadow:0 2px 8px rgba(0,0,0,.35);transition:filter .15s}' +
    'button:hover{filter:brightness(1.15)}' +
    '#save{background:#1d9bf0;padding:10px 16px}' +
    '#clear{background:#536471;padding:10px 12px}' +
    '</style>' +
    '<div class="wrap">' +
    '<button id="save" type="button">💾 Save (0)</button>' +
    '<button id="clear" type="button" title="Clear collected links">✕</button>' +
    '</div>';
  const saveBtn = root.getElementById('save');
  const clearBtn = root.getElementById('clear');
  saveBtn.addEventListener('click', saveTxt);
  clearBtn.addEventListener('click', clearAll);

  let flashTimer = null;
  function render() {
    if (flashTimer) return; // don't overwrite a temporary message
    const pending = pendingCount();
    const failed = failedCount();
    saveBtn.textContent = '💾 Save (' + cache.size + ')' + (pending ? ' …' : '');
    let tip = cache.size + ' unique links collected';
    if (pending) tip += '\n' + pending + ' shortened link(s) being resolved';
    if (failed) tip += '\n' + failed + ' link(s) could not be resolved';
    saveBtn.title = tip;
  }

  function flash(msg) {
    saveBtn.textContent = msg;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => {
      flashTimer = null;
      render();
    }, 2200);
  }

  document.documentElement.appendChild(host);

  /* ----------------------------- OBSERVER ----------------------------- */

  let debounce = null;
  new MutationObserver(() => {
    clearTimeout(debounce);
    debounce = setTimeout(scan, 300);
  }).observe(document.body, { childList: true, subtree: true });

  window.addEventListener('focus', refreshFromStorage);

  GM_registerMenuCommand('Save links to .txt', saveTxt);
  GM_registerMenuCommand('Clear collected links', clearAll);

  scan();
})();
```

The script is also in the repository: `scripts/x-filecoster-link-collector.user.js`.

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
         ├─ failures.log         <- one line for everything that failed
         └─ <code>.html          <- the pages of a video that could not be found
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
| Compress the finished streams into .mp4 | on | Re-encodes every stream with H.265 and deletes the `.ts` (needs ffmpeg) |
| Video codec for the compression | H.265 | H.264 or H.265, with the graphics card or without |
| Quality (higher is better) | 60 | 1–100; 60 is a good size for the quality |

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

## Smaller files: the compression

A stream that comes from the site is often much bigger than it needs to be. After a stream is
downloaded it is re-encoded and saved as an ordinary `.mp4`, and the `.ts` is deleted.

| encoder | 33 s of video | size | speed |
|---|---|---|---|
| the file from the site | 406×720 | 8.29 MB | — |
| H.264 (libx264, CRF 24) | | 1.23 MB | 5× faster than real time |
| **H.265 (libx265, CRF 29)** | | **1.33 MB** | 6× faster |
| H.264 (graphics card) | | 1.6 MB | 20× faster |
| **H.265 (graphics card)** | | **1.4 MB** | **20× faster** |

(The numbers are from one of your own files, measured with VMAF so that the quality is the same.)

**H.265 is the default** — it is the best size for the quality. H.264 plays everywhere, H.265
needs a player that knows it (every Mac since 2017, Windows 10/11, Android 8, and every browser
since 2023). The graphics card encoders are much faster but a little bigger.

- **Compress the finished streams into .mp4** (in *Settings*) does this automatically after every
  download
- **Convert unconverted (N)** does the same for the videos that were downloaded earlier — and
  also for the `.mp4` files that came straight from the server and were never compressed
- the old, bigger file is deleted when the new one is ready
- `ffmpeg` is needed for this. The app looks for it (and offers to install it on the first start
  or after an update: `brew install ffmpeg` on macOS, `winget install Gyan.FFmpeg` on Windows).
  You can also put the `ffmpeg` program next to the app

## Videos that were downloaded twice

The same video is often uploaded more than once, under different codes. Because two videos with
the same title may not share a file name, the second one gets ` [code]` added to its name.

**Check duplicates** looks for these:

- finished videos with the same name whose **contents really match** (the size and a
  fingerprint of the file) → the redundant one is deleted and taken out of the list
- videos in the queue that point at the same file → the extra one is taken out of the queue, so
  it is not downloaded again

## When something fails

Nothing is lost: every failed video is written down in **one** file,
`filecoster-data/debug/failures.log`:

```
2026-11-11 15:14:32  z6wywxfhbunn  https://filecoster.com/d/z6wywxfhbunn
    error: no video link found (3 page(s) checked). An HLS (.m3u8) stream was found, but it could not be read. The server said: HTTP 404. ...
    html: z6wywxfhbunn.html, z6wywxfhbunn.1.html, z6wywxfhbunn.2.html
```

The `html:` line points to the pages that were saved for that video, so you can see exactly what
the app saw. Each of those files starts with a comment holding the URL it came from:

```
<!-- fetched from: https://filecoster.com/stream.php?bucket=vidoyhls&id=z6wywxfhbunn&t=… -->
```

"The server said: HTTP 404" means the video is not on the CDN at all (the upload never finished
or was removed) — trying again later is all that helps. When the CDN is only busy for a moment
(429, 503, a timeout), the video is **not** given up on: it is retried later with the normal
back-off. Only a page that is really gone (404, 410) stays failed.

## Tests

The tests run against a small mock of filecoster:

```
python3 tests/mock_server.py &   # fake filecoster on :8765
cargo test
```

There are also tests that talk to the **real** site (they need internet and are skipped by
default). They are handy when the site changes:

```
cargo test -- --ignored real_site --nocapture
```

They print the title, the link and the Referer that were found, and download one small stream
to check that the whole chain still works. To look at one video on the real site:

```
CODE=abc123 cargo test -- --ignored real_site_debug -- --nocapture
```
