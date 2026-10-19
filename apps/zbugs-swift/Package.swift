// swift-tools-version: 6.2
import PackageDescription

/// zbugs on the Zero Swift client: the schema, queries, and mutators of
/// apps/zbugs/shared, ported to Swift and checked against the TS ones.
let package = Package(
  name: "ZBugs",
  platforms: [.iOS(.v26), .macOS(.v26)],
  products: [
    .library(name: "ZBugs", targets: ["ZBugs"])
  ],
  dependencies: [
    .package(name: "Zero", path: "../../swift")
  ],
  targets: [
    .target(name: "ZBugs", dependencies: [.product(name: "Zero", package: "Zero")]),
    .testTarget(
      name: "ZBugsTests",
      dependencies: [
        "ZBugs",
        .product(name: "Zero", package: "Zero"),
        .product(name: "ZeroTesting", package: "Zero"),
      ],
      exclude: ["drift-vectors.json"]),
  ],
  swiftLanguageModes: [.v6]
)
