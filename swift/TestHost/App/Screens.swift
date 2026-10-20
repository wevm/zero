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
      // Off the safe area's edges, the scroll view's frame is its viewport,
      // which the UI tests measure rows against.
      Divider()
      VirtualList(list) { item in
        MessageRow(message: item.row)
      }
      .accessibilityIdentifier("list")
      Divider()
      HStack {
        Button("Receive") { backend.receive() }.accessibilityIdentifier("receive")
        Button("Grow last") { backend.growLast() }.accessibilityIdentifier("grow")
        Button("Latest") { toLatest() }
          .accessibilityIdentifier("latest")
      }
      .padding(8)
    }
    .overlay(alignment: .topTrailing) { ProbeSummary() }
    .onChange(of: latest?.id, initial: true) {
      guard !started, let id = latest?.id else { return }
      started = true
      list.scrollToItem(id, align: .end)
    }
    .task {
      guard FrameProbe.script == "chat" else { return }
      await ProbeScript.chat(
        latestID: { latest?.id }, toLatest: toLatest, receive: { backend.receive() })
    }
  }

  private func toLatest() {
    guard let latest else { return }
    FrameProbe.shared.beginJump()
    list.scrollToItem(latest.id, align: .end)
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
    // One element with the padded row's frame: without the shape, it's
    // its texts'.
    .contentShape(.accessibility, Rectangle())
    .accessibilityElement(children: .ignore)
    .accessibilityLabel(message.text)
    .accessibilityIdentifier("row-\(message.id)")
    .background(ProbeMarker(id: message.id))
  }
}

/// A long list to page through, both ways.
struct IssuesScreen: View {
  @State private var list: IssueList

  init(backend: Backend) {
    _list = State(initialValue: backend.issueList())
  }

  var body: some View {
    VStack(spacing: 0) {
      Divider()
      issues
      Divider()
    }
    .overlay(alignment: .topTrailing) { ProbeSummary() }
    .task {
      guard FrameProbe.script == "issues" else { return }
      await ProbeScript.issues()
    }
  }

  private var issues: some View {
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
      .contentShape(.accessibility, Rectangle())
      .accessibilityElement(children: .ignore)
      .accessibilityLabel(item.row.title)
      .accessibilityIdentifier("row-\(item.row.id)")
      .background(ProbeMarker(id: item.row.id))
    }
    .accessibilityIdentifier("list")
  }
}
