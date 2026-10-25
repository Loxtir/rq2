#!/usr/bin/env python3
"""Local JSON viewer benchmark server. Run with: python stress_server.py"""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json

HOST = "127.0.0.1"
PORT = 8789
ITEM_COUNT = 65_536


def build_body():
    payload = {
        "generated": True,
        "itemCount": ITEM_COUNT,
        "items": [
            {
                "id": index,
                "nested": {
                    "group": {
                        "index": index,
                        "value": f"nested value {index}",
                    }
                },
            }
            for index in range(ITEM_COUNT)
        ],
    }
    return json.dumps(payload, indent=2).encode("utf-8")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        if self.path == "/health":
            body = b'{"status":"ok"}'
            content_type = "application/json"
        elif self.path == "/stress":
            body = build_body()
            line_count = body.count(b"\n") + 1
            if line_count < 500_000:
                self.send_error(500, f"generated only {line_count} lines")
                return
            content_type = "application/json; charset=utf-8"
            print(f"Served {line_count:,} lines ({len(body):,} bytes)", flush=True)
        else:
            self.send_error(404)
            return

        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, format, *args):
        print(f"{self.address_string()} - {format % args}", flush=True)


if __name__ == "__main__":
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"JSON stress server listening on http://{HOST}:{PORT}", flush=True)
    server.serve_forever()
