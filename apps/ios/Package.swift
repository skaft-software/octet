// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "OctetCompanion",
    // iOS is the shipping platform. macOS is declared so the same host-neutral
    // service, reducer, wire and view-model sources can be built and unit
    // tested on a macOS host without an iOS simulator or device.
    platforms: [.iOS(.v16), .macOS(.v13)],
    products: [.library(name: "OctetCompanion", targets: ["OctetCompanion"])],
    // No SwiftPM dependency on `../apple-shared` (product `OctetServe`) is
    // declared: this library imports nothing from it by design — the app-owned
    // `ServeClientBoundary` binds the transport through a closure factory. The
    // shared package itself now compiles (`swift build` in `apps/apple-shared`
    // completes), but it exports wire DTOs only and still has no client surface
    // (`ServeClient`, pairing, Keychain, replay) for this target to link. The
    // Xcode app target links the shared client through `project.yml`, and an
    // adapter re-adds the product dependency here once that client surface
    // exists.
    targets: [
        .target(
            name: "OctetCompanion",
            path: "Sources/OctetCompanion"
        ),
        .testTarget(
            name: "OctetCompanionTests",
            dependencies: ["OctetCompanion"],
            path: "Tests/OctetCompanionTests"
        )
    ]
)
