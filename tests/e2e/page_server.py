"""Local look-alike pages for the page tab tests (#31, #32): a fake tracker and a fake wiki on 127.0.0.1, with invented content only.

PageServer() listens on 127.0.0.1 on a free port in a thread of the test. It counts GET requests per path, so a test can tell that switching workspaces did not reload a page.
Pages:
- /tracker/PAY-1234: title "PAY-1234 Fix invoice rounding", links to /tracker/PAY-1235 and /wiki/Home.
- /tracker/PAY-1235: title "PAY-1235 Round ledger totals", links back to PAY-1234.
- /wiki/Home: title "Billing wiki - Home".
Every other path answers 404. Favicon requests are answered 404 and not counted.
"""

from __future__ import annotations

import html
import threading
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGES: dict[str, tuple[str, str]] = {
    "/tracker/PAY-1234": (
        "PAY-1234 Fix invoice rounding",
        '<h1>PAY-1234 Fix invoice rounding</h1>'
        "<p>Invoices round each line, so totals are off by a cent.</p>"
        '<p><a id="next" href="/tracker/PAY-1235">PAY-1235 Round ledger totals</a></p>'
        '<p><a id="wiki" href="/wiki/Home">Billing wiki</a></p>',
    ),
    "/tracker/PAY-1235": (
        "PAY-1235 Round ledger totals",
        "<h1>PAY-1235 Round ledger totals</h1>"
        '<p><a id="back" href="/tracker/PAY-1234">PAY-1234 Fix invoice rounding</a></p>',
    ),
    "/wiki/Home": (
        "Billing wiki - Home",
        "<h1>Billing wiki</h1><p>How the invented billing-api service issues invoices.</p>",
    ),
}


def page_html(title: str, body: str) -> bytes:
    # The first link of a page fills the whole top of the page, so a click near the top left follows it.
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\">"
        f"<title>{html.escape(title)}</title>"
        "<style>body{margin:0;font:16px sans-serif} a{display:block;padding:40px 20px}</style>"
        f"</head><body>{body}</body></html>"
    ).encode()


class PageServer:
    def __init__(self) -> None:
        self.counts: Counter[str] = Counter()
        self._lock = threading.Lock()
        server = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:  # noqa: N802 - the http.server name
                path = self.path.split("?", 1)[0]
                if path == "/favicon.ico":
                    self.send_error(404)
                    return
                with server._lock:
                    server.counts[path] += 1
                page = PAGES.get(path)
                if page is None:
                    self.send_error(404)
                    return
                content = page_html(*page)
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(content)))
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                self.wfile.write(content)

            def log_message(self, format: str, *args: object) -> None:  # noqa: A002 - the http.server name
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self._server.server_address[1]
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def url(self, path: str) -> str:
        return f"http://127.0.0.1:{self.port}{path}"

    def gets(self, path: str) -> int:
        with self._lock:
            return self.counts[path]

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
