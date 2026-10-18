// swift-tools-version: 6.2
// DDCKit: the app's logic that runs without AppKit, so `swift test` covers it with
// no app host carrying the bundle id (ADR "Menu bar app: Swift shell over the ddc CLI").
// Foundation only; the CLI it drives is the one engine.
import PackageDescription

let package = Package(
    name: "DDCKit",
    platforms: [.macOS("26.0")],
    products: [
        .library(name: "DDCKit", targets: ["DDCKit"])
    ],
    targets: [
        .target(name: "DDCKit"),
        .testTarget(
            name: "DDCKitTests",
            dependencies: ["DDCKit"],
            resources: [.copy("Fixtures")]
        ),
    ]
)
