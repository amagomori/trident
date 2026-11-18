"""KAN-177 regression: closing a window must leave clipboard ownership alone.

Runs one trident binary: stores a marker through OSC 52, opens more windows,
then closes windows in turn — a window the clipboard is not bound to and the
window it is bound to — first while Trident owns the clipboard and then while
another application (xclip) owns it. Each close must leave the current
selection's text alone, and a fresh store from the surviving window must still
work. Exits 1 on any mismatch.

Run it from a desktop session with xdotool, xclip and a WM running, or pass
--xvfb for a self-contained smoke check (needs Xvfb and openbox):

    python3 .github/scripts/clipboard-qa/close_owner.py \
        --trident ../t-head/target/release/trident --xvfb
"""
import argparse, base64, os, shutil, signal, subprocess, sys, tempfile, time

DELAY = 30  # xdotool type delay in ms — the app needs time to read OSC sequences


def sh(cmd, env, check=True, capture=False):
    r = subprocess.run(cmd, env=env, capture_output=capture, text=True)
    if check and r.returncode != 0:
        fail(f"command failed ({r.returncode}): {cmd}: {r.stderr.strip()}")
    return r


def fail(msg):
    try:
        subprocess.run(["import", "-window", "root", "/tmp/close_owner_fail.png"],
                       env=dict(os.environ), timeout=10)
    except Exception:
        pass
    print(f"FAIL: {msg} (screenshot: /tmp/close_owner_fail.png)", flush=True)
    sys.exit(1)


def check(label, got, want):
    if got != want:
        fail(f"{label}: clipboard is {got!r}, want {want!r}")
    print(f"  ok: {label} -> {got!r}", flush=True)


def xclip_read(env, target=None):
    cmd = ["xclip", "-selection", "clipboard"]
    if target:
        cmd += ["-t", target]
    cmd += ["-o"]
    return subprocess.run(cmd, env=env, capture_output=True, text=True).stdout


def trident_windows(env):
    # xdotool exits 1 when nothing matches, so don't check the return code.
    return sh(["xdotool", "search", "--class", "trident"], env,
              check=False, capture=True).stdout.split()


def focus_window(env, wid):
    # xdotool's --window flag sends synthetic events the toolkit ignores, so
    # keys must go to the focused window; activation can lag, so poll it.
    sh(["xdotool", "windowactivate", wid], env)
    for _ in range(40):
        act = sh(["xdotool", "getactivewindow"], env, check=False,
                 capture=True).stdout.strip()
        if act == wid:
            # The first keystroke can still be eaten while the raise settles;
            # a ctrl+c clears any partial line and absorbs the race.
            time.sleep(0.5)
            sh(["xdotool", "key", "ctrl+c"], env)
            time.sleep(0.3)
            return
        time.sleep(0.1)
    fail(f"window {wid} never took focus")


def type_seq(env, wid, text):
    focus_window(env, wid)
    sh(["xdotool", "type", "--delay", str(DELAY), text], env)
    sh(["xdotool", "key", "Return"], env)


def osc52(env, wid, text):
    b64 = base64.b64encode(text.encode()).decode()
    type_seq(env, wid, "printf '\\033]52;c;" + b64 + "\\007'")
    time.sleep(1)


def open_window(env):
    before = set(trident_windows(env))
    sh(["xdotool", "key", "ctrl+shift+n"], env)
    for _ in range(40):
        new = set(trident_windows(env)) - before
        if new:
            return new.pop()
        time.sleep(0.25)
    fail("new window never appeared after ctrl+shift+n")


