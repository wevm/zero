// The record script's eyes and hands beyond cliclick, through the macOS
// accessibility API (the caller needs the Accessibility permission):
//   ax screen                    the main screen's usable area: "x y w h"
//   ax scroll DY STEPS           a fast wheel swipe at the pointer
//   ax dump PID [DEPTH]          an app's tree: role, labels, frame
//   ax find PID QUERY [INDEX]    the INDEXth match's frame: "x y w h"
//   ax window PID                the app's first window: "x y w h"
//   ax place PID X Y W H         moves and sizes that window
// QUERY is `field=text` pairs joined by `&` (role, title, desc, id,
// placeholder, value, help), each a case-insensitive substring match, or bare
// text matched against every label. Points are top-left-origin screen points.
import AppKit
import ApplicationServices
import Foundation

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
  usage: ax screen | ax scroll DY STEPS | ax dump PID [DEPTH] | ax find PID QUERY [INDEX]
         ax window PID | ax place PID X Y W H

  """

switch args.count > 1 ? args[1] : "" {
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
func search(_ e: AXUIElement) -> CGRect? {
  let l = labels(e)
  let ok = pairs.allSatisfy { key, text in
    if let key { return l[key]?.lowercased().contains(text) ?? false }
    return l.contains { $0.key != "role" && $0.value.lowercased().contains(text) }
  }
  if ok, let f = frame(e), f.width > 0, f.height > 0 {
    if found == wanted { return f }
    found += 1
  }
  for c in children(e) { if let f = search(c) { return f } }
  return nil
}
guard let f = search(app) else { exit(1) }
print("\(Int(f.minX)) \(Int(f.minY)) \(Int(f.width)) \(Int(f.height))")
