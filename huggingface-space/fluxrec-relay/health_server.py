"""Minimal HTTP server so HF Space stays reachable and GitHub can keep it awake."""
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        return

    def do_GET(self):
        if self.path.split("?", 1)[0] in ("/", "/health"):
            body = b"fluxrec-relay-ok\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(404)
        self.end_headers()


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", 7860), Handler).serve_forever()