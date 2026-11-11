#if canImport(SwiftUI)
  import Foundation
  import SwiftUI

  /// Markdown as zbugs shows it, with GitHub's syntax: paragraphs, headings,
  /// lists, quotes, code blocks, tables, and images from the web. `Text`
  /// shows only inline Markdown, so each block Foundation finds is a view of
  /// its own.
  struct MarkdownView: View {
    private let blocks: [MarkdownBlock]

    init(_ text: String) {
      blocks = MarkdownBlock.parse(text)
    }

    var body: some View {
      VStack(alignment: .leading, spacing: 0) {
        ForEach(blocks.indices, id: \.self) { i in
          MarkdownBlockView(block: blocks[i])
            .padding(.top, i == 0 ? 0 : blocks[i].spacing(after: blocks[i - 1]))
        }
      }
      .frame(maxWidth: .infinity, alignment: .leading)
      .textSelection(.enabled)
    }
  }

  /// A block of Markdown, where lists and quotes put it.
  private struct MarkdownBlock {
    enum Content {
      /// A paragraph, or a heading of a level from 1.
      case text(AttributedString, heading: Int?)
      case image(URL, alt: String)
      case code(String)
      case rule
      /// The rows, the header first.
      case table([[AttributedString]], id: Int)
    }

    var content: Content
    /// How deeply lists nest it.
    var depth = 0
    /// Its list item's bullet or number, on the item's first block.
    var marker: String?
    var quoted = false
    /// The outermost list it's in.
    var list: Int?

    /// The space above it: less between the items of a list.
    func spacing(after previous: MarkdownBlock) -> CGFloat {
      list != nil && list == previous.list ? 4 : 12
    }

    static func parse(_ text: String) -> [MarkdownBlock] {
      guard
        let document = try? AttributedString(
          markdown: text,
          options: .init(
            interpretedSyntax: .full, failurePolicy: .returnPartiallyParsedIfPossible))
      else { return [MarkdownBlock(content: .text(AttributedString(text), heading: nil))] }

      var blocks: [MarkdownBlock] = []
      var markedItems: Set<Int> = []
      var lastRow: Int?
      // The text by the block it's in. A block's components are it, then
      // the blocks around it, innermost first.
      for (intent, range) in document.runs[\.presentationIntent] {
        let components = intent?.components ?? []
        let slice = AttributedString(document[range])
        var block = MarkdownBlock(content: .rule)
        for (i, component) in components.enumerated() {
          switch component.kind {
          case .orderedList, .unorderedList:
            block.depth += 1
            block.list = component.identity
          case .listItem(let ordinal):
            if block.marker == nil, markedItems.insert(component.identity).inserted {
              let ordered = components.dropFirst(i + 1).first?.kind == .orderedList
              block.marker = ordered ? "\(ordinal)." : "•"
            }
          case .blockQuote:
            block.quoted = true
          default:
            break
          }
        }
        func add(_ content: Content) {
          block.content = content
          blocks.append(block)
          block.marker = nil
        }

        switch components.first?.kind {
        case .codeBlock:
          var code = String(slice.characters)
          while code.hasSuffix("\n") { code.removeLast() }
          add(.code(code))
        case .thematicBreak:
          add(.rule)
        case .tableCell:
          // A cell, then its row, then its table. Cells come row by row.
          let row = components.dropFirst().first?.identity
          let table = components.dropFirst(2).first?.identity ?? 0
          if case .table(var rows, let id)? = blocks.last?.content, id == table {
            if row != lastRow { rows.append([]) }
            rows[rows.count - 1].append(slice)
            blocks[blocks.count - 1].content = .table(rows, id: id)
          } else {
            add(.table([[slice]], id: table))
          }
          lastRow = row
        default:
          var heading: Int?
          if case .header(let level)? = components.first?.kind { heading = level }
          // Each image on its own, between the text before and after it.
          var start = slice.startIndex
          func text(to end: AttributedString.Index) {
            let part = trimmed(AttributedString(slice[start..<end]))
            if !part.characters.isEmpty { add(.text(part, heading: heading)) }
          }
          for (url, run) in slice.runs[\.imageURL] {
            guard let url else { continue }
            text(to: run.lowerBound)
            add(.image(url, alt: String(slice[run].characters)))
            start = run.upperBound
          }
          text(to: slice.endIndex)
        }
      }
      return blocks
    }
  }

  /// `s` without the whitespace at its ends.
  private func trimmed(_ s: AttributedString) -> AttributedString {
    let characters = s.characters
    guard let first = characters.firstIndex(where: { !$0.isWhitespace }),
      let last = characters.lastIndex(where: { !$0.isWhitespace })
    else { return AttributedString() }
    return AttributedString(s[first...last])
  }

  private struct MarkdownBlockView: View {
    let block: MarkdownBlock

    var body: some View {
      let row = HStack(alignment: .firstTextBaseline, spacing: 6) {
        if block.depth > 0 {
          Text(block.marker ?? "")
            .monospacedDigit()
            .foregroundStyle(.secondary)
            .frame(minWidth: 18, alignment: .trailing)
        }
        content
      }
      .padding(.leading, CGFloat(max(block.depth - 1, 0)) * 24)
      if block.quoted {
        row
          .padding(.leading, 12)
          .overlay(alignment: .leading) { Capsule().fill(.tertiary).frame(width: 3) }
          .foregroundStyle(.secondary)
      } else {
        row
      }
    }

    @ViewBuilder private var content: some View {
      switch block.content {
      case .text(let text, let heading):
        Text(text).font(heading.map(Self.font) ?? .body)
      case .image(let url, let alt):
        MarkdownImage(url: url, alt: alt)
      case .code(let code):
        let text = Text(code).font(.system(.callout, design: .monospaced)).padding(10)
        Group {
          #if os(macOS)
            // Long lines wrap: a Mac's horizontal ScrollView keeps the scroll
            // wheel's vertical scrolling from the page.
            text
          #else
            // Long lines scroll, as zbugs' do.
            ScrollView(.horizontal) { text.fixedSize() }
          #endif
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 6))
      case .rule:
        // Across, not down the row as in an HStack.
        VStack { Divider() }
      case .table(let rows, _):
        Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
          ForEach(rows.indices, id: \.self) { r in
            GridRow {
              ForEach(rows[r].indices, id: \.self) { c in
                Text(rows[r][c]).bold(r == 0)
              }
            }
            if r == 0 { Divider() }
          }
        }
      }
    }

    private static func font(_ level: Int) -> Font {
      switch level {
      case 1: .title2.bold()
      case 2: .title3.bold()
      case 3: .headline
      default: .subheadline.bold()
      }
    }
  }

  /// An image from the web at its own size, or narrower to fit. Until it
  /// arrives, a placeholder; when it can't be shown, a link to it.
  private struct MarkdownImage: View {
    let url: URL
    let alt: String
    @State private var image: WebImages.Loaded?
    @State private var failed = false

    init(url: URL, alt: String) {
      self.url = url
      self.alt = alt
      // A row scrolled back into view shows it at once, at its height.
      _image = State(initialValue: WebImages.cached(url))
    }

    var body: some View {
      Group {
        if let image {
          image.image
            .resizable()
            .aspectRatio(image.size, contentMode: .fit)
            .frame(maxWidth: image.size.width)
            .accessibilityLabel(alt)
        } else if failed {
          Link(alt.isEmpty ? url.absoluteString : alt, destination: url)
        } else {
          RoundedRectangle(cornerRadius: 6)
            .fill(.quaternary.opacity(0.6))
            .frame(width: 240, height: 120)
            .overlay { ProgressView().controlSize(.small) }
        }
      }
      .task(id: url) {
        guard image == nil else { return }
        image = await WebImages.load(url)
        failed = image == nil
      }
    }
  }

  /// Images from the web, each fetched once and kept while there's room.
  @MainActor
  private enum WebImages {
    struct Loaded {
      let image: Image
      let size: CGSize
    }

    /// Wraps a `Loaded` for `NSCache`.
    private final class Entry {
      let loaded: Loaded
      init(_ loaded: Loaded) { self.loaded = loaded }
    }

    private static let cache: NSCache<NSURL, Entry> = {
      let cache = NSCache<NSURL, Entry>()
      cache.countLimit = 100
      return cache
    }()
    private static var loading: [URL: Task<Loaded?, Never>] = [:]

    static func cached(_ url: URL) -> Loaded? {
      cache.object(forKey: url as NSURL)?.loaded
    }

    /// The image at `url`, or nil when it isn't one. A failure isn't kept,
    /// so the next view of it tries again.
    static func load(_ url: URL) async -> Loaded? {
      if let loaded = cached(url) { return loaded }
      let task =
        loading[url]
        ?? Task {
          defer { loading[url] = nil }
          guard ["http", "https"].contains(url.scheme?.lowercased()),
            let fetched = try? await URLSession.shared.data(from: url),
            (fetched.1 as? HTTPURLResponse)?.statusCode == 200,
            let loaded = decode(fetched.0)
          else { return nil }
          cache.setObject(Entry(loaded), forKey: url as NSURL)
          return loaded
        }
      loading[url] = task
      return await task.value
    }

    private static func decode(_ data: Data) -> Loaded? {
      #if os(macOS)
        guard let image = NSImage(data: data), image.size.width > 0 else { return nil }
        return Loaded(image: Image(nsImage: image), size: image.size)
      #else
        guard let image = UIImage(data: data), image.size.width > 0 else { return nil }
        return Loaded(image: Image(uiImage: image), size: image.size)
      #endif
    }
  }
#endif
