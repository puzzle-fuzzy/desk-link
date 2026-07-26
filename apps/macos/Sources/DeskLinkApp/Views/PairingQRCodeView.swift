import AppKit
import CoreImage
import SwiftUI

struct PairingQRCodeView: View {
    let encodedInvite: Data

    var body: some View {
        Group {
            if let image = DeskLinkQRCode.image(for: encodedInvite.base64EncodedString(), size: 220) {
                Image(nsImage: image)
                    .interpolation(.none)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 220, height: 220)
                    .padding(12)
                    .background(Color.white)
                    .overlay {
                        RoundedRectangle(cornerRadius: 6)
                            .stroke(DeskLinkPalette.border, lineWidth: 1)
                    }
                    .accessibilityLabel("DeskLink 连接二维码")
                    .accessibilityHint("让 iPhone 的 DeskLink 扫描此二维码")
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "qrcode")
                        .font(.system(size: 42))
                    Text("二维码暂时无法生成")
                        .font(.system(size: 12, weight: .semibold))
                    Text("请重新生成二维码")
                        .font(.system(size: 11))
                }
                .foregroundStyle(DeskLinkPalette.secondaryInk)
                .frame(width: 220, height: 220)
                .background(Color.white)
                .overlay {
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(DeskLinkPalette.border, lineWidth: 1)
                }
            }
        }
    }
}

enum DeskLinkQRCode {
    private static let context = CIContext()

    static func payload(for encodedInvite: Data) -> String {
        encodedInvite.base64EncodedString()
    }

    static func image(for payload: String, size: CGFloat) -> NSImage? {
        guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
        filter.setValue(Data(payload.utf8), forKey: "inputMessage")
        filter.setValue("Q", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage,
              let cgImage = context.createCGImage(output, from: output.extent.integral)
        else { return nil }

        return NSImage(
            cgImage: cgImage,
            size: NSSize(width: size, height: size)
        )
    }
}
