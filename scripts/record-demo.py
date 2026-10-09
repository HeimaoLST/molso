#!/usr/bin/env python3
"""Record real molso PTY output and replay it as a terminal GIF (Unix)."""

import argparse
import errno
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import pty
import secrets
import select
import signal
import struct
import sys
import tempfile
import termios
import textwrap
from threading import Thread
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from PIL import Image, ImageDraw, ImageFont


def main():
    project = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=project / "target/release/molso")
    parser.add_argument("--output", type=Path, default=project / "docs/assets/demo.gif")
    parser.add_argument("--font", default="/System/Library/Fonts/Menlo.ttc" if sys.platform == "darwin" else "DejaVuSansMono.ttf")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    font = ImageFont.truetype(args.font, 18)
    small_font = ImageFont.truetype(args.font, 14)
    frames, durations, lines = [], [], []

    def frame(duration):
        canvas = Image.new("RGB", (1200, 640), "#101820")
        draw = ImageDraw.Draw(canvas)
        draw.rounded_rectangle((16, 16, 1184, 624), radius=14, fill="#151f2a", outline="#304150")
        for x, color in [(40, "#ee6a5e"), (62, "#f5bd4f"), (84, "#61c454")]:
            draw.ellipse((x, 34, x + 10, 44), fill=color)
        draw.text((480, 32), "molso / local demo", font=small_font, fill="#8fabbc")
        draw.line((17, 62, 1183, 62), fill="#304150")
        wrapped = []
        for line in "".join(lines).replace("\r", "").split("\n"):
            wrapped.extend(textwrap.wrap(line, width=101, replace_whitespace=False, drop_whitespace=False) or [""])
        for index, line in enumerate(wrapped[-18:]):
            color = "#9adbc9" if line.startswith("$ ") else "#d8e2eb"
            draw.text((38, 82 + index * 27), line, font=font, fill=color)
        draw.text((38, 591), "local authenticated API  /  fictional token", font=small_font, fill="#8095a6")
        frames.append(canvas)
        durations.append(duration)

    with tempfile.TemporaryDirectory(prefix="molso-demo-") as directory:
        root = Path(directory)
        fixture = secrets.token_hex(16).encode()
        authenticated_requests = []

        class DemoAPI(BaseHTTPRequestHandler):
            def do_GET(self):
                authenticated = self.headers.get("Authorization") == "Bearer " + fixture.decode()
                if not authenticated:
                    status, result = 401, {"error": "authentication required"}
                elif self.path != "/user":
                    status, result = 404, {"error": "not found"}
                else:
                    authenticated_requests.append(self.path)
                    status, result = 200, {"login": "demo-agent"}
                body = json.dumps(result).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        server = HTTPServer(("127.0.0.1", 0), DemoAPI)
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        endpoint = f"http://127.0.0.1:{server.server_port}"
        consumer = root / "demo-api"
        consumer.write_text("#!" + sys.executable + "\n" + '''import os, sys
from urllib.request import Request, urlopen
token = sys.stdin.buffer.read().decode() if '--stdin' in sys.argv else os.environ.get('DEMO_TOKEN', '')
request = Request(os.environ['DEMO_API_URL'] + sys.argv[1], headers={'Authorization': 'Bearer ' + token})
with urlopen(request, timeout=5) as response:
    print('HTTP', response.status)
    print(response.read().decode())
''')
        consumer.chmod(0o700)
        env = os.environ.copy()
        env.update(PATH=str(binary.parent) + os.pathsep + env.get("PATH", ""),
                   MOLSO_VAULT=str(root / "vault"), MOLSO_KEY_FILE=str(root / "key"),
                   DEMO_API_URL=endpoint, NO_COLOR="1", TERM="dumb")

        def record(command, hidden_input=None, expected_output=None):
            if lines:
                lines.append("\n")
            lines.append("$ " + command)
            frame(800)
            lines.append("\n")
            pid, fd = pty.fork()
            if pid == 0:
                os.chdir(root)
                os.execve("/bin/sh", ["sh", "-c", command], env)
            output = b""
            deadline = time.monotonic() + 10
            pending = hidden_input
            try:
                import fcntl
                fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 101, 0, 0))
                while True:
                    assert time.monotonic() < deadline, f"timed out: {command}"
                    if pending is not None and b"(input hidden): " in output:
                        if not termios.tcgetattr(fd)[3] & termios.ECHO:
                            frame(1500)
                            os.write(fd, pending + b"\n")
                            pending = None
                    if not select.select([fd], [], [], 0.05)[0]:
                        continue
                    try:
                        chunk = os.read(fd, 4096)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not chunk:
                        break
                    output += chunk
                    assert fixture not in output, "fictional credential was echoed"
                    lines.append(chunk.decode())
                    frame(150)
            except BaseException:
                os.killpg(pid, signal.SIGKILL)
                raise
            finally:
                os.close(fd)
                _, status = os.waitpid(pid, 0)
            code = os.waitstatus_to_exitcode(status)
            assert code == 0, f"{command}: exit {code}"
            if expected_output is not None:
                assert expected_output in output.replace(b"\r", b""), f"missing API result: {command}"
            frame(1400)
            print(f"recorded: {command} (exit {code})")

        try:
            for headers in [{}, {"Authorization": "Bearer wrong-demo-token"}]:
                try:
                    with urlopen(Request(endpoint + "/user", headers=headers), timeout=5):
                        raise AssertionError("demo API accepted an unauthenticated request")
                except HTTPError as error:
                    assert error.code == 401
            print("verified: missing and incorrect tokens both return HTTP 401")

            record("molso init")
            record("molso put demo/agent", hidden_input=fixture)
            record("molso list")
            result = b'HTTP 200\n{"login": "demo-agent"}'
            record("molso exec --env DEMO_TOKEN=demo/agent -- ./demo-api /user", expected_output=result)
            record("molso exec --stdin demo/agent -- ./demo-api /user --stdin", expected_output=result)
            record("molso get demo/agent | ./demo-api /user --stdin", expected_output=result)
            assert authenticated_requests == ["/user"] * 3
            frame(3500)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    args.output.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(args.output, save_all=True, append_images=frames[1:],
                   duration=durations, loop=0, optimize=True)
    print(f"saved {args.output} ({args.output.stat().st_size:,} bytes)")


if __name__ == "__main__":
    main()
