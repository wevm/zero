import Foundation

/// The zero-cache address as the client takes it. zbugs' backend prints it
/// as `ws://`, and the client only takes `http://` and `https://`, so `ws://`
/// becomes `http://` and `wss://` becomes `https://`. Nil for any other
/// scheme, or no host: `cacheURLError` says why.
public func cacheClientURL(_ url: String) -> String? {
  let url = url.trimmingCharacters(in: .whitespacesAndNewlines)
  guard let separator = url.range(of: "://") else { return nil }
  let rest = url[separator.upperBound...]
  guard let first = rest.first, !"/?#".contains(first) else { return nil }
  switch url[..<separator.lowerBound].lowercased() {
  case "http", "ws": return "http://" + rest
  case "https", "wss": return "https://" + rest
  default: return nil
  }
}

/// Why the client can't take `url` as the zero-cache address, or nil when it
/// can.
public func cacheURLError(_ url: String) -> String? {
  cacheClientURL(url) == nil
    ? "\"\(url)\" isn't a zero-cache address. Use an http://, https://, ws://, or wss:// URL, such as http://127.0.0.1:4848."
    : nil
}
