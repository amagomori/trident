#!/usr/bin/env python3
"""Claims CLIPBOARD serving text/plain plus a custom target no terminal offers.

close_owner.py runs this so "another application owns the selection" is
detectable by content: text/plain carries the same string Trident stored, but
text/x-qa-payload carries a payload only this owner serves. If Trident takes
the selection back, the custom target disappears even though the plain text
stays identical.

    python3 .github/scripts/clipboard-qa/external_owner.py [TEXT [PAYLOAD]]

Needs python3-xlib; runs until killed.
"""
import sys

from Xlib import X, Xatom, display
from Xlib.protocol import event

TEXT = sys.argv[1] if len(sys.argv) > 1 else "SAME_TEXT"
PAYLOAD = sys.argv[2] if len(sys.argv) > 2 else "EXTERNAL_FORMAT_PAYLOAD"

d = display.Display()
s = d.screen()
win = s.root.create_window(0, 0, 1, 1, 0, s.root_depth)

CLIP = d.intern_atom("CLIPBOARD")
FMT = d.intern_atom("text/x-qa-payload")
TARGS = d.intern_atom("TARGETS")
SERVING = {
    d.intern_atom("TARGETS"): None,
    d.intern_atom("text/plain"): TEXT,
    d.intern_atom("UTF8_STRING"): TEXT,
    Xatom.STRING: TEXT,
    FMT: PAYLOAD,
}

win.set_selection_owner(CLIP, X.CurrentTime)
d.sync()

while True:
    ev = d.next_event()
    if ev.type != X.SelectionRequest:
        continue
    prop = ev.property if ev.property != X.NONE else ev.target
    if ev.target == TARGS:
        ev.requestor.change_property(prop, Xatom.ATOM, 32, list(SERVING))
    elif ev.target in SERVING:
        ev.requestor.change_property(prop, ev.target, 8,
                                     SERVING[ev.target].encode())
    else:
        prop = X.NONE
    ev.requestor.send_event(event.SelectionNotify(
        requestor=ev.requestor, selection=ev.selection, target=ev.target,
        property=prop, time=ev.time))
    d.sync()
