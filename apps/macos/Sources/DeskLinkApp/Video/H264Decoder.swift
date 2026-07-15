import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

private final class SendablePixelBuffer: @unchecked Sendable {
    let value: CVPixelBuffer

    init(_ value: CVPixelBuffer) {
        self.value = value
    }
}

@MainActor
final class H264Decoder {
    var onFrame: ((CVPixelBuffer) -> Void)?

    private var decompressionSession: VTDecompressionSession?
    private var formatDescription: CMVideoFormatDescription?
    private(set) var latestPixelBuffer: CVPixelBuffer?
    private(set) var lastFrameID: UInt64 = 0
    private(set) var configVersion: UInt32 = 0
    private(set) var configuredWidth: UInt16 = 0
    private(set) var configuredHeight: UInt16 = 0
    private var consecutiveFailures = 0
    private var keyframeRequestIssued = false

    deinit {
        if let decompressionSession {
            VTDecompressionSessionInvalidate(decompressionSession)
        }
    }

    @discardableResult
    func configure(
        sequenceHeader: Data,
        width: UInt16,
        height: UInt16,
        version: UInt32
    ) -> Bool {
        guard width > 0, height > 0, version > 0, version >= configVersion else {
            registerFailure()
            return false
        }
        if version == configVersion,
           width == configuredWidth,
           height == configuredHeight,
           decompressionSession != nil
        {
            return true
        }
        guard let parameterSets = try? H264AnnexB.parameterSets(in: sequenceHeader) else {
            registerFailure()
            return false
        }

        var description: CMVideoFormatDescription?
        let status = parameterSets.sps.withUnsafeBytes { spsBytes in
            parameterSets.pps.withUnsafeBytes { ppsBytes in
                var pointers: [UnsafePointer<UInt8>] = [
                    spsBytes.bindMemory(to: UInt8.self).baseAddress!,
                    ppsBytes.bindMemory(to: UInt8.self).baseAddress!,
                ]
                let sizes = [parameterSets.sps.count, parameterSets.pps.count]
                return pointers.withUnsafeMutableBufferPointer { pointerBuffer in
                    sizes.withUnsafeBufferPointer { sizeBuffer in
                        CMVideoFormatDescriptionCreateFromH264ParameterSets(
                            allocator: kCFAllocatorDefault,
                            parameterSetCount: 2,
                            parameterSetPointers: pointerBuffer.baseAddress!,
                            parameterSetSizes: sizeBuffer.baseAddress!,
                            nalUnitHeaderLength: 4,
                            formatDescriptionOut: &description
                        )
                    }
                }
            }
        }
        guard status == noErr, let description else {
            registerFailure()
            return false
        }

        invalidateSession()
        var callback = VTDecompressionOutputCallbackRecord(
            decompressionOutputCallback: h264OutputCallback,
            decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque()
        )
        var session: VTDecompressionSession?
        let sessionStatus = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: description,
            decoderSpecification: nil,
            imageBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: Int(width),
                kCVPixelBufferHeightKey as String: Int(height),
                kCVPixelBufferMetalCompatibilityKey as String: true,
            ] as CFDictionary,
            outputCallback: &callback,
            decompressionSessionOut: &session
        )
        guard sessionStatus == noErr, let session else {
            registerFailure()
            return false
        }
        formatDescription = description
        decompressionSession = session
        configVersion = version
        configuredWidth = width
        configuredHeight = height
        consecutiveFailures = 0
        keyframeRequestIssued = false
        return true
    }

    @discardableResult
    func receive(accessUnit: Data, frameID: UInt64, version: UInt32) -> Bool {
        guard frameID > lastFrameID,
              frameID <= UInt64(Int64.max),
              !accessUnit.isEmpty
        else { return false }
        guard version == configVersion,
              let session = decompressionSession,
              let formatDescription,
              let avcc = try? H264AnnexB.avccAccessUnit(from: accessUnit)
        else {
            registerFailure()
            return false
        }

        var blockBuffer: CMBlockBuffer?
        guard CMBlockBufferCreateEmpty(
            allocator: nil,
            capacity: 1,
            flags: CMBlockBufferFlags(0),
            blockBufferOut: &blockBuffer
        ) == noErr,
              let blockBuffer,
              CMBlockBufferAppendMemoryBlock(
                  blockBuffer,
                  memoryBlock: nil,
                  length: avcc.count,
                  blockAllocator: nil,
                  customBlockSource: nil,
                  offsetToData: 0,
                  dataLength: avcc.count,
                  flags: CMBlockBufferFlags(0)
              ) == noErr
        else {
            registerFailure()
            return false
        }

        let copyStatus = avcc.withUnsafeBytes { bytes in
            CMBlockBufferReplaceDataBytes(
                with: bytes.baseAddress!,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: avcc.count
            )
        }
        guard copyStatus == noErr else {
            registerFailure()
            return false
        }

        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: 30),
            presentationTimeStamp: CMTime(value: CMTimeValue(frameID), timescale: 30),
            decodeTimeStamp: .invalid
        )
        var sampleSize = avcc.count
        var sampleBuffer: CMSampleBuffer?
        let sampleStatus = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: blockBuffer,
            formatDescription: formatDescription,
            sampleCount: 1,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        )
        guard sampleStatus == noErr, let sampleBuffer else {
            registerFailure()
            return false
        }

        // Use the stable CF_OPTIONS bit instead of an SDK-specific Swift enum spelling.
        // kVTDecodeFrame_EnableAsynchronousDecompression has raw value 1 << 0.
        let asynchronousDecode = VTDecodeFrameFlags(rawValue: 1 << 0)
        let decodeStatus = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: asynchronousDecode,
            frameRefcon: nil,
            infoFlagsOut: nil
        )
        guard decodeStatus == noErr else {
            registerFailure()
            return false
        }
        lastFrameID = frameID
        consecutiveFailures = 0
        keyframeRequestIssued = false
        return true
    }

    func takeKeyframeRequest() -> Bool {
        guard consecutiveFailures >= 3, !keyframeRequestIssued else { return false }
        keyframeRequestIssued = true
        return true
    }

    func reset() {
        invalidateSession()
        latestPixelBuffer = nil
        lastFrameID = 0
        configVersion = 0
        configuredWidth = 0
        configuredHeight = 0
        consecutiveFailures = 0
        keyframeRequestIssued = false
    }

    private func invalidateSession() {
        if let decompressionSession {
            VTDecompressionSessionWaitForAsynchronousFrames(decompressionSession)
            VTDecompressionSessionInvalidate(decompressionSession)
        }
        decompressionSession = nil
        formatDescription = nil
    }

    private func registerFailure() {
        consecutiveFailures += 1
    }

    private func accept(pixelBuffer: CVPixelBuffer, frameID: UInt64) {
        guard frameID >= lastFrameID else { return }
        latestPixelBuffer = pixelBuffer
        onFrame?(pixelBuffer)
    }

    nonisolated fileprivate func enqueue(pixelBuffer: CVPixelBuffer, frameID: UInt64) {
        let sendableBuffer = SendablePixelBuffer(pixelBuffer)
        Task { @MainActor [weak self, sendableBuffer] in
            self?.accept(pixelBuffer: sendableBuffer.value, frameID: frameID)
        }
    }
}

private func h264OutputCallback(
    _ decompressionOutputRefCon: UnsafeMutableRawPointer?,
    _ sourceFrameRefCon: UnsafeMutableRawPointer?,
    _ status: OSStatus,
    _ infoFlags: VTDecodeInfoFlags,
    _ imageBuffer: CVImageBuffer?,
    _ presentationTimeStamp: CMTime,
    _ presentationDuration: CMTime
) {
    guard status == noErr,
          let decompressionOutputRefCon,
          let imageBuffer,
          presentationTimeStamp.value >= 0
    else { return }
    let decoder = Unmanaged<H264Decoder>
        .fromOpaque(decompressionOutputRefCon)
        .takeUnretainedValue()
    decoder.enqueue(
        pixelBuffer: imageBuffer,
        frameID: UInt64(presentationTimeStamp.value)
    )
}
