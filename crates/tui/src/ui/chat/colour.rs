//! A speaker's colour: a hash of its handle, not its place in the list, so a message keeps the
//! colour it had when somebody above it joins and a renamed session keeps the colour too.

use rano::style::Role as Paint;

/// How many hues a speaker can speak in - the palette the room's design named, and the whole of
/// what `hue:` may say.
pub(crate) const HUES: u8 = 8;

/// The eight, said as hues rather than severities: a speaker's colour says *who*, and never that
/// something needs a person or failed.
const HUE_PAINTS: [Paint; HUES as usize] = [
    Paint::Keyword,    // magenta
    Paint::Code,       // cyan
    Paint::StringLit,  // green
    Paint::NumberLit,  // yellow
    Paint::DiagError,  // red, and not `Failure`: nobody failed, this is a hue
    Paint::FuncName,   // blue
    Paint::Heading,    // cyan, bold
    Paint::Subheading, // blue, bold
];

/// The hue a handle speaks in: FNV-1a 32 of the lowercased handle, mod eight. The handle and not
/// the display name, which is why a rename leaves the colour where it was.
pub(crate) fn hue(handle: &str) -> u8 {
    let mut h: u32 = 0x811c_9dc5;
    for b in handle.to_ascii_lowercase().bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    (h % u32::from(HUES)) as u8
}

/// The paint a hue is drawn in. A hue past the palette wraps rather than panicking: a layer's
/// `hue: 200` is a mistake, and the last colour in the table is at least a colour somebody chose.
pub(crate) fn paint(hue: u8) -> Paint {
    HUE_PAINTS[usize::from(hue) % HUE_PAINTS.len()]
}
