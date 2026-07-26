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
        private var previewLayer: AVCaptureVideoPreviewLayer?

        override func viewDidLoad() {
            super.viewDidLoad()
            view.backgroundColor = .black
            configureSession()
        }

        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            previewLayer?.frame = view.bounds
        }

        func start() {
            guard !session.isRunning else { return }
            DispatchQueue.global(qos: .userInitiated).async { [session] in
                session.startRunning()
            }
        }

        func stop() {
            guard session.isRunning else { return }
            session.stopRunning()
        }

        private func configureSession() {
            guard let device = AVCaptureDevice.default(for: .video),
                  let input = try? AVCaptureDeviceInput(device: device),
                  session.canAddInput(input)
            else { return }
            session.addInput(input)

            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]

            let layer = AVCaptureVideoPreviewLayer(session: session)
            layer.videoGravity = .resizeAspectFill
            previewLayer = layer
            view.layer.insertSublayer(layer, at: 0)
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
