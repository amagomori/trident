"""Check piped raster images and terminal size queries against the built CLI."""
import fcntl
import os
from pathlib import Path
import select
import struct
import subprocess
import sys
import tempfile
import termios

binary = Path(sys.argv[1]).resolve()
samples = Path(__file__).resolve().parents[3] / "crates/term-app/samples"
env = os.environ.copy()
env.pop("TMUX", None)
env.pop("TMUX_PANE", None)


def show(*args, error=None, status=0, **kwargs):
    if "input" not in kwargs:
        kwargs.setdefault("stdin", subprocess.DEVNULL)
    result = subprocess.run([binary, "show", *args], capture_output=True, timeout=5,
                            start_new_session=True, env=env, **kwargs)
    assert result.returncode == status, (args, result.returncode, result.stderr)
    if error:
        assert not result.stdout and error in result.stderr, (args, result)
    return result.stdout


def parity(name, *options):
    path = samples / name
    named = show(*options, path)
    assert named.startswith(b"\x1b_G")
    assert show(*options, "-", input=path.read_bytes()) == named


parity("sample.png")
parity("rotated.jpg", "--width", "3", "--height", "2")
parity("sample.png", "--fit")
png = (samples / "sample.png").read_bytes()
assert show("-", input=png + b"trailing bytes") == show(samples / "sample.png")
assert b"a=f" in show("-", input=(samples / "animated.webp").read_bytes())
with tempfile.TemporaryDirectory() as directory:
    Path(directory, "-").write_text("literal dash")
    assert show("./-", cwd=directory) == b"literal dash\n"

for data, reason in [(b"", b"empty"), (b"not an image", b"raster image"),
                     ((samples / "corrupt.png").read_bytes(), b"decode PNG"),
                     ((samples / "sample.gif").read_bytes()[:-12], b"decode GIF")]:
    show("-", input=data, status=1, error=reason)


def interrupted_streams():
    # The pixels can be intact even when IEND or its checksum is not.
    for missing in (1, 2, 3, 4, 12):
        show("-", input=png[:-missing], status=1, error=b"decode PNG")
    show("-", input=png[:-1] + bytes([png[-1] ^ 1]), status=1, error=b"decode PNG")
    jpeg = (samples / "sample.jpg").read_bytes()[:675]
    webp = (samples / "animated.webp").read_bytes()[:128]
    show("-", input=jpeg, status=1, error=b"decode JPEG")
    show("-", input=webp, status=1, error=b"decode WEBP")
    # EOI in a JPEG comment is not the end of the enclosing image.
    show("-", input=jpeg[:2] + b"\xff\xfe\x00\x04\xff\xd9" + jpeg[2:],
         status=1, error=b"decode JPEG")
    # Even a correctly sized RIFF must contain complete chunk headers.
    show("-", input=webp[:4] + struct.pack("<I", len(webp) - 8) + webp[8:],
         status=1, error=b"decode WEBP")


interrupted_streams()

# An open writer must not delay a usage error until EOF.
read_fd, write_fd = os.pipe()
try:
    show(stdin=read_fd, status=2, error=b"needs a file")
    for args in [("-", "-"), (samples / "sample.png", "-"), ("-", "--", "other")]:
        show(*args, stdin=read_fd, status=2, error=b"only operand")
finally:
    os.close(read_fd)
    os.close(write_fd)

with tempfile.TemporaryFile() as image:
    image.write((samples / "sample.jpg").read_bytes())
    image.truncate(64 * 1024 * 1024)
    image.seek(0)
    assert show("-", stdin=image) == show(samples / "sample.jpg")
with open("/dev/zero", "rb") as infinite:
    show("-", stdin=infinite, status=1, error=b"64 MiB")

master, slave = os.openpty()
try:
    show("-", stdin=slave, status=1, error=b"pipe a raster image")
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))

    def controlling_tty():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    with subprocess.Popen([binary, "show", "--width", "2", "-"], env=env,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          pass_fds=(slave,), preexec_fn=controlling_tty) as child:
        try:
            child.stdin.write((samples / "sample.png").read_bytes())
            child.stdin.flush()
            assert select.select([master], [], [], 5)[0], "missing controlling TTY query"
            assert b"\x1b[14t" in os.read(master, 256)
            os.write(master, b"\x1b[4;480;960t\x1b[?1;2c")
            wire, err = child.communicate(timeout=5)
            assert child.returncode == 0 and b"s=24,v=16" in wire, (wire, err)
        finally:
            child.kill()
finally:
    os.close(master)
    os.close(slave)

# Reset the private server's tile allocator so both encodings use the same IDs.
with tempfile.TemporaryDirectory() as directory:
    socket = str(Path(directory, "tmux"))
    tmux = ["tmux", "-S", socket]
    subprocess.run([*tmux, "-f", "/dev/null", "new-session", "-d", "sleep 60"], check=True)
    try:
        pid = subprocess.check_output([*tmux, "display", "-p", "#{pid}"], text=True).strip()
        env.update(TMUX=f"{socket},{pid},0", TMUX_PANE="%0")
        interrupted_streams()
        path = samples / "sample.jpg"
        named = show(path)
        subprocess.run([*tmux, "set", "-s", "@trident-image-sequence", "0"], check=True)
        assert named.startswith(b"\x1bPtmux;") and show("-", input=path.read_bytes()) == named
    finally:
        subprocess.run([*tmux, "kill-server"], check=True)
