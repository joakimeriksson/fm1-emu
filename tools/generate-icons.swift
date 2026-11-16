// Regenerate on macOS: swift tools/generate-icons.swift (AppKit + iconutil, no packages).
import AppKit
import Foundation
import ImageIO
let root = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
let fm = FileManager.default
let source = NSImage(contentsOf: root.appendingPathComponent("assets/icons/controls.png"))!
func png(_ relative: String, _ size: Int, opaque: Bool = false, maskable: Bool = false) throws {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let context = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size*4, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    context.interpolationQuality = .high
    context.setFillColor(CGColor(colorSpace: space, components: [41.0/255, 42.0/255, 43.0/255, 1])!)
    if opaque || maskable {
        context.fill(CGRect(x: 0, y: 0, width: size, height: size))
    } else {
        let margin = Double(size)*0.03
        let shape = CGPath(roundedRect: CGRect(x: margin, y: margin, width: Double(size)-2*margin, height: Double(size)-2*margin), cornerWidth: Double(size)*0.19, cornerHeight: Double(size)*0.19, transform: nil)
        context.addPath(shape); context.fillPath()
        context.addPath(shape); context.clip()
    }
    let inset = maskable ? Double(size)*0.22 : 0
    context.draw(source.cgImage(forProposedRect: nil, context: nil, hints: nil)!, in: CGRect(x: inset, y: inset, width: Double(size)-2*inset, height: Double(size)-2*inset))
    let url = root.appendingPathComponent(relative)
    try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    let destination = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, context.makeImage()!, nil)
    precondition(CGImageDestinationFinalize(destination))
}

for size in [16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512, 1024] { try png("assets/icons/png/fm1-\(size).png", size) }
for size in [16,32,48] { try png("web/icons/favicon-\(size).png",size) }
for size in [120,152,167,180] { try png("web/icons/apple-touch-icon-\(size).png",size,opaque:true) }
for size in [192,512] {
 try png("web/icons/icon-\(size).png",size)
 try png("web/icons/maskable-\(size).png",size,opaque:true,maskable:true)
}
for size in [16,32,128,256,512] {
 for scale in [1,2] {
  let suffix = scale == 2 ? "@2x" : ""
  try png("assets/icons/FM1.iconset/icon_\(size)x\(size)\(suffix).png",size*scale)
 }
}
// ICO directory with lossless PNG frames, supported by modern Windows and browsers.
func ico(_ path: String, _ sizes: [Int]) throws {
 var output = Data([0,0,1,0,UInt8(sizes.count),0]); var frames = [Data]()
 var offset = 6 + 16*sizes.count
 func le(_ value: Int, _ bytes: Int) -> [UInt8] { (0..<bytes).map { UInt8((value >> ($0*8)) & 255) } }
 for size in sizes {
  let data = try Data(contentsOf: root.appendingPathComponent("assets/icons/png/fm1-\(size).png"))
  output.append(contentsOf: [UInt8(size == 256 ? 0 : size),UInt8(size == 256 ? 0 : size),0,0,1,0,32,0])
  output.append(contentsOf: le(data.count,4)); output.append(contentsOf: le(offset,4))
  offset += data.count; frames.append(data)
 }
 frames.forEach { output.append($0) }; try output.write(to: root.appendingPathComponent(path))
}
try ico("assets/icons/FM1.ico",[16,20,24,32,40,48,64,96,128,256])
try ico("web/favicon.ico",[16,32,48])
let process = Process(); process.executableURL = URL(fileURLWithPath:"/usr/bin/iconutil")
process.arguments = ["-c","icns","assets/icons/FM1.iconset","-o","assets/icons/FM1.icns"]
try process.run(); process.waitUntilExit(); precondition(process.terminationStatus == 0)
// X11 _NET_WM_ICON payload: dimensions followed by straight ARGB pixels.
let linuxImage = NSBitmapImageRep(data: try Data(contentsOf: root.appendingPathComponent("assets/icons/png/fm1-64.png")))!
var argb = Data()
for y in 0..<64 { for x in 0..<64 {
 let c = linuxImage.colorAt(x: x, y: y)!.usingColorSpace(.sRGB)!
 argb.append(contentsOf: [UInt8((c.blueComponent*255).rounded()), UInt8((c.greenComponent*255).rounded()), UInt8((c.redComponent*255).rounded()), UInt8((c.alphaComponent*255).rounded())])
} }
try argb.write(to: root.appendingPathComponent("assets/icons/linux-64.argb"))
let favicon = try Data(contentsOf: root.appendingPathComponent("web/icons/favicon-32.png"))
try ("data:image/png;base64," + favicon.base64EncodedString()).write(to: root.appendingPathComponent("assets/icons/favicon-data-uri.txt"), atomically: true, encoding: .utf8)
print("Generated native, favicon, Apple touch and maskable icons.")

// Compose uniform sRGB body + transparent controls into full-bleed artwork.
// Opaque canvas preserves layout bounds when Icon Composer imports the image.
// No baked outer bevel, rounded tile or inset: macOS supplies the enclosure.
try png("assets/icons/source.png", 1024, opaque: true)
try Data(contentsOf: root.appendingPathComponent("assets/icons/source.png")).write(to: root.appendingPathComponent("assets/icons/FM1.icon/Assets/device.png"))
let macOutput = root.appendingPathComponent("assets/icons/macos")
try fm.createDirectory(at: macOutput, withIntermediateDirectories: true)
try fm.createDirectory(at: root.appendingPathComponent("target"), withIntermediateDirectories: true)
let compiler = Process(); compiler.executableURL = URL(fileURLWithPath: "/usr/bin/xcrun")
compiler.arguments = ["actool", "assets/icons/FM1.icon", "--compile", macOutput.path, "--app-icon", "FM1", "--output-partial-info-plist", root.appendingPathComponent("target/icon-partial.plist").path, "--platform", "macosx", "--target-device", "mac", "--minimum-deployment-target", "11.0"]
try compiler.run(); compiler.waitUntilExit(); precondition(compiler.terminationStatus == 0)
