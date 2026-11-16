// Black-box startup and key-to-composited-frame latency for macOS terminals.
// ScreenCaptureKit display stream (frame PTS = WindowServer composite time) + CGEvent key injection.
// usage: mlat <startup|latency> <runs> <probes> <out.json> <hog 0|1> -- cmd args...
import AppKit
import ScreenCaptureKit
import CoreMedia

let argv = CommandLine.arguments
let mode = argv[1], runs = Int(argv[2])!, probes = Int(argv[3])!, outPath = argv[4], hog = argv[5] == "1"
let cmd = Array(argv[(argv.firstIndex(of: "--")! + 1)...])

let trace = ProcessInfo.processInfo.environment["MLAT_TRACE"] != nil
struct Frame { var pts: Double; var arrival: Double; var red: Int; var sum: UInt64 }
final class Capture: NSObject, SCStreamOutput, SCStreamDelegate {
  let lock = NSLock()
  var frames: [Frame] = []
  var rect = CGRect.zero // in pixels
  func stream(_ s: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
    let arrival = CACurrentMediaTime()
    if trace, let a = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]], let st = a.first?[.status] as? Int { FileHandle.standardError.write("frame status=\(st) t=\(arrival)\n".data(using: .utf8)!) }
    guard type == .screen, let atts = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
          let raw = atts.first?[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
          let pb = CMSampleBufferGetImageBuffer(sb) else { return }
    let pts = CMSampleBufferGetPresentationTimeStamp(sb).seconds
    lock.lock(); let r = rect; lock.unlock()
    CVPixelBufferLockBaseAddress(pb, .readOnly)
    let base = CVPixelBufferGetBaseAddress(pb)!.assumingMemoryBound(to: UInt8.self)
    let bpr = CVPixelBufferGetBytesPerRow(pb), w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
    let x0 = max(0, Int(r.minX)), x1 = min(w, Int(r.maxX)), y0 = max(0, Int(r.minY)), y1 = min(h, Int(r.maxY))
    var red = 0; var sum: UInt64 = 1469598103934665603
    if x1 > x0 && y1 > y0 {
      for y in y0..<y1 {
        let row = base + y * bpr
        for x in x0..<x1 {
          let p = row + x * 4 // BGRA
          let b = p[0], g = p[1], rr = p[2]
          if rr > 170 && g < 80 && b < 80 { red += 1 }
          sum = (sum ^ UInt64(b) ^ (UInt64(g) << 8) ^ (UInt64(rr) << 16)) &* 1099511628211
        }
      }
    }
    CVPixelBufferUnlockBaseAddress(pb, .readOnly)
    lock.lock(); frames.append(Frame(pts: pts, arrival: arrival, red: red, sum: sum)); lock.unlock()
  }
  func setRect(_ r: CGRect) { lock.lock(); rect = r; lock.unlock() }
  func snapshot() -> [Frame] { lock.lock(); defer { lock.unlock() }; return frames }
  func clear() { lock.lock(); frames.removeAll(); lock.unlock() }
}

func now() -> Double { CACurrentMediaTime() }
func sleepMs(_ ms: Double) { usleep(useconds_t(ms * 1000)) }
func windows() -> [[String: Any]] {
  (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]) ?? []
}
func winId(_ w: [String: Any]) -> Int { w[kCGWindowNumber as String] as? Int ?? 0 }
func appWindow(excluding before: Set<Int>) -> [String: Any]? {
  windows().first { !before.contains(winId($0)) && ($0[kCGWindowLayer as String] as? Int) == 0 &&
    (($0[kCGWindowBounds as String] as? [String: Double])?["Width"] ?? 0) > 200 }
}

let cap = Capture()
let sema = DispatchSemaphore(value: 0)
var stream: SCStream!
SCShareableContent.getWithCompletionHandler { content, err in
  let d = content!.displays[0]
  let cfg = SCStreamConfiguration()
  cfg.width = d.width; cfg.height = d.height
  cfg.pixelFormat = kCVPixelFormatType_32BGRA
  cfg.minimumFrameInterval = CMTime(value: 1, timescale: Int32(ProcessInfo.processInfo.environment["MLAT_HZ"] ?? "240")!)
  cfg.queueDepth = Int(ProcessInfo.processInfo.environment["MLAT_QD"] ?? "4")!; cfg.showsCursor = false
  let filter = SCContentFilter(display: d, excludingApplications: [], exceptingWindows: [])
  stream = SCStream(filter: filter, configuration: cfg, delegate: cap)
  try! stream.addStreamOutput(cap, type: .screen, sampleHandlerQueue: DispatchQueue(label: "cap", qos: .userInteractive))
  stream.startCapture { e in if let e = e { print("capture error", e); exit(1) }; sema.signal() }
}
sema.wait()
let screen = NSScreen.screens[0].frame
cap.setRect(CGRect(x: 0, y: 0, width: screen.width, height: screen.height))
sleepMs(500)

var hogs: [Process] = []
if hog { for _ in 0..<ProcessInfo.processInfo.activeProcessorCount { let p = Process(); p.executableURL = URL(fileURLWithPath: "/usr/bin/yes"); p.standardOutput = FileHandle.nullDevice; try! p.run(); hogs.append(p) } }

