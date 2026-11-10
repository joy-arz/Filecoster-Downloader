import re, time, threading, base64, json
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs

PORT = 8765
N = 3_000_000
_cache = {}
def data(code, n=N):
    key = (code, n)
    if key not in _cache:
        seed = sum(map(ord, code))
        period = bytes(((j * 7 + seed) % 256) for j in range(256))
        _cache[key] = period * (n // 256) + period[: n % 256]
    return _cache[key]

BIG = 6_000_000
state = {"token_b": 0, "dropped_a": False, "hits": []}
lock = threading.Lock()

def card(code, title):
    return f'<article class="drive-file-card"><a href="/d/{code}" class="thumb-link"><img src="x.jpg"></a><a href="/d/{code}" class="file-name" title="{title}">{title}</a></article>'

def folder_page(title, files, subs=(), pages=(), back=False):
    chips = "".join(f'<a href="/f/{c}" class="folder-chip"><svg></svg><span>{t}</span></a>' for c, t in subs)
    pag = "".join(f'<a class="page-btn" href="{h}">{i}</a>' for i, h in pages)
    return f'''<html><head><title>📂 # {title}</title></head><body><main>
<h1 class="drive-title"># {title}</h1>
<section><div class="folder-row">{chips}</div></section>
<section><div class="file-grid">{"".join(card(c,t) for c,t in files)}</div></section>
<div class="drive-pagination">{pag}</div></main></body></html>'''

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, fmt, *a):
        pass
    def send_html(self, body, code=200):
        b = body.encode()
        self.send_response(code); self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def send_text(self, body, ctype="application/vnd.apple.mpegurl", code=200):
        b = body.encode()
        self.send_response(code); self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def send_bin(self, body, ctype="video/mp2t"):
        self.send_response(200); self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        u = urlparse(self.path); q = parse_qs(u.query); p = u.path
        with lock: state["hits"].append(self.path)
        if p == "/f/root":
            if q.get("page") == ["2"]:
                return self.send_html(folder_page("Test Folder", [("ccc333","## Video Lucu 3")], subs=[("sub","Sub Folder")], pages=[(1,"/f/root"),(2,"/f/root?page=2")]))
            return self.send_html(folder_page("Test Folder", [("aaa111","## Video Lucu 1"),("bbb222","## Video Lucu 2")], pages=[(1,"/f/root"),(2,"/f/root?page=2")]))
        if p == "/f/sub":
            return self.send_html(folder_page("Sub Folder", [("ddd444","Dl link video"),("eee555","No video here")]))
        # --- HLS ---
        if p == "/hls/master.m3u8":
            return self.send_text(
                "#EXTM3U\n#EXT-X-VERSION:3\n"
                "#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n"
                "/hls/low.m3u8\n"
                "#EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720\n"
                "/hls/media.m3u8\n")
        if p == "/hls/low.m3u8":
            return self.send_text(
                "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\n/hls/low1.ts\n#EXT-X-ENDLIST\n")
        if p == "/hls/media.m3u8":
            return self.send_text(
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n"
                "#EXTINF:4.0,\n/hls/seg1.ts\n#EXTINF:4.0,\n/hls/seg2.ts\n#EXTINF:4.0,\n/hls/seg3.ts\n"
                "#EXT-X-ENDLIST\n")
        if p == "/hls/fmp4.m3u8":
            return self.send_text(
                "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:4\n"
                '#EXT-X-MAP:URI="/hls/init.mp4"\n'
                "#EXTINF:4.0,\n/hls/seg1.ts\n#EXTINF:4.0,\n/hls/seg2.ts\n#EXT-X-ENDLIST\n")
        if p == "/hls/init.mp4":
            return self.send_bin(data("init", 1000), "video/mp4")
        if p.startswith("/hls/seg") and p.endswith(".ts"):
            n = {"seg1": 100_000, "seg2": 200_000, "seg3": 150_000, "low1": 50_000}[p[5:-3]]
            return self.send_bin(data(p[5:-3], n))
        m = re.fullmatch(r"/d/(\w+)", p)
        if m:
            c = m.group(1)
            if c == "aaa111": return self.send_html('<html><title>Lucu 1</title><video controls><source src="/media/aaa111.mp4" type="video/mp4"></video></html>')
            if c == "bbb222":
                with lock: state["token_b"] += 1; t = state["token_b"]
                return self.send_html(f'<html><title>Lucu 2</title><video src="/media/bbb222.mp4?t={t}"></video></html>')
            if c == "ccc333": return self.send_html('<html><title>Lucu 3</title><script>var cfg={"file":"http:\\/\\/127.0.0.1:%d\\/media\\/ccc333.mp4?x=1\\u0026y=2"};</script></html>' % PORT)
            if c == "ddd444": return self.send_html('<html><title>Dl</title><a class="btn" href="/dl/ddd444">Download now</a></html>')
            if c == "ifr111":
                payload = base64.urlsafe_b64encode(json.dumps({"id": c, "ti": "Judul Dari Token", "exp": 9999999999}).encode()).decode().rstrip("=")
                return self.send_html("""<html><head><title>funny vid</title></head><body>
<div class="video-content"><div id="player"><div class="video-link"><img class="thumbnail" src="x.jpg"></div>
<iframe id="videq_iframe" style="display:none"></iframe></div></div>
<script>(function () { var SRC = '/embed?id=abc&t=%s.a6706bafa54ed83430f8753eafb6a578'; var TRACE_URLS = ['/cdn-cgi/trace']; })();</script></body></html>""" % payload)
            if c == "slow": return self.send_html('<html><title>Slow one</title><video><source src="/media/slow.mp4"></video></html>')
            if c == "eee555": return self.send_html('<html><title>No video</title><p>nothing</p></html>')
            if c == "hls111":
                return self.send_html('<html><title>Stream</title><script>var cfg={"file":"http:\\/\\/127.0.0.1:%d\\/hls\\/master.m3u8"};</script></html>' % PORT)
            if c == "hls222":
                return self.send_html('<html><title>Stream mp4</title><script>var cfg={"file":"http:\\/\\/127.0.0.1:%d\\/hls\\/fmp4.m3u8"};</script></html>' % PORT)
            if c == "rng200": return self.send_html('<html><title>Rng 200</title><video><source src="/media/rng200.mp4"></video></html>')
            if c == "rng416": return self.send_html('<html><title>Rng 416</title><video><source src="/media/rng416.mp4"></video></html>')
            if c == "rngbad": return self.send_html('<html><title>Rng bad</title><video><source src="/media/rngbad.mp4"></video></html>')
            return self.send_html("nope", 404)
        if p == "/dl/ddd444":
            return self.send_html('<html><a href="/media/ddd444.mp4" download>file</a></html>')
        if p == "/embed":
            return self.send_html('<html><script>player.setup({sources:[{file:"http:\\/\\/127.0.0.1:%d\\/media\\/ifr111.mp4?k=1"}]});</script></html>' % PORT)
        m = re.fullmatch(r"/media/(\w+)\.mp4", p)
        if m:
            return self.media(m.group(1), q)
        self.send_html("nf", 404)
    def media(self, c, q):
        if c == "bbb222":
            with lock: cur = state["token_b"]
            if q.get("t") != [str(cur)]:
                return self.send_html("expired", 403)
        if c == "ifr111" and "/embed?" not in (self.headers.get("Referer") or ""):
            return self.send_html("hotlink", 403)
        total = BIG if c == "slow" else N
        d = data(c, total)
        rng = self.headers.get("Range")
        start, end, code = 0, total - 1, 200
        if rng:
            mm = re.fullmatch(r"bytes=(\d+)-(\d*)", rng)
            start = int(mm.group(1)); end = int(mm.group(2)) if mm.group(2) else total - 1
            if start >= total:
                self.send_response(416); self.send_header("Content-Range", f"bytes */{total}"); self.send_header("Content-Length","0"); self.end_headers(); return
            code = 206
        body = d[start:end+1]

        # --- servers that misbehave on purpose, to test the resume logic ---
        # (only when a resume is asked for, so the 1-byte probe still works)
        if c == "rng200":
            # always answers 200 with the whole file, even when a Range is asked for
            body, start, code = d, 0, 200
        elif c == "rng416" and start > 0:
            # refuses every resume: the client has to throw the part away and start over
            self.send_response(416); self.send_header("Content-Range", f"bytes */{total}")
            self.send_header("Content-Length","0"); self.end_headers(); return
        elif c == "rngbad" and start > 0:
            # answers 206 but with a Content-Range that does not match the request
            body = d[:end+1]
            self.send_response(206); self.send_header("Content-Type", "video/mp4")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Content-Range", f"bytes {start+123}-{end}/{total}")
            self.end_headers(); self.wfile.write(body); return
        self.send_response(code); self.send_header("Content-Type", "video/mp4"); self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(len(body)))
        if code == 206: self.send_header("Content-Range", f"bytes {start}-{end}/{total}")
        self.end_headers()
        try:
            if c == "aaa111" and not state["dropped_a"] and start == 0:
                with lock: state["dropped_a"] = True
                self.wfile.write(body[: len(body)*2//5]); self.wfile.flush()
                self.connection.close(); self.close_connection = True; return
            if c == "slow":
                for i in range(0, len(body), 65536):
                    self.wfile.write(body[i:i+65536]); self.wfile.flush(); time.sleep(0.02)
            else:
                self.wfile.write(body)
        except Exception:
            pass

ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
