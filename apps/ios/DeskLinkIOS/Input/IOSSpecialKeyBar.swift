import SwiftUI

struct IOSSpecialKeyBar: View {
    @ObservedObject var keyboard: IOSKeyboardInput

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(IOSSpecialKey.allCases) { key in
                    Button {
                        if let modifier = key.modifier {
                            keyboard.sendSpecialKey(key, pressed: !keyboard.activeModifiers.contains(modifier))
                        } else {
                            keyboard.sendSpecialKey(key, pressed: true)
                            keyboard.sendSpecialKey(key, pressed: false)
                        }
                    } label: {
                        Text(key.rawValue)
                            .font(.caption.weight(.medium))
                            .lineLimit(1)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 8)
                    }
                    .buttonStyle(.bordered)
                    .tint(key.modifier.map { keyboard.activeModifiers.contains($0) } == true ? .accentColor : nil)
                }
            }
            .padding(.horizontal, 12)
        }
        .frame(minHeight: 42)
    }
}
