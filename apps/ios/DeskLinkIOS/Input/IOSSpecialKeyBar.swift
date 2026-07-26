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
                        Text(key.displayTitle)
                            .font(.caption.weight(.medium))
                            .lineLimit(1)
                            .minimumScaleFactor(0.75)
                            .frame(minWidth: key == .command ? 72 : 50, minHeight: 36)
                    }
                    .buttonStyle(.bordered)
                    .tint(key.modifier.map { keyboard.activeModifiers.contains($0) } == true ? .accentColor : nil)
                    .accessibilityLabel(key.rawValue)
                }
            }
            .padding(.horizontal, 12)
        }
        .frame(minHeight: 42)
    }
}