def close_window(env, wid):
    # Exit the window's shell: the same path a user takes, and it runs the
    # app's window teardown rather than an X kill, which no window survives.
    focus_window(env, wid)
    sh(["xdotool", "type", "--delay", str(DELAY), "exit"], env)
    sh(["xdotool", "key", "Return"], env)
    for _ in range(60):
        if wid not in trident_windows(env):
            time.sleep(1)  # let the surviving window settle after the swap
            return
        time.sleep(0.25)
    fail(f"window {wid} never closed")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trident", required=True, help="trident binary to test")
    ap.add_argument("--xvfb", action="store_true",
                    help="spawn Xvfb and openbox on a private display")
    ap.add_argument("--display", default=":77", help="display for --xvfb")
    args = ap.parse_args()

    for tool in ("xdotool", "xclip"):
        if not shutil.which(tool):
            fail(f"{tool} not found")

    env = dict(os.environ)
    spawned = []
    if args.xvfb:
        for tool in ("Xvfb", "openbox"):
            if not shutil.which(tool):
                fail(f"{tool} not found for --xvfb")
        lock = f"/tmp/.X{args.display[1:]}-lock"
        if os.path.exists(lock):
            os.unlink(lock)
        env["DISPLAY"] = args.display
        env["XAUTHORITY"] = ""
        os.environ["DISPLAY"] = args.display
        spawned.append(subprocess.Popen(
            ["Xvfb", args.display, "-ac", "-screen", "0", "1280x900x24"],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        time.sleep(1)
        spawned.append(subprocess.Popen(["openbox"], env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        time.sleep(1)
    elif not env.get("DISPLAY"):
        fail("no DISPLAY; pass --xvfb")

    proc = subprocess.Popen([args.trident], env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(60):
            wins = trident_windows(env)
            if wins:
                break
            time.sleep(0.5)
        else:
            fail("trident window never appeared")
        w1 = wins[0]
        sh(["xdotool", "windowactivate", w1], env)
        time.sleep(0.7)

        print("scenario: our selection survives either window closing")
        osc52(env, w1, "TRIDENT_OWNED_MARKER")
        check("initial store", xclip_read(env), "TRIDENT_OWNED_MARKER")
        w2, w3 = open_window(env), open_window(env)
        # w3 is shown; w1 holds the clipboard binding, w2 is background.
        close_window(env, w3)
        check("close unbound window", xclip_read(env), "TRIDENT_OWNED_MARKER")
        close_window(env, w1)
        check("close bound window", xclip_read(env), "TRIDENT_OWNED_MARKER")

        # w2 survives and now holds the binding; storing there must still work.
        osc52(env, w2, "AFTER_REBIND_MARKER")
        check("store after rebind", xclip_read(env), "AFTER_REBIND_MARKER")

        print("scenario: another app's selection survives either window closing")
        w4 = open_window(env)
        w5 = open_window(env)
        # The external owner serves Trident's exact text as text/plain plus a
        # custom target only it can answer: stealing the selection drops the
        # payload even though the plain text stays byte-identical.
        helper = os.path.join(os.path.dirname(__file__), "external_owner.py")
        ext = subprocess.Popen(
            ["python3", helper, "AFTER_REBIND_MARKER", "EXTERNAL_FORMAT_PAYLOAD"],
            env=env)
        time.sleep(1)
        check("external claim, plain text", xclip_read(env),
              "AFTER_REBIND_MARKER")
        check("external claim, custom format",
              xclip_read(env, "text/x-qa-payload"), "EXTERNAL_FORMAT_PAYLOAD")
        # w5 is shown but unbound; w2 holds the binding on every build.
        close_window(env, w5)
        check("close unbound window, plain text", xclip_read(env),
              "AFTER_REBIND_MARKER")
        check("close unbound window, custom format",
              xclip_read(env, "text/x-qa-payload"), "EXTERNAL_FORMAT_PAYLOAD")
        close_window(env, w2)
        check("close bound window, plain text", xclip_read(env),
              "AFTER_REBIND_MARKER")
        check("close bound window, custom format",
              xclip_read(env, "text/x-qa-payload"), "EXTERNAL_FORMAT_PAYLOAD")
        ext.terminate()
        print("PASS: all checks held")
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
        for p in spawned:
            p.terminate()


if __name__ == "__main__":
    main()
