@preconcurrency import AVFoundation
import DeskLinkC
import Foundation
import SwiftUI
import UIKit

enum IOSPairingInput {
    static func decodeInvite(_ text: String) -> Data? {
        let normalized = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let data = Data(base64Encoded: normalized),
              data.count == Int(DESKLINK_PAIRING_INVITE_BYTES)
        else { return nil }
        return data
    }
}

struct IOSQRCodeScanner: UIViewControllerRepresentable {
    let onInvite: (Data) -> Void
    let onInvalidPayload: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(onInvite: onInvite, onInvalidPayload: onInvalidPayload)
    }

    func makeUIViewController(context: Context) -> ScannerViewController {
        let controller = ScannerViewController()
        controller.onPayload = context.coordinator.handle
        controller.onInvalidPayload = context.coordinator.handleInvalid
        context.coordinator.start(controller: controller)
        return controller
    }

    func updateUIViewController(_ controller: ScannerViewController, context: Context) {}

    static func dismantleUIViewController(_ controller: ScannerViewController, coordinator: Coordinator) {
        controller.stop()
    }

    @MainActor
    final class Coordinator: NSObject {
        private let onInvite: (Data) -> Void
        private let onInvalidPayload: () -> Void
        private var didFinish = false

        init(onInvite: @escaping (Data) -> Void, onInvalidPayload: @escaping () -> Void) {
            self.onInvite = onInvite
            self.onInvalidPayload = onInvalidPayload
        }

        func start(controller: ScannerViewController) {
            controller.start()
        }

        func handle(_ payload: String, controller: ScannerViewController) {
            guard !didFinish else { return }
            guard let invite = IOSPairingInput.decodeInvite(payload) else {
                handleInvalid(controller: controller)
                return
            }
            didFinish = true
            controller.stop()
            onInvite(invite)
        }

        func handleInvalid(controller: ScannerViewController) {
            guard !didFinish else { return }
            didFinish = true
            controller.stop()
            onInvalidPayload()
        }
    }

    @MainActor
    final class ScannerViewController: UIViewController, @MainActor AVCaptureMetadataOutputObjectsDelegate {
        var onPayload: ((String, ScannerViewController) -> Void)?
        var onInvalidPayload: ((ScannerViewController) -> Void)?

        private let session = AVCaptureSession()
        private let sessionQueue = DispatchQueue(label: "com.desklink.ios.qr-scanner")
        private var previewLayer: AVCaptureVideoPreviewLayer?
        private var activityIndicator: UIActivityIndicatorView?
        private var statusLabel: UILabel?

        override func viewDidLoad() {
            super.viewDidLoad()
            view.backgroundColor = .black
            view.isOpaque = true

            let previewLayer = AVCaptureVideoPreviewLayer(session: session)
            previewLayer.videoGravity = .resizeAspectFill
            self.previewLayer = previewLayer
            view.layer.insertSublayer(previewLayer, at: 0)

            let activityIndicator = UIActivityIndicatorView(style: .large)
            activityIndicator.color = .white
            activityIndicator.hidesWhenStopped = true
            activityIndicator.startAnimating()
            activityIndicator.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(activityIndicator)
            NSLayoutConstraint.activate([
                activityIndicator.centerXAnchor.constraint(equalTo: view.centerXAnchor),
                activityIndicator.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            ])
            self.activityIndicator = activityIndicator

            let statusLabel = UILabel()
            statusLabel.text = "正在启动相机…"
            statusLabel.textColor = .white.withAlphaComponent(0.82)
            statusLabel.font = .preferredFont(forTextStyle: .subheadline)
            statusLabel.textAlignment = .center
            statusLabel.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(statusLabel)
            NSLayoutConstraint.activate([
                statusLabel.topAnchor.constraint(equalTo: activityIndicator.bottomAnchor, constant: 14),
                statusLabel.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            ])
            self.statusLabel = statusLabel
        }

        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            previewLayer?.frame = view.bounds
        }

        func start() {
            if !isViewLoaded {
                loadViewIfNeeded()
            }

            let session = session
            sessionQueue.async { [weak self, session] in
                guard let self else { return }

                if session.inputs.isEmpty {
                    guard self.configureSession(session) else { return }
                }

                guard !session.isRunning else {
                    DispatchQueue.main.async { [weak self] in
                        self?.scannerDidBecomeReady()
                    }
                    return
                }

                session.startRunning()
                DispatchQueue.main.async { [weak self] in
                    self?.scannerDidBecomeReady()
                }
            }
        }

        func stop() {
            let session = session
            sessionQueue.async { [session] in
                guard session.isRunning else { return }
                session.stopRunning()
            }
        }

        private func configureSession(_ session: AVCaptureSession) -> Bool {
            guard let device = AVCaptureDevice.default(for: .video),
                  let input = try? AVCaptureDeviceInput(device: device),
                  session.canAddInput(input)
            else {
                DispatchQueue.main.async { [weak self] in
                    self?.scannerDidFail(message: "无法访问相机，请在系统设置中允许 DeskLink 使用相机。")
                }
                return false
            }

            session.beginConfiguration()
            defer { session.commitConfiguration() }
            session.addInput(input)

            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else {
                DispatchQueue.main.async { [weak self] in
                    self?.scannerDidFail(message: "相机扫码功能暂时不可用，请稍后重试。")
                }
                return false
            }
            session.addOutput(output)
            output.metadataObjectTypes = [.qr]
            DispatchQueue.main.async { [weak self, output] in
                guard let self else { return }
                output.setMetadataObjectsDelegate(self, queue: .main)
            }
            return true
        }

        private func scannerDidBecomeReady() {
            activityIndicator?.stopAnimating()
            statusLabel?.isHidden = true
        }

        private func scannerDidFail(message: String) {
            activityIndicator?.stopAnimating()
            statusLabel?.text = message
            statusLabel?.numberOfLines = 0
            statusLabel?.preferredMaxLayoutWidth = view.bounds.width - 48
        }

        func metadataOutput(
            _ output: AVCaptureMetadataOutput,
            didOutput metadataObjects: [AVMetadataObject],
            from connection: AVCaptureConnection
        ) {
            guard let object = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
                  let value = object.stringValue
            else { return }
            onPayload?(value, self)
        }
    }
}
