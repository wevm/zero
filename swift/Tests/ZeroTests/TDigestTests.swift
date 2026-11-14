import Testing

@testable import Zero

/// The TDigest port against JS's: every expected number is what
/// `packages/shared/src/tdigest.ts` returns for the same calls, run with
/// the repo's Node.
@Suite struct TDigestTests {
  @Test func answersAsJSDoesOnceItHasMergedItsValues() {
    // Enough values to merge the buffer, some with update metrics' weight.
    let d = TDigest()
    for i in 0..<1000 { d.add(Double((i * 7) % 101), weight: i % 3 == 0 ? 32 : 1) }
    #expect(
      [0, 0.25, 0.5, 0.95, 0.99, 1].map(d.quantile) == [
        0, 24.38504613033506, 49.76361353903839, 95.57283979178716, 100, 100,
      ])
    #expect([10, 50.5, 100].map(d.cdf) == [0.10489695261581822, 0.5090373478632793, 1])
    #expect(d.count() == 11354)
    let json = d.toJSON()
    #expect(json.count == 247)
    #expect(Array(json.prefix(7)) == [100, 0, 1, 0, 32, 0, 32])
  }

  @Test func mergesWhenReadAsJSDoes() {
    // Reading merges the buffer, which changes the centroids later values
    // land in, as in JS.
    let a = TDigest()
    let b = TDigest()
    for i in 0..<900 {
      a.add(Double(900 - i))
      b.add(Double(900 - i))
      if i == 450 { _ = b.quantile(0.5) }
    }
    #expect([a.quantile(0.9), a.quantile(0.1)] == [810.5, 90.5])
    #expect([b.quantile(0.9), b.quantile(0.1)] == [810.5, 90.5])
    #expect(a.toJSON().count == 237)
    #expect(b.toJSON().count == 255)
  }

  @Test func keepsFewerCentroidsWithALowerCompression() {
    let d = TDigest(compression: 20)
    for i in 0..<500 { d.add(Double(i)) }
    #expect(
      d.toJSON() == [
        20, 0, 1, 3.5, 6, 9.5, 6, 16, 7, 24, 9, 33.5, 10, 50, 23, 73.5, 24, 91.5, 12, 109, 23,
        130, 19, 152, 25, 177, 25, 201.5, 24, 225, 23, 247, 21, 274.5, 34, 309.5, 36, 344.5, 34,
        377, 31, 406, 27, 431, 23, 451.5, 18, 467, 13, 480.5, 14, 492, 9, 498, 3,
      ])
  }

  @Test func readsAndWritesJSsJSON() throws {
    let d = try TDigest(json: [100, 1, 1, 5, 2, 9, 3])
    #expect([d.quantile(0.5), d.quantile(0.95)] == [6.6, 9])
    #expect(d.cdf(5) == 0.3333333333333333)
    #expect(d.count() == 6)
    #expect(d.toJSON() == [100, 1, 1, 5, 2, 9, 3])
    let server = try TDigest(json: [100, 2, 3, 4, 1, 10, 2])
    #expect([server.quantile(0.5), server.quantile(0.95)] == [3.5, 10])
    // An even count isn't a digest's (`Invalid centroids array`).
    #expect(throws: TDigest.InvalidJSON.self) { try TDigest(json: [100, 1]) }
    #expect(throws: TDigest.InvalidJSON.self) { try TDigest(json: []) }
  }

  @Test func ignoresWhatJSIgnores() {
    let d = TDigest()
    d.add(.nan)
    d.add(1, weight: 0)
    d.add(1, weight: -1)
    d.add(1, weight: .infinity)
    #expect(d.count() == 0)
    #expect(d.quantile(0.5).isNaN)
    d.add(3)
    #expect(d.quantile(0.5) == 3)
    // One update sampled out of `UPDATE_SAMPLE_RATE`.
    let sampled = TDigest()
    sampled.add(0, weight: 32)
    #expect(sampled.count() == 32)
    #expect(sampled.quantile(0.5) == 0)
  }

  @Test func mergesItself() {
    let d = TDigest()
    for i in 0..<10 { d.add(Double(i * i)) }
    d.merge(d)
    #expect(d.count() == 20)
    #expect(
      d.toJSON() == [
        100, 0, 1, 0, 1, 1, 1, 1, 1, 4, 1, 4, 1, 9, 1, 9, 1, 16, 1, 16, 1, 25, 1, 25, 1, 36, 1, 36,
        1, 49, 1, 49, 1, 64, 1, 64, 1, 81, 1, 81, 1,
      ])
    #expect(d.quantile(0.75) == 49)
  }

  /// `Math.asin` and `Math.sin` as Node 22 computes them, on arguments
  /// where glibc's last bit differs.
  @Test func computesNodesTrigonometry() {
    #expect(fdlibmAsin(-0.8461538461538461) == -1.0087265237892695)
    #expect(fdlibmAsin(-0.5294117647058824) == -0.5579070399678958)
    #expect(fdlibmAsin(-0.8888888888888888) == -1.0949140771344799)
    #expect(fdlibmSin(0.7960981043985682) == 0.7146321601621326)
    #expect(fdlibmSin(0.8546279036617737) == 0.7543266873983547)
    #expect(fdlibmSin(-1.0340198899748416) == -0.8593615719698496)
    #expect(fdlibmSin(1) == 0.8414709848078965)
  }
}
