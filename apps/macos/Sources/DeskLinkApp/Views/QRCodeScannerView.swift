import AVFoundation
import AppKit
import SwiftUI

final class QRCodeScannerModel: NSObject, ObservableObject, AVCaptureMetadataOutputObjectsDelegate, @unchecked Sendable {
    @Published private(set) var message = "正在准备相机…"
    @Published private(set) var detectedPayload: String?
    @Published private(set) var canScan = false

    let session = AVCaptureSession()
    private let sessionQueue = DispatchQueue(label: "com.desklink.qr-scanner")
    private var isConfigured = false
    private var hasReportedResult = false

    func start() {
        hasReportedResult = false
        detectedPayload = nil

        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            configureAndStart()
        case .notDetermined:
            message = "需要允许 DeskLink 使用相机来扫描二维码。"
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    guard let self else { return }
                    if granted {
                        self.configureAndStart()
                    } else {
                        self.canScan = false
                        self.message = "相机权限未开启，请在系统设置中允许 DeskLink 使用相机。"
                    }
                }
            }
        case .denied, .restricted:
            canScan = false
            message = "相机权限未开启，请在系统设置中允许 DeskLink 使用相机。"
        @unknown default:
            canScan = false
            message = "当前无法使用相机，请改用设备 ID 和密码连接。"
        }
    }

    func stop() {
        sessionQueue.async { [weak self] in
            guard let self, self.session.isRunning else { return }
            self.session.stopRunning()
        }
    }

    func openCameraSettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera") else {
            return
        }
        NSWorkspace.shared.open(url)
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !hasReportedResult,
              let code = metadataObjects
                .compactMap({ $0 as? AVMetadataMachineReadableCodeObject })
                .first(where: { $0.type == .qr }),
              let value = code.stringValue,
              !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { return }

        hasReportedResult = true
        session.stopRunning()
        DispatchQueue.main.async { [weak self] in
            self?.detectedPayload = value
            self?.canScan = false
            self?.message = "二维码已识别，正在准备连接。"
        }
    }

    private func configureAndStart() {
        sessionQueue.async { [weak self] in
            guard let self else { return }

            if !self.isConfigured {
                guard let camera = AVCaptureDevice.default(for: .video) else {
                    DispatchQueue.main.async { [weak self] in
                        self?.canScan = false
                        self?.message = "没有找到可用相机，请改用设备 ID 和密码连接。"
                    }
                    return
                }

                do {
                    let input = try AVCaptureDeviceInput(device: camera)
                    let metadataOutput = AVCaptureMetadataOutput()
                    guard self.session.canAddInput(input), self.session.canAddOutput(metadataOutput) else {
                        throw ScannerError.configurationUnavailable
                    }

                    self.session.beginConfiguration()
                    self.session.addInput(input)
                    self.session.addOutput(metadataOutput)
                    metadataOutput.setMetadataObjectsDelegate(self, queue: self.sessionQueue)
                    metadataOutput.metadataObjectTypes = [.qr]
                    self.session.commitConfiguration()
                    self.isConfigured = true
                } catch {
                    DispatchQueue.main.async { [weak self] in
                        self?.canScan = false
                        self?.message = "相机初始化失败，请改用设备 ID 和密码连接。"
                    }
                    return
                }
            }

            guard !self.session.isRunning else { return }
            self.session.startRunning()
            DispatchQueue.main.async { [weak self] in
                self?.canScan = true
                self?.message = "将二维码放入取景框内，识别后会自动开始连接。"
            }
        }
    }

    private enum ScannerError: Error {
        case configurationUnavailable
    }
}

struct QRCodeScannerSheet: View {
    @Environment(\.dismiss) private var dismiss
    @StateObject private var scanner = QRCodeScannerModel()
    let onScanned: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("扫描二维码")
                        .font(.system(size: 20, weight: .semibold))
                    Text("扫描另一台设备生成的一次性连接二维码。")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("取消") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }

            if scanner.canScan {
                QRCodeScannerPreview(session: scanner.session)
                    .frame(width: 460, height: 290)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .overlay {
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(Color.white.opacity(0.35), lineWidth: 1)
                    }
            } else {
                VStack(spacing: 12) {
                    Image(systemName: "camera.viewfinder")
                        .font(.system(size: 34, weight: .medium))
                        .foregroundStyle(.secondary)
                    Text(scanner.message)
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 340)
                    if AVCaptureDevice.authorizationStatus(for: .video) == .denied {
                        Button("打开相机设置") { scanner.openCameraSettings() }
                            .buttonStyle(.bordered)
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 290)
                .background(Color(nsColor: .underPageBackgroundColor), in: RoundedRectangle(cornerRadius: 10))
            }

            HStack(spacing: 8) {
                Image(systemName: "info.circle")
                Text(scanner.message)
                    .lineLimit(2)
            }
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
        }
        .padding(24)
        .frame(width: 510, height: 430)
        .onAppear { scanner.start() }
        .onDisappear { scanner.stop() }
        .onChange(of: scanner.detectedPayload) { payload in
            guard let payload else { return }
            onScanned(payload)
            dismiss()
        }
    }
}

struct QRCodeScannerPreview: NSViewRepresentable {
    let session: AVCaptureSession

    func makeNSView(context: Context) -> QRCodeScannerPreviewView {
        let view = QRCodeScannerPreviewView()
        view.session = session
        return view
    }

    func updateNSView(_ nsView: QRCodeScannerPreviewView, context: Context) {
        nsView.session = session
    }
}

final class QRCodeScannerPreviewView: NSView {
    var session: AVCaptureSession? {
        didSet { previewLayer.session = session }
    }

    private var previewLayer: AVCaptureVideoPreviewLayer {
        guard let layer = layer as? AVCaptureVideoPreviewLayer else {
            preconditionFailure("QRCodeScannerPreviewView requires an AVCaptureVideoPreviewLayer")
        }
        return layer
    }

    override func makeBackingLayer() -> CALayer {
        let layer = AVCaptureVideoPreviewLayer()
        layer.videoGravity = .resizeAspectFill
        return layer
    }

    override func layout() {
        super.layout()
        previewLayer.frame = bounds
    }
}
