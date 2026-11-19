"""KAN-177 regression: closing a window must leave clipboard ownership alone.

Runs one trident binary: stores a marker through OSC 52, opens more windows,
then closes windows in turn — a window the clipboard is not bound to and the
window it is bound to — first while Trident owns the clipboard and then while
another application owns it. Each close must leave the current
selection's text alone, and a fresh store from the surviving window must still
work. Exits 1 on any mismatch.

Run it from a dedicated X11 session with xdotool, xclip and a WM running, or pass
--xvfb for a self-contained smoke check (needs Xvfb and openbox):

    python3 .github/scripts/clipboard-qa/close_owner.py \
        --trident ../t-head/target/release/trident --xvfb
"""
import argparse, base64, os, shutil, subprocess, sys, time
from contextlib import ExitStack, contextmanager

DELAY = 30  # xdotool type delay in ms — the app needs time to read OSC sequences


def sh(cmd, env, check=True):
    r = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=10)
    if check and r.returncode != 0:
        fail(f"command failed ({r.returncode}): {cmd}: {r.stderr.strip()}")
    return r


def fail(msg):
    sys.exit(f"FAIL: {msg}")


@contextmanager
def child(cmd, env):
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL)
    try:
        yield proc
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def check(label, got, want):
    if got != want:
        fail(f"{label}: clipboard is {got!r}, want {want!r}")
    print(f"  ok: {label} -> {got!r}", flush=True)


def xclip_read(env, target=None):
    cmd = ["xclip", "-selection", "clipboard"]
    if target:
        cmd += ["-t", target]
    cmd += ["-o"]
    return sh(cmd, env, check=False).stdout


def trident_windows(env, proc):
    if proc.poll() is not None:
        fail(f"Trident process {proc.pid} exited ({proc.returncode})")
    # xdotool exits 1 when nothing matches, so don't check the return code.
    return set(sh(["xdotool", "search", "--all", "--pid", str(proc.pid),
                   "--class", "^trident$"], env, check=False).stdout.split())


def focus_window(env, proc, wid):
    # xdotool's --window flag sends synthetic events the toolkit ignores, so
    # keys must go to the focused window; activation can lag, so poll it.
    if wid not in trident_windows(env, proc):
        fail(f"window {wid} does not belong to Trident process {proc.pid}")
    sh(["xdotool", "windowactivate", wid], env)
    for _ in range(40):
        act = sh(["xdotool", "getactivewindow"], env, check=False).stdout.strip()
        if act == wid:
            time.sleep(0.3)  # let the shell receive focus before typing
            if sh(["xdotool", "getactivewindow"], env).stdout.strip() == wid:
                return
        time.sleep(0.1)
    fail(f"window {wid} never took focus")


def type_seq(env, proc, wid, text):
    focus_window(env, proc, wid)
    # A newly focused shell can miss the first key. Start on a clean prompt.
    sh(["xdotool", "key", "ctrl+c"], env)
    time.sleep(0.3)
    if sh(["xdotool", "getactivewindow"], env).stdout.strip() != wid:
        fail(f"window {wid} lost focus before typing")
    sh(["xdotool", "type", "--delay", str(DELAY), text], env)
    sh(["xdotool", "key", "Return"], env)


def osc52(env, proc, wid, text):
    b64 = base64.b64encode(text.encode()).decode()
    type_seq(env, proc, wid, "printf '\\033]52;c;" + b64 + "\\007'")
    time.sleep(1)


def open_window(env, proc, source):
    before = trident_windows(env, proc)
    focus_window(env, proc, source)
    sh(["xdotool", "key", "ctrl+shift+n"], env)
    for _ in range(40):
        new = trident_windows(env, proc) - before
        if len(new) == 1:
            return new.pop()
        if new:
            fail(f"ambiguous new windows for process {proc.pid}: {new}")
        time.sleep(0.25)
    fail("new window never appeared after ctrl+shift+n")


