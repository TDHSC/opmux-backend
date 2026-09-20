#!/usr/bin/env python3
"""Owned OpenAI-compatible HTTP/TLS simulator for local container checks.

Binds only the requested address. Dummy credentials only. Does not call
real providers. Request bodies, headers, and credentials are not logged.
"""

from __future__ import annotations

import argparse
import json
import ssl
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


MAX_BODY_BYTES = 1_048_576
SIMULATED_CONTENT = "SIMULATED_OPENAI_OK"


class Handler(BaseHTTPRequestHandler):
    """Deterministic Chat Completions and /models fixture."""

    server_version = "OpmuxLocalSimulator/1.0"
    sys_version = ""

    def log_message(self, format, *args):  # noqa: A003
        """Omit request lines, headers, credentials, and bodies."""

    def reply(self, status: int, value: dict) -> None:
        body = json.dumps(value, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def authorized(self) -> bool:
        expected = f"Bearer {self.server.simulator_credential}"
        if self.headers.get("Authorization") == expected:
            return True
        self.reply(401, {"error": {"message": "Dummy fixture authorization required"}})
        return False

    def bump(self, name: str) -> None:
        path = self.server.count_file
        if path is None:
            return
        counts = {"generation": 0, "models": 0}
        if path.is_file():
            try:
                loaded = json.loads(path.read_text())
                if isinstance(loaded, dict):
                    counts.update(
                        {
                            key: int(loaded.get(key, 0) or 0)
                            for key in ("generation", "models")
                        }
                    )
            except (OSError, ValueError, TypeError):
                pass
        counts[name] = counts.get(name, 0) + 1
        path.write_text(json.dumps(counts, separators=(",", ":")))

    def do_GET(self) -> None:  # noqa: N802
        if self.path.split("?", 1)[0] != "/v1/models":
            self.reply(404, {"error": {"message": "Unknown fixture endpoint"}})
            return
        self.bump("models")
        if not self.authorized():
            return
        self.reply(
            200,
            {
                "object": "list",
                "data": [
                    {
                        "id": "example-chat-model",
                        "object": "model",
                        "owned_by": "local-simulator",
                    }
                ],
            },
        )

    def do_POST(self) -> None:  # noqa: N802
        if self.path.split("?", 1)[0] != "/v1/chat/completions":
            self.reply(404, {"error": {"message": "Unknown fixture endpoint"}})
            return
        self.bump("generation")
        if not self.authorized():
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= MAX_BODY_BYTES:
                raise ValueError("Invalid body size")
            request = json.loads(self.rfile.read(length))
            if not isinstance(request, dict):
                raise ValueError("Object required")
            model = request.get("model")
            messages = request.get("messages")
            if not isinstance(model, str) or not model.strip():
                raise ValueError("Invalid model")
            if not isinstance(messages, list) or not messages:
                raise ValueError("Invalid messages")
        except (ValueError, UnicodeError, TypeError, json.JSONDecodeError):
            self.reply(400, {"error": {"message": "Invalid fixture request"}})
            return
        self.reply(
            200,
            {
                "id": "chatcmpl-local-simulator",
                "object": "chat.completion",
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "finish_reason": "stop",
                        "message": {
                            "role": "assistant",
                            "content": SIMULATED_CONTENT,
                        },
                    }
                ],
                "usage": {
                    "prompt_tokens": 120,
                    "completion_tokens": 30,
                    "total_tokens": 150,
                },
            },
        )


class SimulatorServer(ThreadingHTTPServer):
    """HTTP server with dummy credential and optional call counter."""

    def __init__(self, address, credential: str, count_file: Path | None):
        super().__init__(address, Handler)
        self.simulator_credential = credential
        self.count_file = count_file


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--credential", required=True)
    parser.add_argument("--tls-cert")
    parser.add_argument("--tls-key")
    parser.add_argument("--count-file")
    parser.add_argument("--print-port", action="store_true")
    args = parser.parse_args(argv)
    if bool(args.tls_cert) != bool(args.tls_key):
        parser.error("both --tls-cert and --tls-key are required for TLS")
    return args


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    count_file = Path(args.count_file) if args.count_file else None
    server = SimulatorServer((args.host, args.port), args.credential, count_file)
    if args.tls_cert:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(args.tls_cert, args.tls_key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
    if args.print_port:
        print(server.server_address[1], flush=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
