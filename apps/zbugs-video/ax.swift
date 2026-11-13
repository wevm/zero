// The record script's eyes and hands beyond cliclick, through the macOS
// accessibility API (the caller needs the Accessibility permission):
//   ax permissions               checks (and asks for) Accessibility and
//                                Screen Recording
//   ax screen                    the main screen's usable area: "x y w h"
//   ax scroll DY STEPS           a fast wheel swipe at the pointer
//   ax dump PID [DEPTH]          an app's tree: role, labels, frame
//   ax find PID QUERY [INDEX]    the INDEXth match's frame: "x y w h"
//   ax show PID QUERY [INDEX]    the same, scrolled into view first
//   ax window PID                the app's first window: "x y w h"
//   ax place PID X Y W H         moves and sizes that window
//   ax text PID [TEXT [INDEX]]   the INDEXth line of text on screen in the
//                                app's window containing TEXT (`=TEXT`:
//                                that whole line): "x y w h";
//                                without TEXT, every line and its frame
//                                (Vision's text recognition, for windows
//                                that expose no controls)
//   ax caption PNG W H TITLE DETAIL  a W×H-pixel caption band
// QUERY is `field=text` pairs joined by `&` (role, title, desc, id,
// placeholder, value, help), each a case-insensitive substring match
// (`field==text` for the whole label), or bare text matched against every
// label. Points are top-left-origin screen points.
import AppKit
import ApplicationServices
import Foundation
import Vision

func attr(_ e: AXUIElement, _ name: String) -> AnyObject? {
  var v: AnyObject?
  return AXUIElementCopyAttributeValue(e, name as CFString, &v) == .success ? v : nil
}
func str(_ e: AXUIElement, _ name: String) -> String {
  guard let v = attr(e, name) else { return "" }
  if let s = v as? String { return s }
  if let n = v as? NSNumber { return n.stringValue }
  return ""
}
func frame(_ e: AXUIElement) -> CGRect? {
  guard let p = attr(e, kAXPositionAttribute), let s = attr(e, kAXSizeAttribute) else { return nil }
  var pt = CGPoint.zero, sz = CGSize.zero
  AXValueGetValue(p as! AXValue, .cgPoint, &pt)
  AXValueGetValue(s as! AXValue, .cgSize, &sz)
  return CGRect(origin: pt, size: sz)
}
func children(_ e: AXUIElement) -> [AXUIElement] {
  (attr(e, kAXChildrenAttribute) as? [AXUIElement]) ?? []
}
func labels(_ e: AXUIElement) -> [String: String] {
  [
    "role": str(e, kAXRoleAttribute), "title": str(e, kAXTitleAttribute),
    "desc": str(e, kAXDescriptionAttribute), "id": str(e, "AXIdentifier"),
    "placeholder": str(e, kAXPlaceholderValueAttribute),
    "value": String(str(e, kAXValueAttribute).prefix(80)), "help": str(e, kAXHelpAttribute),
  ]
}

let args = CommandLine.arguments
let usage = """
  usage: ax permissions | ax screen | ax scroll DY STEPS | ax dump PID [DEPTH]
         ax find|show PID QUERY [INDEX] | ax text PID [TEXT [INDEX]]
         ax window PID | ax place PID X Y W H
         ax caption PNG W H TITLE DETAIL

  """

switch args.count > 1 ? args[1] : "" {
case "permissions":
  // Whether this process, and so the app that runs it, may use the
  // accessibility API and capture the screen; asks for each it may not.
  let accessibility = AXIsProcessTrustedWithOptions(
    [kAXTrustedCheckOptionPrompt.takeUnretainedValue(): true] as CFDictionary)
  let screen = CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess()
  print("accessibility \(accessibility ? "granted" : "missing")")
  print("screen-recording \(screen ? "granted" : "missing")")
  exit(accessibility && screen ? 0 : 1)
case "screen":
  // The main screen's usable area (below the menu bar, beside the Dock), in
  // the top-left-origin points the other commands use.
  let s = NSScreen.screens[0]
  let v = s.visibleFrame
  print("\(Int(v.minX)) \(Int(s.frame.height - v.maxY)) \(Int(v.width)) \(Int(v.height))")
  exit(0)
case "scroll":
  // A fast swipe at the pointer: STEPS pixel-precise wheel events of DY
  // points each (negative scrolls down), one every 8 ms.
  guard args.count == 4, let dy = Int32(args[2]), let steps = Int(args[3]) else { break }
  for _ in 0..<steps {
    CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: dy, wheel2: 0, wheel3: 0)?
      .post(tap: .cghidEventTap)
    usleep(8000)
  }
  exit(0)
