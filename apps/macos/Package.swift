// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "OctetMacOS",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(name: "OctetMacOS", targets: ["OctetMacOS"]),
    ],
    dependencies: [
        // The shared package owns Serve wire validation, TLS pinning, pairing,
        // credential storage, replay, and command idempotency. The Mac target
        // must not grow a second transport implementation.
        .package(path: "../apple-shared"),
    ],
    targets: [
        .executableTarget(
            name: "OctetMacOS",
            dependencies: [
                .product(name: "OctetServe", package: "apple-shared"),
            ],
            path: "Sources/OctetMacOS",
            // `Resources/Info.plist` is consumed by `scripts/build-app.sh` (installed
            // into `Contents/Info.plist` of the assembled bundle); SwiftPM forbids
            // Info.plist as a resource of an executable target, so it is excluded.
            exclude: ["Resources/Info.plist"],
            swiftSettings: [
                .enableUpcomingFeature("StrictConcurrency"),
            ]
        ),
        .testTarget(
            name: "OctetMacOSTests",
            dependencies: ["OctetMacOS"],
            path: "Tests/OctetMacOSTests"
        ),
    ]
)
