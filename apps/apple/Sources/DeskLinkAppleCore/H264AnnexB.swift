import Foundation

public enum H264AnnexBError: Error, Equatable {
    case noNALUnits
    case missingParameterSets
    case oversizedNALUnit
}

public struct H264ParameterSets: Equatable {
    public let sps: Data
    public let pps: Data
}

public enum H264AnnexB {
    public static func parameterSets(in sequenceHeader: Data) throws -> H264ParameterSets {
        let units = nalUnits(in: sequenceHeader)
        guard !units.isEmpty else { throw H264AnnexBError.noNALUnits }
        guard let sps = units.first(where: { nalType($0) == 7 }),
              let pps = units.first(where: { nalType($0) == 8 })
        else {
            throw H264AnnexBError.missingParameterSets
        }
        return H264ParameterSets(sps: sps, pps: pps)
    }

    public static func avccAccessUnit(from annexB: Data) throws -> Data {
        let ranges = nalRanges(in: annexB)
        guard !ranges.isEmpty else { throw H264AnnexBError.noNALUnits }
        var output = Data()
        output.reserveCapacity(annexB.count + ranges.count)
        try annexB.withUnsafeBytes { rawBytes in
            let bytes = rawBytes.bindMemory(to: UInt8.self)
            for range in ranges {
                guard let length = UInt32(exactly: range.count) else {
                    throw H264AnnexBError.oversizedNALUnit
                }
                output.append(UInt8(truncatingIfNeeded: length >> 24))
                output.append(UInt8(truncatingIfNeeded: length >> 16))
                output.append(UInt8(truncatingIfNeeded: length >> 8))
                output.append(UInt8(truncatingIfNeeded: length))
                output.append(contentsOf: bytes[range])
            }
        }
        return output
    }

    public static func nalUnits(in annexB: Data) -> [Data] {
        nalRanges(in: annexB).map { range in
            annexB.subdata(in: (annexB.startIndex + range.lowerBound)..<(annexB.startIndex + range.upperBound))
        }
    }

    public static func containsIDR(in annexB: Data) -> Bool {
        let ranges = nalRanges(in: annexB)
        return annexB.withUnsafeBytes { rawBytes in
            let bytes = rawBytes.bindMemory(to: UInt8.self)
            return ranges.contains { bytes[$0.lowerBound] & 0x1f == 5 }
        }
    }

    /// Offsets into the original Data. Scanning never materializes a byte array
    /// or a Data for every NAL; conversion copies payloads only into its output.
    static func nalRanges(in annexB: Data) -> [Range<Int>] {
        annexB.withUnsafeBytes { rawBytes in
            let bytes = rawBytes.bindMemory(to: UInt8.self)
            var ranges: [Range<Int>] = []
            var payloadStart: Int?
            var index = 0
            while index + 2 < bytes.count {
                let markerLength: Int
                if index + 3 < bytes.count, bytes[index] == 0,
                   bytes[index + 1] == 0, bytes[index + 2] == 0, bytes[index + 3] == 1 {
                    markerLength = 4
                } else if bytes[index] == 0, bytes[index + 1] == 0, bytes[index + 2] == 1 {
                    markerLength = 3
                } else {
                    index += 1
                    continue
                }
                if let start = payloadStart {
                    var end = index
                    while end > start, bytes[end - 1] == 0 { end -= 1 }
                    if start < end { ranges.append(start..<end) }
                }
                payloadStart = index + markerLength
                index += markerLength
            }
            if let start = payloadStart {
                var end = bytes.count
                while end > start, bytes[end - 1] == 0 { end -= 1 }
                if start < end { ranges.append(start..<end) }
            }
            return ranges
        }
    }

    private static func nalType(_ unit: Data) -> UInt8? {
        unit.first.map { $0 & 0x1f }
    }
}
