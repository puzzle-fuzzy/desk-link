// swift-tools-version: 6.0
import PackageDescription

#if arch(x86_64)
fatalError("DeskLinkApp requires an Apple Silicon arm64 build")
#endif

let package = Package(
    name: "DeskLinkApp",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(name: "DeskLinkApp", targets: ["DeskLinkApp"]),
    ],
    targets: [
        .target(
            name: "DeskLinkC",
            path: "Sources/DeskLinkC",
            publicHeadersPath: "include"
        ),
        .executableTarget(
            name: "DeskLinkApp",
            dependencies: ["DeskLinkC"],
            path: "Sources/DeskLinkApp",
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("ApplicationServices"),
                .linkedFramework("CoreMedia"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("CoreVideo"),
                .linkedFramework("Metal"),
                .linkedFramework("MetalKit"),
                .linkedFramework("ScreenCaptureKit"),
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
            name: "DeskLinkAppTests",
            dependencies: ["DeskLinkApp"],
            path: "Tests/DeskLinkAppTests"
        ),
    ],
    swiftLanguageModes: [.v6]
)