def close_window(env, proc, wid, survivor):
    # Exit the window's shell: the same path a user takes, and it runs the
    # app's window teardown rather than an X kill, which no window survives.
    if trident_windows(env, proc) != {wid, survivor}:
        fail(f"expected windows {wid} and {survivor} before close")
    type_seq(env, proc, wid, "exit")
    for _ in range(60):
        wins = trident_windows(env, proc)
        if wid not in wins:
            if wins != {survivor}:
                fail(f"after closing {wid}, expected window {survivor}, found {wins}")
            focus_window(env, proc, survivor)
            print(f"  ok: closed {wid}; retained {survivor} (PID {proc.pid})", flush=True)
            return
        time.sleep(0.25)
    fail(f"window {wid} never closed")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trident", required=True, help="trident binary to test")
    ap.add_argument("--xvfb", action="store_true",
                    help="spawn Xvfb and openbox on a private display")
    ap.add_argument("--display", help="display number for --xvfb (default: find a free one)")
    ap.add_argument("--openbox", action="store_true", help=argparse.SUPPRESS)
    args = ap.parse_args()

    for tool in ("xdotool", "xclip"):
        if not shutil.which(tool):
            fail(f"{tool} not found")

    env = dict(os.environ)
    for var in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "TRIDENT_SOCKET"):
        env.pop(var, None)
    if args.xvfb:
        for tool in ("xvfb-run", "Xvfb", "xauth", "openbox"):
            if not shutil.which(tool):
                fail(f"{tool} not found for --xvfb")
        server = ["-a"]
        if args.display is not None:
            if not args.display.startswith(":") or not args.display[1:].isdecimal():
                ap.error("--display must look like :77")
            server = ["-n", args.display[1:]]
        os.execvpe("xvfb-run", ["xvfb-run", *server, "-s", "-screen 0 1280x900x24",
                               sys.executable, __file__, "--openbox", "--trident",
                               args.trident], env)
    elif args.display:
        ap.error("--display requires --xvfb")
    elif not env.get("DISPLAY"):
        fail("no DISPLAY; pass --xvfb")

    with ExitStack() as children:
        if args.openbox:
            wm = children.enter_context(child(["openbox"], env))
            for _ in range(40):
                if wm.poll() is not None:
                    fail("openbox exited before it was ready")
                if sh(["xdotool", "get_num_desktops"], env, check=False).returncode == 0:
                    break
                time.sleep(0.1)
            else:
                fail("openbox did not become the window manager")
        proc = children.enter_context(child([args.trident], env))
        for _ in range(60):
            wins = trident_windows(env, proc)
            if wins:
                break
            time.sleep(0.5)
        else:
            fail("trident window never appeared")
        if len(wins) != 1:
            fail(f"expected one initial Trident window, found {wins}")
        w1 = wins.pop()

        print("scenario: our selection survives either window closing")
        osc52(env, proc, w1, "TRIDENT_OWNED_MARKER")
        check("initial store", xclip_read(env), "TRIDENT_OWNED_MARKER")
        w2 = open_window(env, proc, w1)
        close_window(env, proc, w2, w1)
        check("close unbound window", xclip_read(env), "TRIDENT_OWNED_MARKER")
        w3 = open_window(env, proc, w1)
        close_window(env, proc, w1, w3)
        check("close bound window", xclip_read(env), "TRIDENT_OWNED_MARKER")

        osc52(env, proc, w3, "AFTER_REBIND_MARKER")
        check("store after rebind", xclip_read(env), "AFTER_REBIND_MARKER")

        print("scenario: another app's selection survives either window closing")
        w4 = open_window(env, proc, w3)
        # The external owner serves Trident's exact text as text/plain plus a
        # custom target only it can answer: stealing the selection drops the
        # payload even though the plain text stays byte-identical.
        helper = os.path.join(os.path.dirname(__file__), "external_owner.py")
        ext = children.enter_context(child(
            [sys.executable, helper, "AFTER_REBIND_MARKER", "EXTERNAL_FORMAT_PAYLOAD"], env))
        for _ in range(40):
            if ext.poll() is not None:
                fail("external clipboard owner exited before claiming the selection")
            if xclip_read(env, "text/x-qa-payload") == "EXTERNAL_FORMAT_PAYLOAD":
                break
            time.sleep(0.1)
        check("external claim, plain text", xclip_read(env),
              "AFTER_REBIND_MARKER")
        check("external claim, custom format",
              xclip_read(env, "text/x-qa-payload"), "EXTERNAL_FORMAT_PAYLOAD")
        close_window(env, proc, w4, w3)
        check("close unbound window, plain text", xclip_read(env),
              "AFTER_REBIND_MARKER")
        check("close unbound window, custom format",
              xclip_read(env, "text/x-qa-payload"), "EXTERNAL_FORMAT_PAYLOAD")
        w5 = open_window(env, proc, w3)
        close_window(env, proc, w3, w5)
        check("close bound window, plain text", xclip_read(env),
              "AFTER_REBIND_MARKER")
        check("close bound window, custom format",
              xclip_read(env, "text/x-qa-payload"), "EXTERNAL_FORMAT_PAYLOAD")
        osc52(env, proc, w5, "FINAL_SURVIVOR_MARKER")
        check("store from final survivor", xclip_read(env), "FINAL_SURVIVOR_MARKER")
        print("PASS: all checks held")


if __name__ == "__main__":
    main()