case "caption":
  // White title and dimmer detail on zbugs' dark gray, sized to the band.
  guard args.count == 7, let w = Int(args[3]), let h = Int(args[4]) else { break }
  // Drawn in pixels, whatever the screen's scale.
  guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: w, pixelsHigh: h, bitsPerSample: 8, samplesPerPixel: 4,
    hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
  else { exit(1) }
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
  NSColor(red: 0x1c / 255, green: 0x1c / 255, blue: 0x1e / 255, alpha: 1).setFill()
  NSRect(x: 0, y: 0, width: w, height: h).fill()
  let x = CGFloat(h) * 0.35
  for (text, size, alpha, top) in [
    (args[5], 0.34, 1.0, 0.16), (args[6], 0.24, 0.8, 0.58),
  ] {
    let font = NSFont.systemFont(ofSize: CGFloat(h) * size, weight: alpha == 1 ? .medium : .regular)
    let attrs: [NSAttributedString.Key: Any] = [
      .font: font, .foregroundColor: NSColor.white.withAlphaComponent(alpha),
    ]
    // `top` is the line's top from the band's top, as a share of its height.
    let y = CGFloat(h) * (1 - top) - font.ascender
    (text as NSString).draw(at: NSPoint(x: x, y: y), withAttributes: attrs)
  }
  NSGraphicsContext.restoreGraphicsState()
  guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
  try? png.write(to: URL(fileURLWithPath: args[2]))
  exit(0)
default: break
}

guard args.count >= 3, let pid = pid_t(args[2]) else {
  FileHandle.standardError.write(usage.data(using: .utf8)!)
  exit(2)
}
let app = AXUIElementCreateApplication(pid)

func mainWindow() -> AXUIElement? {
  (attr(app, kAXWindowsAttribute) as? [AXUIElement])?.first
}

switch args[1] {
case "window":
  guard let w = mainWindow(), let f = frame(w) else { exit(1) }
  print("\(Int(f.minX)) \(Int(f.minY)) \(Int(f.width)) \(Int(f.height))")
  exit(0)
case "place":
  guard args.count == 7, let w = mainWindow(),
    let x = Double(args[3]), let y = Double(args[4]), let width = Double(args[5]), let height = Double(args[6])
  else { exit(2) }
  var size = CGSize(width: width, height: height), origin = CGPoint(x: x, y: y)
  AXUIElementPerformAction(w, kAXRaiseAction as CFString)
  // Size, then position, then size again: a window can't grow past the
  // screen's edge from where it was.
  AXUIElementSetAttributeValue(w, kAXSizeAttribute as CFString, AXValueCreate(.cgSize, &size)!)
  AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, AXValueCreate(.cgPoint, &origin)!)
  AXUIElementSetAttributeValue(w, kAXSizeAttribute as CFString, AXValueCreate(.cgSize, &size)!)
  if let f = frame(w) { print("\(Int(f.minX)) \(Int(f.minY)) \(Int(f.width)) \(Int(f.height))") }
  exit(0)
default: break
}

