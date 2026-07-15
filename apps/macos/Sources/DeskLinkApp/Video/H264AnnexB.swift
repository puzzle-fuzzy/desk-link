import Foundation

enum H264AnnexBError: Error, Equatable {
    case noNALUnits
    case missingParameterSets
    case oversizedNALUnit
}

struct H264ParameterSets: Equatable {
    let sps: Data
    let pps: Data
}

enum H264AnnexB {
    static func parameterSets(in sequenceHeader: Data) throws -> H264ParameterSets {
        let units = nalUnits(in: sequenceHeader)
        guard !units.isEmpty else { throw H264AnnexBError.noNALUnits }
        guard let sps = units.first(where: { nalType($0) == 7 }),
              let pps = units.first(where: { nalType($0) == 8 })
        else {
            throw H264AnnexBError.missingParameterSets
        }
        return H264ParameterSets(sps: sps, pps: pps)
    }

    static func avccAccessUnit(from annexB: Data) throws -> Data {
        let units = nalUnits(in: annexB)
        guard !units.isEmpty else { throw H264AnnexBError.noNALUnits }
        var output = Data()
        output.reserveCapacity(annexB.count)
        for unit in units {
            guard let length = UInt32(exactly: unit.count) else {
                throw H264AnnexBError.oversizedNALUnit
            }
            output.append(UInt8(truncatingIfNeeded: length >> 24))
            output.append(UInt8(truncatingIfNeeded: length >> 16))
            output.append(UInt8(truncatingIfNeeded: length >> 8))
            output.append(UInt8(truncatingIfNeeded: length))
            output.append(unit)
        }
        return output
    }

    static func nalUnits(in annexB: Data) -> [Data] {
        let bytes = [UInt8](annexB)
        guard !bytes.isEmpty else { return [] }
        var markers: [(offset: Int, length: Int)] = []
        var index = 0
        while index + 2 < bytes.count {
            if index + 3 < bytes.count,
               bytes[index] == 0,
               bytes[index + 1] == 0,
               bytes[index + 2] == 0,
               bytes[index + 3] == 1
            {
                markers.append((index, 4))
                index += 4
            } else if bytes[index] == 0,
                      bytes[index + 1] == 0,
                      bytes[index + 2] == 1
            {
                markers.append((index, 3))
                index += 3
            } else {
                index += 1
            }
        }
        guard !markers.isEmpty else { return [] }

        var units: [Data] = []
        units.reserveCapacity(markers.count)
        for markerIndex in markers.indices {
            let start = markers[markerIndex].offset + markers[markerIndex].length
            var end = markerIndex + 1 < markers.count
                ? markers[markerIndex + 1].offset
                : bytes.count
            while end > start, bytes[end - 1] == 0 {
                end -= 1
            }
            if start < end {
                units.append(Data(bytes[start..<end]))
            }
        }
        return units
    }

    private static func nalType(_ unit: Data) -> UInt8? {
        unit.first.map { $0 & 0x1f }
    }
}
