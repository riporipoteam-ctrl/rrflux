from http.server import BaseHTTPRequestHandler, HTTPServer


class H(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.path.split("?", 1)[0] in ("/", "/health"):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok\n")
        else:
            self.send_response(404)
            self.end_headers()


import os

port = int(os.environ.get("PORT", "7860"))
HTTPServer(("0.0.0.0", port), H).serve_forever()