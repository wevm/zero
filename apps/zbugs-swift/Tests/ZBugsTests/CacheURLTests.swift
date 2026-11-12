import Testing
import ZBugs

@Suite struct CacheURLTests {
  @Test func takesWebSocketSchemesAsTheirHTTPOnes() {
    #expect(cacheClientURL("ws://127.0.0.1:4848") == "http://127.0.0.1:4848")
    #expect(cacheClientURL(" WSS://zero.example/x ") == "https://zero.example/x")
    #expect(cacheClientURL("http://h:1/p") == "http://h:1/p")
    #expect(cacheClientURL("https://h") == "https://h")
  }

  @Test func rejectsOtherSchemesAndMissingHosts() {
    for bad in ["", "127.0.0.1:4848", "ftp://h", "ws://", "http:///p", "wsx://h"] {
      #expect(cacheClientURL(bad) == nil, "\(bad)")
      #expect(cacheURLError(bad) != nil, "\(bad)")
    }
    #expect(cacheURLError("ws://127.0.0.1:4848") == nil)
  }
}
