#!/usr/bin/env python3
"""Record real molso PTY output and replay it as a terminal GIF (Unix)."""

import argparse
import errno
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
import time

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
            if "nothing saved" in line or "refusing to write" in line:
                color = "#f2c08c"
            draw.text((38, 82 + index * 27), line, font=font, fill=color)
        draw.text((38, 591), "fictional credential  /  actual process output", font=small_font, fill="#8095a6")
        frames.append(canvas)
        durations.append(duration)

    with tempfile.TemporaryDirectory(prefix="molso-demo-") as directory:
        root = Path(directory)
        fixture = secrets.token_hex(16).encode()
        (root / ".expected").write_bytes(fixture)
        consumer = root / "consumer.py"
        consumer.write_text("#!" + sys.executable + "\n" + '''import os, sys
from pathlib import Path
expected = Path(__file__).with_name('.expected').read_bytes()
received = sys.stdin.buffer.read() if '--stdin' in sys.argv else os.environ.get('GH_TOKEN', '').encode()
matched = received == expected
print('credential received: MATCH' if matched else 'credential received: MISMATCH')
sys.exit(0 if matched else 1)
''')
        consumer.chmod(0o700)
        env = os.environ.copy()
        env.update(PATH=str(binary.parent) + os.pathsep + env.get("PATH", ""),
                   MOLSO_VAULT=str(root / "vault"), MOLSO_KEY_FILE=str(root / "key"),
                   NO_COLOR="1", TERM="dumb")

        def record(command, expected_code=0, hidden_input=None):
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
            assert code == expected_code, f"{command}: exit {code}"
            frame(2000 if expected_code else 1400)
            print(f"recorded: {command} (exit {code})")

        record("molso init")
        record("molso put github/agent", hidden_input=fixture)
        record("molso list")
        record("molso exec --env GH_TOKEN=github/agent -- ./consumer.py")
        record("molso exec --stdin github/agent -- ./consumer.py --stdin")
        record("molso get github/agent", expected_code=2)
        record("false | molso put github/agent --stdin --update", expected_code=2)
        record("molso exec --env GH_TOKEN=github/agent -- ./consumer.py")
        frame(3500)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(args.output, save_all=True, append_images=frames[1:],
                   duration=durations, loop=0, optimize=True)
    print(f"saved {args.output} ({args.output.stat().st_size:,} bytes)")


if __name__ == "__main__":
    main()
