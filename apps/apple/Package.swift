// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "DeskLinkAppleCore",
    platforms: [
        .macOS(.v13),
        .iOS(.v16),
    ],
    products: [
        .library(name: "DeskLinkAppleCore", targets: ["DeskLinkAppleCore"]),
        .library(name: "DeskLinkC", targets: ["DeskLinkC"]),
    ],
    targets: [
        .target(
            name: "DeskLinkC",
            path: "Sources/DeskLinkC",
            publicHeadersPath: "include"
        ),
        .target(
            name: "DeskLinkAppleCore",
            dependencies: ["DeskLinkC"],
            path: "Sources/DeskLinkAppleCore",
            linkerSettings: [
                .linkedFramework("CoreMedia"),
                .linkedFramework("CoreVideo"),
                .linkedFramework("Security"),
                .linkedFramework("VideoToolbox"),
                .unsafeFlags([
                    "-L", "../../target/aarch64-apple-darwin/release",
                    "-L", "../../target/debug",
                    "-ldesklink_ffi",
                ]),
            ]
        ),
        .testTarget(
            name: "DeskLinkAppleCoreTests",
            dependencies: ["DeskLinkAppleCore"],
            path: "Tests/DeskLinkAppleCoreTests"
        ),
    ],
    swiftLanguageModes: [.v6]
)
