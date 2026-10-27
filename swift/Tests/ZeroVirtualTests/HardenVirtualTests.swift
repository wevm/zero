import Testing
import ZeroTesting

@testable import ZeroVirtual

// Exit tests run on macOS and Linux only, not in the iOS simulator.
#if os(macOS) || os(Linux)
  /// Options that trap the virtualizer. Each runs in a child process (an exit
  /// test), so a trap fails only its own test.
  @Suite struct HardenVirtualTests {
    /// `minPageSize: .max`, a way to say "one page holds everything". The
    /// page size is rounded up to an even number with `n + 1`, which
    /// overflows.
    @Test func hugeMinPageSize() async {
      await #expect(processExitsWith: .success) {
        await MainActor.run {
          var options = VirtualizerCore<String, TestRow, String, TestRow>.Options(
            estimatedRowHeight: 48, rowKey: \.id, toStartRow: { $0 }, listContext: "ctx")
          options.minPageSize = .max
          _ = VirtualizerCore(options, scheduler: ManualScheduler())
        }
      }
    }
  }
#endif
