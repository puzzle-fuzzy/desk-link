import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

struct H264EncoderFormat: Equatable, Sendable {
    let sps: Data
    let pps: Data

    var annexBParameterSets: Data {
        Data([0, 0, 0, 1]) + sps + Data([0, 0, 0, 1]) + pps
    }
}

enum EncodedVideoEvent: Equatable, Sendable {
    case configuration(
        streamID: UInt64,
        version: UInt32,
        width: UInt16,
        height: UInt16,
        annexBParameterSets: Data
    )
    case accessUnit(
        streamID: UInt64,
        frameID: UInt64,
        configVersion: UInt32,
        isKeyframe: Bool,
        annexB: Data
    )
}

enum MacH264EncoderError: Error, Equatable {
    case invalidDimensions
    case session(OSStatus)
    case malformedAVCC
    case missingFormatDescription
}

enum H264EncoderOutputAssembler {
    static func events(
        avccAccessUnit: Data,
        format: H264EncoderFormat,
        frameID: UInt64,
        streamID: UInt64,
        width: UInt16,
        height: UInt16,
        configVersion: UInt32,
        emitsConfiguration: Bool
    ) throws -> [EncodedVideoEvent] {
        let annexB = try annexB(fromAVCC: avccAccessUnit)
        var output: [EncodedVideoEvent] = []
        if emitsConfiguration {
            output.append(.configuration(
                streamID: streamID,
                version: configVersion,
                width: width,
                height: height,
                annexBParameterSets: format.annexBParameterSets
            ))
        }
        output.append(.accessUnit(
            streamID: streamID,
            frameID: frameID,
            configVersion: configVersion,
            isKeyframe: containsIDR(in: annexB),
            annexB: annexB
        ))
        return output
    }

    static func annexB(fromAVCC avcc: Data) throws -> Data {
        let bytes = [UInt8](avcc)
        var offset = 0
        var output = Data()
        while offset < bytes.count {
            guard offset + 4 <= bytes.count else { throw MacH264EncoderError.malformedAVCC }
            let length = Int(bytes[offset]) << 24 | Int(bytes[offset + 1]) << 16
                | Int(bytes[offset + 2]) << 8 | Int(bytes[offset + 3])
            offset += 4
            guard length > 0, offset + length <= bytes.count else {
                throw MacH264EncoderError.malformedAVCC
            }
            output.append(contentsOf: [0, 0, 0, 1])
            output.append(contentsOf: bytes[offset..<(offset + length)])
            offset += length
        }
        guard !output.isEmpty else { throw MacH264EncoderError.malformedAVCC }
        return output
    }

    private static func containsIDR(in annexB: Data) -> Bool {
        H264AnnexB.nalUnits(in: annexB).contains { ($0.first ?? 0) & 0x1f == 5 }
    }
}

final class MacH264Encoder: @unchecked Sendable {
    typealias EventHandler = @Sendable (EncodedVideoEvent) -> Void

    private let eventQueue = DispatchQueue(label: "com.desklink.encoder.events", qos: .userInitiated)
    private let lock = NSLock()
    private var compressionSession: VTCompressionSession?
    private var forceKeyframe = false
    private var streamID: UInt64 = 0
    private var width: UInt16 = 0
    private var height: UInt16 = 0
    private var configurationVersion: UInt32 = 0
    private var emittedConfigurationVersion: UInt32 = 0
    private var latestFormat: H264EncoderFormat?

    var onEvent: EventHandler?

    deinit { stop() }

