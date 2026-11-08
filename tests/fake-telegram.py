"""A fake of the few Bot API methods messagent uses, for the bridge's test.

POST /inject with {"from": <id>, "text": ...} makes a message arrive in the sender's private chat;
with "photo" or "document" as base64 instead, a photo or a file, with "caption",
and for a file "file_name" and "mime_type".
GET /calls returns every call of the bot so far, as [{"method": ..., "body": ...}];
a file uploaded in a form is in the body as {"filename", "type", "size", "sha256"}.
"""

import base64
import hashlib
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

lock = threading.Condition()
updates = []
calls = []
next_message = [1000]
files = {}


def form(content_type, body):
    """The fields of a multipart form, a file as its name, type, size and hash."""
    boundary = content_type.split("boundary=", 1)[1].encode()
    fields = {}
    for part in body.split(b"--" + boundary)[1:-1]:
        head, _, value = part.strip(b"\r\n").partition(b"\r\n\r\n")
        head = head.decode()
        name = head.split('name="', 1)[1].split('"', 1)[0]
        if 'filename="' in head:
            kind = head.split("Content-Type: ", 1)[1].split("\r\n")[0] if "Content-Type: " in head else None
            fields[name] = {
                "filename": head.split('filename="', 1)[1].split('"', 1)[0],
                "type": kind,
                "size": len(value),
                "sha256": hashlib.sha256(value).hexdigest(),
            }
        else:
            fields[name] = value.decode()
    return fields


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
        if self.path.startswith("/file/"):
            data = files[self.path.rsplit("/", 1)[-1]]
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        with lock:
            self.answer(calls)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length)
        content_type = self.headers.get("Content-Type") or ""
        if content_type.startswith("multipart/form-data"):
            body = form(content_type, raw)
            body["chat_id"] = int(body["chat_id"])
        else:
            body = json.loads(raw or b"{}")
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
                    },
                })
                message = updates[-1]["message"]
                if "text" in body:
                    message["text"] = body["text"]
                if "caption" in body:
                    message["caption"] = body["caption"]
                for kind in ("photo", "document"):
                    if kind in body:
                        file_id = f"{kind}{n}"
                        files[file_id] = base64.b64decode(body[kind])
                        if kind == "photo":
                            message["photo"] = [{"file_id": "small" + file_id}, {"file_id": file_id}]
                        else:
                            message["document"] = {"file_id": file_id, "file_name": body["file_name"], "mime_type": body["mime_type"]}
                lock.notify_all()
            self.answer({"message_id": n})
            return
        method = self.path.rsplit("/", 1)[-1]
        if method == "getFile":
            self.answer({"ok": True, "result": {"file_id": body["file_id"], "file_path": f"files/{body['file_id']}"}})
        elif method == "getMe":
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
            if method in ("sendPhoto", "sendDocument"):
                self.answer({"ok": True, "result": {
                    "message_id": message_id(), "date": 1793000000,
                    "chat": {"id": body["chat_id"], "type": "private"}}})
            elif method == "sendMessage":
                if body.get("parse_mode") not in (None, "HTML", "MarkdownV2", "Markdown"):
                    self.answer({"ok": False, "error_code": 400, "description": "Bad Request: unsupported parse_mode"})
                    return
                self.answer({"ok": True, "result": {
                    "message_id": message_id(), "date": 1793000000,
                    "chat": {"id": body["chat_id"], "type": "private"}, "text": body.get("text")}})
            else:
                self.answer({"ok": True, "result": True})


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
