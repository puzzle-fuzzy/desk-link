import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

final class H264Decoder {
    private var decompressionSession: VTDecompressionSession?
    private var formatDescription: CMVideoFormatDescription?
    private(set) var latestPixelBuffer: CVPixelBuffer?
    private(set) var lastFrameID: UInt64 = 0
    private(set) var configVersion: UInt32 = 0
    private var consecutiveFailures = 0

    deinit {
        invalidate()
    }

    @discardableResult
    func configure(sps: Data, pps: Data, version: UInt32) -> Bool {
        guard !sps.isEmpty, !pps.isEmpty else { return false }

        var description: CMVideoFormatDescription?
        let status = sps.withUnsafeBytes { spsBytes in
            pps.withUnsafeBytes { ppsBytes in
                var pointers: [UnsafePointer<UInt8>] = [
                    spsBytes.bindMemory(to: UInt8.self).baseAddress!,
                    ppsBytes.bindMemory(to: UInt8.self).baseAddress!,
                ]
                let sizes = [sps.count, pps.count]
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
        guard status == noErr, let description else { return false }

        invalidate()
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
            ] as CFDictionary,
            outputCallback: &callback,
            decompressionSessionOut: &session
        )
        guard sessionStatus == noErr, let session else { return false }
        formatDescription = description
        decompressionSession = session
        configVersion = version
        consecutiveFailures = 0
        return true
    }

    @discardableResult
    func receive(accessUnit: Data, frameID: UInt64, version: UInt32) -> Bool {
        guard frameID > lastFrameID, !accessUnit.isEmpty else { return false }
        guard version == configVersion,
              let session = decompressionSession,
              let formatDescription
        else {
            consecutiveFailures += 1
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
                  length: accessUnit.count,
                  blockAllocator: nil,
                  customBlockSource: nil,
                  offsetToData: 0,
                  dataLength: accessUnit.count,
                  flags: CMBlockBufferFlags(0)
              ) == noErr
        else {
            consecutiveFailures += 1
            return false
        }

        let copyStatus = accessUnit.withUnsafeBytes { bytes in
            CMBlockBufferReplaceDataBytes(
                with: bytes.baseAddress!,
                blockBuffer: blockBuffer,
                offsetIntoDestination: 0,
                dataLength: accessUnit.count
            )
        }
        guard copyStatus == noErr else {
            consecutiveFailures += 1
            return false
        }

        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: 30),
            presentationTimeStamp: CMTime(value: CMTimeValue(frameID), timescale: 30),
            decodeTimeStamp: .invalid
        )
        var sampleSize = accessUnit.count
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
            consecutiveFailures += 1
            return false
        }

        let decodeStatus = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sampleBuffer,
            flags: [],
            frameRefcon: nil,
            infoFlagsOut: nil
        )
        guard decodeStatus == noErr else {
            consecutiveFailures += 1
            return false
        }
        lastFrameID = frameID
        consecutiveFailures = 0
        return true
    }

    var shouldRequestKeyframe: Bool { consecutiveFailures >= 3 }

    func invalidate() {
        if let session = decompressionSession {
            VTDecompressionSessionInvalidate(session)
        }
        decompressionSession = nil
        formatDescription = nil
    }

    fileprivate func accept(pixelBuffer: CVPixelBuffer) {
        latestPixelBuffer = pixelBuffer
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
          let imageBuffer
    else { return }
    let decoder = Unmanaged<H264Decoder>
        .fromOpaque(decompressionOutputRefCon)
        .takeUnretainedValue()
    decoder.accept(pixelBuffer: imageBuffer)
}
