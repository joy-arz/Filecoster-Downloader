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
