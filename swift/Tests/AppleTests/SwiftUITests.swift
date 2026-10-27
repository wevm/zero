#if canImport(SwiftUI)
  import Foundation
  import SwiftUI
  import Testing
  import ZeroTesting

  @testable import ZeroUI

  @testable import Zero

  #if canImport(UIKit)
    import UIKit
  #else
    import AppKit
  #endif

  @Suite @MainActor struct SwiftUITests {
    final class Seen {
      var texts: [[String]] = []
    }

    struct NotesView: View {
      @ZeroQuery(Notes.all()) var notes
      let seen: Seen

      var body: some View {
        let _ = seen.texts.append(notes.map(\.text))
        Text(notes.map(\.text).joined(separator: ","))
      }
    }

    /// Lays the view out and lets SwiftUI process its updates.
    func render(_ view: some View) -> AnyObject {
      #if canImport(UIKit)
        let host = UIHostingController(rootView: view)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 200, height: 200))
        window.rootViewController = host
        window.makeKeyAndVisible()
        host.view.layoutIfNeeded()
        return window
      #else
        let host = NSHostingView(rootView: view)
        host.frame = CGRect(x: 0, y: 0, width: 200, height: 200)
        host.layoutSubtreeIfNeeded()
        return host
      #endif
    }

    func settle(_ host: AnyObject) {
      RunLoop.main.run(until: Date().addingTimeInterval(0.1))
      #if canImport(UIKit)
        (host as? UIWindow)?.layoutIfNeeded()
      #else
        (host as? NSView)?.layoutSubtreeIfNeeded()
      #endif
    }

    @Test func zeroQueryFollowsTheRowsOfItsQuery() throws {
      var options = ZeroOptions(userID: "u", storageKey: randomID(), storage: .memory)
      options.logSink = RecordingLogSink()
      let zero = try Zero(
        schema: .notes, mutators: [Notes.put], options: options,
        scheduler: ManualScheduler(now: 0), socketFactory: nil)
      zero.mutate(Notes.put(.init(id: "a", text: "1")))

      let seen = Seen()
      let host = render(NotesView(seen: seen).zero(zero))
      settle(host)
      #expect(seen.texts.last == ["1"])

      zero.mutate(Notes.put(.init(id: "b", text: "2")))
      settle(host)
      #expect(seen.texts.last == ["1", "2"])
    }

    /// A client freed, then a new one made at its address (as `zero = nil` and then a new client, in one update, can do): `@ZeroQuery` reads the new one, not the closed one's last rows.
    @Test func zeroQueryFollowsANewClientAtAFreedOnesAddress() throws {
      func makeZero(_ text: String) throws -> Zero<Void> {
        var options = ZeroOptions(userID: "u", storageKey: randomID(), storage: .memory)
        options.logSink = RecordingLogSink()
        let zero = try Zero(
          schema: .notes, mutators: [Notes.put], options: options,
          scheduler: ManualScheduler(now: 0), socketFactory: nil)
        zero.mutate(Notes.put(.init(id: "a", text: text)))
        return zero
      }
      let state = QueryState<[Note]>()
      // On macOS each new client lands at the freed one's address.
      for i in 0..<20 {
        var old: Zero<Void>? = try makeZero("old \(i)")
        state.update(old, Notes.all(), .defaultTTL)
        let id = ObjectIdentifier(old!)
        old = nil
        let new = try makeZero("new \(i)")
        state.update(new, Notes.all(), .defaultTTL)
        #expect(
          state.live?.data.map(\.text) == ["new \(i)"], "reused: \(ObjectIdentifier(new) == id)")
      }
    }
  }
#endif