func launch() -> (Process, Double) {
  let p = Process(); p.executableURL = URL(fileURLWithPath: cmd[0]); p.arguments = Array(cmd.dropFirst())
  p.standardOutput = FileHandle.nullDevice; p.standardError = FileHandle.nullDevice
  let t0 = now(); try! p.run(); return (p, t0)
}
func killApp(_ p: Process) {
  kill(p.processIdentifier, SIGTERM); let end = now() + 3
  while p.isRunning && now() < end { sleepMs(20) }
  if p.isRunning { kill(p.processIdentifier, SIGKILL) }
  if let name = ProcessInfo.processInfo.environment["MLAT_KILL"] { let k = Process(); k.executableURL = URL(fileURLWithPath: "/usr/bin/killall"); k.arguments = ["-9", name]; try? k.run(); k.waitUntilExit() }
  sleepMs(1000)
}
var lastRedPts = 0.0
func waitRed(after t0: Double, timeout: Double = 15) -> Double? {
  let end = now() + timeout
  while now() < end {
    if let f = cap.snapshot().first(where: { $0.arrival > t0 && $0.red > 150 }) { lastRedPts = f.pts; return f.arrival }
    sleepMs(2)
  }
  return nil
}

let baseline = Set(windows().map(winId))
var results: [[String: Any]] = []
for run in 1...runs {
  cap.setRect(CGRect(x: 0, y: 0, width: screen.width, height: screen.height)); cap.clear()
  let quiet = now() + 10
  while now() < quiet && !Set(windows().filter { ($0[kCGWindowLayer as String] as? Int) == 0 }.map(winId)).isSubset(of: baseline) { sleepMs(20) }
  sleepMs(400); cap.clear()
  let before = Set(windows().map(winId))
  let (p, t0) = launch()
  var mapped: Double? = nil; var win: [String: Any]? = nil
  let end = now() + 15
  while now() < end && win == nil { if let w = appWindow(excluding: before) { win = w; mapped = (now() - t0) * 1000 } else { sleepMs(1) } }
  let red = waitRed(after: t0)
  var row: [String: Any] = ["run": run, "mapped_ms": mapped as Any, "painted_ms": red.map { ($0 - t0) * 1000 } as Any, "painted_pts_ms": (lastRedPts - t0) * 1000]
  if mode == "latency", red != nil {
    sleepMs(1500)
    let wb = (appWindow(excluding: before) ?? win)?[kCGWindowBounds as String] as? [String: Double]
    let r = wb.map { b in CGRect(x: b["X"]!, y: b["Y"]! + 30, width: b["Width"]!, height: b["Height"]! - 30) } ?? CGRect(x: 0, y: 40, width: screen.width, height: screen.height - 40)
    row["rect"] = wb == nil ? "screen" : "window"
    if let pid = (appWindow(excluding: before) ?? win)?[kCGWindowOwnerPID as String] as? Int32 { NSRunningApplication(processIdentifier: pid)?.activate() }
    else { NSRunningApplication(processIdentifier: p.processIdentifier)?.activate() }
    // focus with a click in the window
    let c = CGPoint(x: r.midX, y: r.midY), src = CGEventSource(stateID: .hidSystemState)
    if wb != nil { CGEvent(mouseEventSource: src, mouseType: .leftMouseDown, mouseCursorPosition: c, mouseButton: .left)!.post(tap: .cghidEventTap)
    CGEvent(mouseEventSource: src, mouseType: .leftMouseUp, mouseCursorPosition: c, mouseButton: .left)!.post(tap: .cghidEventTap) }
    sleepMs(800)
    cap.setRect(r); sleepMs(300)
    var lat: [Double] = [], latPts: [Double] = [], missed = 0, dbg: [[Double]] = []
    for _ in 0..<probes {
      cap.clear(); let gap = Double(ProcessInfo.processInfo.environment["MLAT_GAP"] ?? "100")!; sleepMs(Double.random(in: gap * 0.6...gap * 1.4))
      let snap = cap.snapshot(); let ref = snap.last?.sum
      let tk = now(); if trace { FileHandle.standardError.write("key t=\(tk)\n".data(using: .utf8)!) }
      CGEvent(keyboardEventSource: src, virtualKey: 7, keyDown: true)!.post(tap: .cghidEventTap)
      CGEvent(keyboardEventSource: src, virtualKey: 7, keyDown: false)!.post(tap: .cghidEventTap)
      let e2 = now() + 1.0; var got: Double? = nil
      while now() < e2 && got == nil {
        if let f = cap.snapshot().first(where: { $0.arrival > tk && (ref == nil || $0.sum != ref!) }) { got = f.arrival; latPts.append((f.pts - tk) * 1000) }
        else { sleepMs(0.5) }
      }
      if ProcessInfo.processInfo.environment["MLAT_TRACE"] != nil { FileHandle.standardError.write("probe \(got == nil ? "MISS" : "ok") frames_before=\(snap.count)\n".data(using: .utf8)!) }
      if let g = got { lat.append((g - tk) * 1000) } else { missed += 1; dbg.append(cap.snapshot().map { ($0.arrival - tk) * 1000 }) }
      sleepMs(30)
    }
    row["latency_ms"] = lat; row["latency_pts_ms"] = latPts; row["missed"] = missed; row["missed_frames_dbg"] = Array(dbg.prefix(5)); row["ref_nil"] = 0
    let s = lat.sorted(); if !s.isEmpty { row["p50"] = s[s.count / 2]; row["p99"] = s[min(s.count - 1, Int(Double(s.count) * 0.99))] }
    print("run \(run): p50 \(row["p50"] ?? "-") p99 \(row["p99"] ?? "-") missed \(missed) n \(lat.count)")
  } else { print("run \(run): mapped \(mapped ?? -1) painted \(row["painted_ms"] ?? "-")") }
  results.append(row)
  killApp(p)
}
hogs.forEach { $0.terminate() }
let data = try! JSONSerialization.data(withJSONObject: results, options: [.prettyPrinted])
try! data.write(to: URL(fileURLWithPath: outPath))
stream.stopCapture { _ in }
exit(0)
