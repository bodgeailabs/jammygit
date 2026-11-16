// macOS-only developer utility. Draws exported Ratatui cells, without mocking the UI.
// JAMMYGIT_PREVIEW_DIR=/tmp/jammygit-preview cargo test export_preview -- --ignored
// swift scripts/render_preview.swift /tmp/jammygit-preview/workspace.cells docs/preview.png
import AppKit

let input = try String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8)
let lines = input.components(separatedBy: "\n")
let dimensions = lines[0].components(separatedBy: "\t").map { Int($0)! }
let cellWidth = 9.0
let cellHeight = 20.0
let size = NSSize(width: Double(dimensions[0]) * cellWidth, height: Double(dimensions[1]) * cellHeight)
let image = NSImage(size: size)
let cells = lines.dropFirst().filter { !$0.isEmpty }.map { $0.components(separatedBy: "\t") }
func color(_ value: String) -> NSColor {
    let rgb = value.split(separator: ",").map { Double($0)! / 255.0 }
    return NSColor(srgbRed: rgb[0], green: rgb[1], blue: rgb[2], alpha: 1)
}
image.lockFocusFlipped(true)
for cell in cells {
    color(cell[3]).setFill()
    NSRect(x: Double(cell[0])! * cellWidth, y: Double(cell[1])! * cellHeight, width: cellWidth, height: cellHeight).fill()
}
for cell in cells where cell.count >= 6 && cell[5] != " " {
    let font = NSFont(name: cell[4] == "1" ? "Menlo-Bold" : "Menlo-Regular", size: 14)!
    (cell[5] as NSString).draw(at: NSPoint(x: Double(cell[0])! * cellWidth, y: Double(cell[1])! * cellHeight + 1),
        withAttributes: [.font: font, .foregroundColor: color(cell[2])])
}
image.unlockFocus()
let bitmap = NSBitmapImageRep(data: image.tiffRepresentation!)!
try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[2]))