    func start(width: Int, height: Int, streamID: UInt64 = 0) throws {
        guard let encodedWidth = UInt16(exactly: width),
              let encodedHeight = UInt16(exactly: height),
              encodedWidth > 0, encodedHeight > 0
        else { throw MacH264EncoderError.invalidDimensions }
        stop()
        var session: VTCompressionSession?
        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: Int32(width),
            height: Int32(height),
            codecType: kCMVideoCodecType_H264,
            encoderSpecification: nil,
            imageBufferAttributes: nil,
            compressedDataAllocator: nil,
            outputCallback: macH264EncoderOutputCallback,
            refcon: Unmanaged.passUnretained(self).toOpaque(),
            compressionSessionOut: &session
        )
        guard status == noErr, let session else { throw MacH264EncoderError.session(status) }
        configure(session: session, width: width, height: height)
        let prepareStatus = VTCompressionSessionPrepareToEncodeFrames(session)
        guard prepareStatus == noErr else {
            VTCompressionSessionInvalidate(session)
            throw MacH264EncoderError.session(prepareStatus)
        }
        lock.lock()
        compressionSession = session
        self.streamID = streamID
        self.width = encodedWidth
        self.height = encodedHeight
        configurationVersion = 0
        emittedConfigurationVersion = 0
        latestFormat = nil
        forceKeyframe = false
        lock.unlock()
    }

    func encode(pixelBuffer: CVPixelBuffer, frameID: UInt64) {
        lock.lock()
        guard let session = compressionSession else {
            lock.unlock()
            return
        }
        let shouldForceKeyframe = forceKeyframe
        forceKeyframe = false
        lock.unlock()
        let properties: CFDictionary? = shouldForceKeyframe
            ? [kVTEncodeFrameOptionKey_ForceKeyFrame as String: true] as CFDictionary
            : nil
        _ = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: pixelBuffer,
            presentationTimeStamp: CMTime(value: CMTimeValue(frameID), timescale: 30),
            duration: CMTime(value: 1, timescale: 30),
            frameProperties: properties,
            sourceFrameRefcon: nil,
            infoFlagsOut: nil
        )
    }

    func requestKeyframe() {
        lock.lock()
        forceKeyframe = true
        lock.unlock()
    }

    func stop() {
        lock.lock()
        let session = compressionSession
        compressionSession = nil
        latestFormat = nil
        emittedConfigurationVersion = 0
        configurationVersion = 0
        lock.unlock()
        if let session {
            VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
            VTCompressionSessionInvalidate(session)
        }
    }

    fileprivate func accept(sampleBuffer: CMSampleBuffer) {
        guard let dataBuffer = CMSampleBufferGetDataBuffer(sampleBuffer),
              let formatDescription = CMSampleBufferGetFormatDescription(sampleBuffer),
              let format = Self.format(from: formatDescription),
              let avcc = Self.data(from: dataBuffer)
        else { return }
        let frameID = UInt64(max(0, CMSampleBufferGetPresentationTimeStamp(sampleBuffer).value))
        lock.lock()
        if latestFormat != format {
            latestFormat = format
            configurationVersion &+= 1
            if configurationVersion == 0 { configurationVersion = 1 }
        }
        let version = configurationVersion
        let emitsConfiguration = emittedConfigurationVersion != version
        if emitsConfiguration { emittedConfigurationVersion = version }
        let currentStreamID = streamID
        let currentWidth = width
        let currentHeight = height
        lock.unlock()
        guard version > 0,
              let events = try? H264EncoderOutputAssembler.events(
                  avccAccessUnit: avcc,
                  format: format,
                  frameID: frameID,
                  streamID: currentStreamID,
                  width: currentWidth,
                  height: currentHeight,
                  configVersion: version,
                  emitsConfiguration: emitsConfiguration
              )
        else { return }
        for event in events { publish(event) }
    }

    private func publish(_ event: EncodedVideoEvent) {
        eventQueue.async { [weak self] in self?.onEvent?(event) }
    }

    private func configure(session: VTCompressionSession, width: Int, height: Int) {
        let bitrate = max(1_000_000, min(12_000_000, width * height * 4))
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate, value: bitrate as CFTypeRef)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: 30 as CFTypeRef)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: 60 as CFTypeRef)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_High_AutoLevel)
    }

    private static func format(from description: CMFormatDescription) -> H264EncoderFormat? {
        var spsPointer: UnsafePointer<UInt8>?
        var spsSize = 0
        var parameterSetCount = 0
        var nalHeaderLength: Int32 = 0
        let spsStatus = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            description,
            parameterSetIndex: 0,
            parameterSetPointerOut: &spsPointer,
            parameterSetSizeOut: &spsSize,
            parameterSetCountOut: &parameterSetCount,
            nalUnitHeaderLengthOut: &nalHeaderLength
        )
        var ppsPointer: UnsafePointer<UInt8>?
        var ppsSize = 0
        let ppsStatus = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            description,
            parameterSetIndex: 1,
            parameterSetPointerOut: &ppsPointer,
            parameterSetSizeOut: &ppsSize,
            parameterSetCountOut: nil,
            nalUnitHeaderLengthOut: nil
        )
        guard spsStatus == noErr, ppsStatus == noErr,
              let spsPointer, let ppsPointer,
              parameterSetCount >= 2, nalHeaderLength == 4
        else { return nil }
        return H264EncoderFormat(
            sps: Data(bytes: spsPointer, count: spsSize),
            pps: Data(bytes: ppsPointer, count: ppsSize)
        )
    }

    private static func data(from blockBuffer: CMBlockBuffer) -> Data? {
        var dataLength = 0
        var pointer: UnsafeMutablePointer<Int8>?
        guard CMBlockBufferGetDataPointer(
            blockBuffer,
            atOffset: 0,
            lengthAtOffsetOut: nil,
            totalLengthOut: &dataLength,
            dataPointerOut: &pointer
        ) == noErr,
              let pointer,
              dataLength > 0
        else { return nil }
        return Data(bytes: pointer, count: dataLength)
    }
}

private func macH264EncoderOutputCallback(
    _ outputCallbackRefCon: UnsafeMutableRawPointer?,
    _ sourceFrameRefCon: UnsafeMutableRawPointer?,
    _ status: OSStatus,
    _ infoFlags: VTEncodeInfoFlags,
    _ sampleBuffer: CMSampleBuffer?
) {
    guard status == noErr, let outputCallbackRefCon, let sampleBuffer else { return }
    Unmanaged<MacH264Encoder>
        .fromOpaque(outputCallbackRefCon)
        .takeUnretainedValue()
        .accept(sampleBuffer: sampleBuffer)
}
