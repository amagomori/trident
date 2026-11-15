// keylat: black-box keypress-to-pixel latency for one app window on macOS (KAN-89 / KAN-64).
// Build: swiftc -O keylat.swift -o keylat
// Run:   ./keylat --pid <window-owner pid> [--probes 200]
// Needs Accessibility (to post CGEvents) and Screen Recording (to read other apps' pixels)
// for the terminal app running it. Prints one JSON line: {"samples":[ms...],"timeouts":N,"region":[w,h]}.
import AppKit
import CoreGraphics
import Foundation

func arg(_ name: String, _ dflt: String) -> String {
    let a = CommandLine.arguments
    if let i = a.firstIndex(of: name), i + 1 < a.count { return a[i + 1] }
    return dflt
}
func fail(_ msg: String) -> Never {
    print("{\"error\":\"\(msg)\"}")
    exit(1)
}

let pid = pid_t(arg("--pid", "0"))!
let probes = Int(arg("--probes", "200"))!

func window() -> CGRect? {
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]] else { return nil }
    var best: CGRect?
    for w in list where (w[kCGWindowOwnerPID as String] as? pid_t) == pid && (w[kCGWindowLayer as String] as? Int) == 0 {
        if let b = w[kCGWindowBounds as String] as? NSDictionary, let r = CGRect(dictionaryRepresentation: b),
           r.width > 200, r.height > 200, best == nil || r.width * r.height > best!.width * best!.height {
            best = r
        }
    }
    return best
}

var rect: CGRect?
for _ in 0..<100 where rect == nil {
    rect = window()
    if rect == nil { usleep(100_000) }
}
guard let win = rect else { fail("no on-screen window for pid \(pid)") }
if #available(macOS 14, *) {
    NSRunningApplication(processIdentifier: pid)?.activate()
} else {
    NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
}
usleep(1_500_000)

let src = CGEventSource(stateID: .hidSystemState)
func click(_ p: CGPoint) {
    for t in [CGEventType.leftMouseDown, .leftMouseUp] {
        CGEvent(mouseEventSource: src, mouseType: t, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
    }
}
func press(_ key: CGKeyCode) {
    CGEvent(keyboardEventSource: src, virtualKey: key, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: src, virtualKey: key, keyDown: false)?.post(tap: .cghidEventTap)
}
// CGWindowListCreateImage is obsoleted in the macOS 15 SDK but still works at runtime: load it by name.
typealias CreateImage = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?
guard let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") else {
    fail("CGWindowListCreateImage not found")
}
let createImage = unsafeBitCast(sym, to: CreateImage.self)
// Grab pixels in global display points (top-left origin), as raw bytes plus layout.
func grab(_ r: CGRect) -> (Data, Int, Int, Int)? {
    guard let img = createImage(r, CGWindowListOption.optionOnScreenOnly.rawValue, kCGNullWindowID,
                                CGWindowImageOption.bestResolution.rawValue)?.takeRetainedValue(),
          let data = img.dataProvider?.data as Data? else { return nil }
    return (data, img.width, img.height, img.bytesPerRow)
}

let keyX: CGKeyCode = 0x07, keyDelete: CGKeyCode = 0x33
click(CGPoint(x: win.midX, y: win.midY))
usleep(500_000)
guard let base = grab(win) else { fail("capture failed (Screen Recording permission?)") }
press(keyX)
usleep(1_000_000)
guard let after = grab(win) else { fail("capture failed") }
press(keyDelete)
usleep(1_000_000)
if base.0 == after.0 { fail("keypress produced no visible change (focus, Accessibility or Screen Recording permission?)") }

// Bounding box of changed pixels, back to points, padded by 4 points.
let (bw, bh, row) = (base.1, base.2, base.3)
let scale = CGFloat(bw) / win.width
var (minX, minY, maxX, maxY) = (bw, bh, -1, -1)
base.0.withUnsafeBytes { (b: UnsafeRawBufferPointer) in
    after.0.withUnsafeBytes { (a: UnsafeRawBufferPointer) in
        for y in 0..<bh {
            for x in 0..<bw {
                let o = y * row + x * 4
                if b.load(fromByteOffset: o, as: UInt32.self) != a.load(fromByteOffset: o, as: UInt32.self) {
                    minX = min(minX, x); maxX = max(maxX, x); minY = min(minY, y); maxY = max(maxY, y)
                }
            }
        }
    }
}
let region = CGRect(x: win.minX + CGFloat(minX) / scale - 4, y: win.minY + CGFloat(minY) / scale - 4,
                    width: CGFloat(maxX - minX + 1) / scale + 8, height: CGFloat(maxY - minY + 1) / scale + 8)

var samples: [Double] = []
var timeouts = 0
for i in 0..<probes {
    usleep(useconds_t.random(in: 60_000...140_000))
    guard let cur = grab(region)?.0 else { fail("capture failed") }
    let t0 = DispatchTime.now().uptimeNanoseconds
    press(i % 2 == 0 ? keyX : keyDelete)
    while true {
        let now = grab(region)?.0
        let dt = Double(DispatchTime.now().uptimeNanoseconds - t0) / 1e6
        if let now = now, now != cur { samples.append(dt); break }
        if dt > 1000 { timeouts += 1; break }
    }
}
let out: [String: Any] = ["samples": samples, "timeouts": timeouts,
                          "region": [Int(region.width), Int(region.height)]]
print(String(data: try! JSONSerialization.data(withJSONObject: out), encoding: .utf8)!)
