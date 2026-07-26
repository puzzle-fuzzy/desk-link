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
    dependencies: [
        .package(path: "../apple"),
    ],
    targets: [
        .executableTarget(
            name: "DeskLinkApp",
            dependencies: [
                .product(name: "DeskLinkAppleCore", package: "apple"),
                .product(name: "DeskLinkC", package: "apple"),
            ],
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
            dependencies: [
                "DeskLinkApp",
                .product(name: "DeskLinkAppleCore", package: "apple"),
            ],
            path: "Tests/DeskLinkAppTests"
        ),
    ],
    swiftLanguageModes: [.v6]
)
