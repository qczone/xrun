// Rasterize the project's X mark with CoreGraphics; no image-generation service.
import AppKit
import Foundation

let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
func image(_ size: Int, template: Bool = false) throws -> Data {
    let space = CGColorSpaceCreateDeviceRGB()
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: CGFloat(size) / 200, y: CGFloat(size) / 200)
    ctx.translateBy(x: 0, y: 200); ctx.scaleBy(x: 1, y: -1)
    func stroke(_ x1: CGFloat, _ y1: CGFloat, _ x2: CGFloat, _ y2: CGFloat, _ colors: [CGColor]) {
        ctx.saveGState()
        ctx.setLineWidth(30.8); ctx.setLineCap(.round)
        ctx.move(to: CGPoint(x: x1, y: y1)); ctx.addLine(to: CGPoint(x: x2, y: y2))
        ctx.replacePathWithStrokedPath(); ctx.clip()
        let gradient = CGGradient(colorsSpace: space, colors: colors as CFArray, locations: [0, 1])!
        ctx.drawLinearGradient(gradient, start: CGPoint(x: x1, y: y1), end: CGPoint(x: x2, y: y2), options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
        ctx.restoreGState()
    }
    let black = CGColor(red: 0, green: 0, blue: 0, alpha: 1)
    stroke(149, 45.4, 51, 154.6, template ? [black, black] : [CGColor(red: 184/255, green: 245/255, blue: 90/255, alpha: 1), CGColor(red: 31/255, green: 203/255, blue: 107/255, alpha: 1)])
    stroke(51, 45.4, 149, 154.6, template ? [black, black] : [CGColor(red: 62/255, green: 224/255, blue: 1, alpha: 1), CGColor(red: 47/255, green: 107/255, blue: 1, alpha: 1)])
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
try image(44, template: true).write(to: root.appendingPathComponent("tray.png"))
