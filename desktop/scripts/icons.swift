// Resize canonical brand PNGs so regeneration preserves the approved ribbon X.
import AppKit
import Foundation

let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let project = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    .deletingLastPathComponent().deletingLastPathComponent()
let assets = project.appendingPathComponent("assets/logos", isDirectory: true)
try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)

func image(_ size: Int) throws -> Data {
    let source = try Data(contentsOf: assets.appendingPathComponent("xrun-app-icon.png"))
    guard let bitmap = NSBitmapImageRep(data: source), let original = bitmap.cgImage else {
        throw NSError(domain: "xrun.icons", code: 1,
                      userInfo: [NSLocalizedDescriptionKey: "Cannot read the canonical app icon"])
    }
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
                        bytesPerRow: size * 4, space: CGColorSpaceCreateDeviceRGB(),
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.interpolationQuality = .high
    ctx.draw(original, in: CGRect(x: 0, y: 0, width: size, height: size))
    return NSBitmapImageRep(cgImage: ctx.makeImage()!).representation(using: .png, properties: [:])!
}

let set = root.appendingPathComponent("icon.iconset", isDirectory: true)
try FileManager.default.createDirectory(at: set, withIntermediateDirectories: true)
for size in [16, 32, 64, 128, 256, 512, 1024] {
    try image(size).write(to: root.appendingPathComponent("\(size).png"))
}
for size in [16, 32, 128, 256, 512] {
    try image(size).write(to: set.appendingPathComponent("icon_\(size)x\(size).png"))
    try image(size * 2).write(to: set.appendingPathComponent("icon_\(size)x\(size)@2x.png"))
}
try image(512).write(to: root.appendingPathComponent("icon.png"))
try Data(contentsOf: assets.appendingPathComponent("xrun-menubar@2x.png"))
    .write(to: root.appendingPathComponent("tray.png"))
