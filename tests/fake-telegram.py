"""A fake of the few Bot API methods messagent uses, for the bridge's test.

POST /inject with {"from": <id>, "text": ...} makes a message arrive in the sender's private chat.
GET /calls returns every call of the bot so far, as [{"method": ..., "body": ...}].
"""

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

lock = threading.Condition()
updates = []
calls = []
next_message = [1000]


def message_id():
    next_message[0] += 1
    return next_message[0]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def answer(self, value):
        body = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except BrokenPipeError:
            pass  # A long poll the bridge gave up on, as it stopped.

    def do_GET(self):
        with lock:
            self.answer(calls)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.path == "/inject":
            with lock:
                sender = body["from"]
                n = message_id()
                updates.append({
                    "update_id": len(updates) + 1,
                    "message": {
                        "message_id": n,
                        "date": 1793000000,
                        "chat": {"id": sender, "type": "private"},
                        "from": {"id": sender, "first_name": body.get("name", "Luis"), "is_bot": False},
                        "text": body["text"],
                    },
                })
                lock.notify_all()
            self.answer({"message_id": n})
            return
        method = self.path.rsplit("/", 1)[-1]
        if method == "getMe":
            self.answer({"ok": True, "result": {"id": 1, "first_name": "Bot", "username": "fakebot"}})
        elif method == "getUpdates":
            offset = body.get("offset", 0)
            with lock:
                lock.wait_for(lambda: any(u["update_id"] >= offset for u in updates), timeout=2)
                fresh = [u for u in updates if u["update_id"] >= offset]
            self.answer({"ok": True, "result": fresh})
        else:
            with lock:
                calls.append({"method": method, "body": body})
            if method == "sendMessage":
                if body.get("parse_mode") not in (None, "HTML", "MarkdownV2", "Markdown"):
                    self.answer({"ok": False, "error_code": 400, "description": "Bad Request: unsupported parse_mode"})
                    return
                self.answer({"ok": True, "result": {
                    "message_id": message_id(), "date": 1793000000,
                    "chat": {"id": body["chat_id"], "type": "private"}, "text": body.get("text")}})
            else:
                self.answer({"ok": True, "result": True})


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
