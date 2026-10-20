import SwiftUI
import ZeroTesting
import ZeroUI
import ZeroVirtual

/// zero-virtual's chat demo (`demo/chat`): it opens at the latest message
/// and follows new ones while it's at the bottom.
struct ChatScreen: View {
  let backend: Backend
  @State private var list: ChatList
  @ZeroQuery(ChatQueries.latest()) private var latest
  @State private var started = false

  init(backend: Backend) {
    self.backend = backend
    _list = State(initialValue: backend.chatList())
  }

  var body: some View {
    VStack(spacing: 0) {
      VirtualList(list) { item in
        MessageRow(message: item.row)
      }
      .accessibilityIdentifier("list")
      HStack {
        Button("Receive") { backend.receive() }.accessibilityIdentifier("receive")
        Button("Grow last") { backend.growLast() }.accessibilityIdentifier("grow")
        Button("Latest") {
          if let latest { list.scrollToItem(latest.id, align: .end) }
        }
        .accessibilityIdentifier("latest")
      }
      .padding(8)
    }
    .onChange(of: latest?.id, initial: true) {
      guard !started, let id = latest?.id else { return }
      started = true
      list.scrollToItem(id, align: .end)
    }
  }
}

struct MessageRow: View {
  let message: TestComment

  var body: some View {
    VStack(alignment: .leading, spacing: 2) {
      Text(message.authorId).font(.caption).foregroundStyle(.secondary)
      Text(message.text)
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(.horizontal, 12)
    .padding(.vertical, 6)
    .accessibilityElement(children: .combine)
    .accessibilityIdentifier("row-\(message.id)")
  }
}

/// A long list to page through, both ways.
struct IssuesScreen: View {
  @State private var list: IssueList

  init(backend: Backend) {
    _list = State(initialValue: backend.issueList())
  }

  var body: some View {
    VirtualList(list) { item in
      VStack(alignment: .leading, spacing: 2) {
        Text(item.row.title)
        if !item.row.description.isEmpty {
          Text(item.row.description).font(.caption).foregroundStyle(.secondary)
        }
      }
      .frame(maxWidth: .infinity, alignment: .leading)
      .padding(.horizontal, 12)
      .padding(.vertical, 8)
      .accessibilityElement(children: .combine)
      .accessibilityIdentifier("row-\(item.row.id)")
    }
    .accessibilityIdentifier("list")
  }
}
