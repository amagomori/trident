"""Check native command arguments at the standalone Trident-to-PTY boundary."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile


with tempfile.TemporaryDirectory() as tmp:
    capture = Path(tmp) / "argv"
    word = b"/w/\xff/a $b{line}.rs"
    env = os.environ.copy()
    env.pop("TRIDENT_SOCKET", None)
    subprocess.run([sys.argv[1], "-e", "/bin/sh", "-c", 'printf "%s" "$1" > "$2"',
                    "capture", word, capture], env=env, timeout=45, check=True)
    assert capture.read_bytes() == word, "argument changed between startup and the PTY child"
