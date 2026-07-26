import AppKit
import Foundation

guard CommandLine.arguments.count == 4 else {
    fputs("usage: render-apple-icon.swift <svg> <png> <size>\n", stderr)
    exit(64)
}

let sourceURL = URL(fileURLWithPath: CommandLine.arguments[1])
let destinationURL = URL(fileURLWithPath: CommandLine.arguments[2])
guard let size = Int(CommandLine.arguments[3]), size > 0 else {
    fputs("icon size must be a positive integer\n", stderr)
    exit(64)
}

do {
    let source = try Data(contentsOf: sourceURL)
    guard let image = NSImage(data: source) else {
        throw NSError(domain: "DeskLinkIconRenderer", code: 1)
    }
    guard let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: size,
        pixelsHigh: size,
        bitsPerSample: 8,
        samplesPerPixel: 4,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: .deviceRGB,
        bitmapFormat: [],
        bytesPerRow: 0,
        bitsPerPixel: 0
    ) else {
        throw NSError(domain: "DeskLinkIconRenderer", code: 2)
    }

    bitmap.size = NSSize(width: size, height: size)
    guard let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
        throw NSError(domain: "DeskLinkIconRenderer", code: 3)
    }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    image.draw(
        in: NSRect(x: 0, y: 0, width: size, height: size),
        from: .zero,
        operation: .sourceOver,
        fraction: 1
    )
    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()

    guard let png = bitmap.representation(using: .png, properties: [:]) else {
        throw NSError(domain: "DeskLinkIconRenderer", code: 4)
    }
    try png.write(to: destinationURL)
} catch {
    fputs("failed to render icon: \(error)\n", stderr)
    exit(1)
}