if args[1] == "text" {
  // What's on screen in the window, read by Vision: the window must be
  // unobscured, and the caller needs Screen Recording.
  guard let w = mainWindow(), let f = frame(w) else { exit(1) }
  let file = NSTemporaryDirectory() + "ax-text-\(getpid()).png"
  let capture = Process()
  capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
  capture.arguments = ["-x", "-R\(Int(f.minX)),\(Int(f.minY)),\(Int(f.width)),\(Int(f.height))", file]
  try? capture.run()
  capture.waitUntilExit()
  defer { try? FileManager.default.removeItem(atPath: file) }
  guard let image = NSImage(contentsOfFile: file)?.cgImage(forProposedRect: nil, context: nil, hints: nil)
  else { exit(1) }
  let request = VNRecognizeTextRequest()
  request.recognitionLevel = .accurate
  do {
    try VNImageRequestHandler(cgImage: image).perform([request])
  } catch {
    // A Mac without a Neural Engine, such as a virtual one, has only the
    // fast recognizer.
    request.recognitionLevel = .fast
    try? VNImageRequestHandler(cgImage: image).perform([request])
  }
  // `=TEXT` matches a whole line.
  let wanted = args.count > 3 ? args[3].lowercased() : nil
  let whole = wanted?.hasPrefix("=") ?? false
  let needle = whole ? String(wanted!.dropFirst()) : wanted
  var skip = args.count > 4 ? Int(args[4]) ?? 0 : 0
  // Top to bottom, then left to right, as a reader would.
  let lines = (request.results ?? []).compactMap { o -> (String, CGRect)? in
    guard let text = o.topCandidates(1).first?.string else { return nil }
    let b = o.boundingBox
    return (text, CGRect(
      x: f.minX + b.minX * f.width, y: f.minY + (1 - b.maxY) * f.height,
      width: b.width * f.width, height: b.height * f.height))
  }.sorted { ($0.1.minY, $0.1.minX) < ($1.1.minY, $1.1.minX) }
  for (text, r) in lines {
    let at = "\(Int(r.minX)) \(Int(r.minY)) \(Int(r.width)) \(Int(r.height))"
    guard let wanted else {
      print("\(at) \(text)")
      continue
    }
    let line = text.lowercased()
    if whole ? line == needle : line.contains(needle!) {
      if skip == 0 {
        print(at)
        exit(0)
      }
      skip -= 1
    }
  }
  exit(wanted == nil ? 0 : 1)
}

if args[1] == "dump" {
  let maxDepth = args.count > 3 ? Int(args[3]) ?? 40 : 40
  func walk(_ e: AXUIElement, _ depth: Int) {
    let l = labels(e)
    let f = frame(e).map { "\(Int($0.minX)),\(Int($0.minY)) \(Int($0.width))x\(Int($0.height))" } ?? "-"
    let text = ["title", "desc", "id", "placeholder", "value", "help"].compactMap { k in
      l[k]!.isEmpty ? nil : "\(k)=\(l[k]!.replacingOccurrences(of: "\n", with: " "))"
    }.joined(separator: " ")
    print(String(repeating: "  ", count: depth) + "\(l["role"]!) [\(f)] \(text)")
    if depth < maxDepth { children(e).forEach { walk($0, depth + 1) } }
  }
  walk(app, 0)
  exit(0)
}

let query = args[3]
let wanted = args.count > 4 ? Int(args[4]) ?? 0 : 0
let pairs: [(String?, String)] = query.split(separator: "&").map { part in
  let kv = part.split(separator: "=", maxSplits: 1).map(String.init)
  return kv.count == 2 ? (kv[0], kv[1].lowercased()) : (nil, kv[0].lowercased())
}
var found = 0
var match: AXUIElement?
func search(_ e: AXUIElement) -> CGRect? {
  let l = labels(e)
  let ok = pairs.allSatisfy { key, text in
    // `field==text` matches the whole label.
    if let key, text.hasPrefix("=") { return l[key]?.lowercased() == String(text.dropFirst()) }
    if let key { return l[key]?.lowercased().contains(text) ?? false }
    return l.contains { $0.key != "role" && $0.value.lowercased().contains(text) }
  }
  // `show` takes elements scrolled out of view, which have no height.
  if ok, let f = frame(e), f.width > 0, f.height > 0 || args[1] == "show" {
    if found == wanted {
      match = e
      return f
    }
    found += 1
  }
  for c in children(e) { if let f = search(c) { return f } }
  return nil
}
guard var f = search(app) else { exit(1) }
if args[1] == "show", let e = match {
  // Scrolled into view, as a web page's controls below the fold.
  AXUIElementPerformAction(e, "AXScrollToVisible" as CFString)
  usleep(400_000)
  guard let shown = frame(e), shown.height > 0 else { exit(1) }
  f = shown
}
print("\(Int(f.minX)) \(Int(f.minY)) \(Int(f.width)) \(Int(f.height))")
