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
    // `ServeClientBoundary` binds the transport through a closure factory — and
    // the shared package's sources do not compile in this tree
    // (`apps/apple-shared/Sources/OctetServe/WireEnums.swift:21` uses the
    // `extension` keyword as an enum case). The Xcode app target still links the
    // shared client through `project.yml`, and a future adapter re-adds the
    // product dependency here.
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
