//! The visual language.
//!
//! The style comes from `reference/aesthetic-neobrutalist-mono.png`. Read
//! `DESIGN.md` §6.3 before changing a value here. The rules are strict on
//! purpose, because the whole interface is six screens' worth of rectangles and
//! the only thing holding it together is consistency.
//!
//! 1. Every border is 2 px and solid ink.
//! 2. Every corner is square.
//! 3. Every font is monospace.
//! 4. A section label sits in a gap in its border line.
//! 5. A raised element gets a hard offset shadow, never a blur.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontWeightBold, NSFontWeightRegular, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyle, NSStringDrawing, NSTextAlignment,
};
use objc2_foundation::{NSAttributedStringKey, NSDictionary, NSPoint, NSRect, NSSize, NSString};

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Every border in the interface.
pub const BORDER: f64 = 2.0;

/// The offset of a hard shadow, right and down.
pub const SHADOW_OFFSET: f64 = 6.0;

/// The panel width. The height follows the content.
pub const PANEL_WIDTH: f64 = 420.0;

/// The height the panel starts at, before it has measured its content.
pub const PANEL_HEIGHT: f64 = 420.0;

/// The panel never shrinks below this, so an empty roster still looks like a
/// panel rather than a sliver.
pub const PANEL_MIN_HEIGHT: f64 = 190.0;

/// The height of the title row above the search field.
pub const HEADER_HEIGHT: f64 = 30.0;

/// The outer margin inside the panel.
pub const MARGIN: f64 = 16.0;

/// One roster row.
pub const ROW_HEIGHT: f64 = 44.0;

/// The square that holds a slot number.
pub const SLOT_BOX: f64 = 28.0;

/// The search and add field.
pub const FIELD_HEIGHT: f64 = 34.0;

/// The box that shows your own key. Two lines of key, plus the copy hint.
pub const KEY_BOX_HEIGHT: f64 = 58.0;

/// How many key characters fit on one line inside the key box.
///
/// A key is 63 characters, which does not fit the panel width on one line at
/// the label size. Truncating it would be worse than useless: a key you cannot
/// read in full is a key you cannot pass on.
pub const KEY_CHARS_PER_LINE: usize = 32;

/// The gap between a section label and its rule.
pub const LABEL_GAP: f64 = 8.0;

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// Borders and text.
pub fn ink() -> Retained<NSColor> {
    srgb(0x11, 0x13, 0x18, 1.0)
}

/// The window background.
pub fn paper() -> Retained<NSColor> {
    srgb(0xF7, 0xF8, 0xFA, 1.0)
}

/// An input field.
pub fn card() -> Retained<NSColor> {
    srgb(0xFF, 0xFF, 0xFF, 1.0)
}

/// A button or a slot box.
pub fn control() -> Retained<NSColor> {
    srgb(0xE4, 0xE8, 0xEF, 1.0)
}

/// The only strong colour in the interface. It marks a live session and
/// nothing else.
pub fn live() -> Retained<NSColor> {
    srgb(0xE5, 0x48, 0x4D, 1.0)
}

/// The presence dot.
pub fn online() -> Retained<NSColor> {
    srgb(0x30, 0xA4, 0x6C, 1.0)
}

/// Text that is present but not the point.
pub fn muted_ink() -> Retained<NSColor> {
    srgb(0x11, 0x13, 0x18, 0.45)
}

/// The stipple used for a hard shadow.
pub fn stipple() -> Retained<NSColor> {
    srgb(0x11, 0x13, 0x18, 0.20)
}

fn srgb(r: u8, g: u8, b: u8, a: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        r as f64 / 255.0,
        g as f64 / 255.0,
        b as f64 / 255.0,
        a,
    )
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// The interface font. Monospace, always.
///
/// `monospacedSystemFontOfSize` gives SF Mono where it exists and falls back on
/// its own. Naming Menlo explicitly would pin an older face on machines that
/// have a better one.
pub fn mono(size: f64) -> Retained<NSFont> {
    unsafe { NSFont::monospacedSystemFontOfSize_weight(size, NSFontWeightRegular) }
}

/// The bold weight, for a name and for a section label.
pub fn mono_bold(size: f64) -> Retained<NSFont> {
    unsafe { NSFont::monospacedSystemFontOfSize_weight(size, NSFontWeightBold) }
}

pub const SIZE_BODY: f64 = 12.0;
pub const SIZE_LABEL: f64 = 10.0;
pub const SIZE_SLOT: f64 = 13.0;

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Fills a rectangle.
pub fn fill(rect: NSRect, color: &NSColor) {
    color.setFill();
    NSBezierPath::fillRect(rect);
}

/// Draws a 2 px border inside a rectangle.
///
/// The rectangle is inset by half the line width, because a stroke straddles
/// the path. Without the inset every border would be a blurred 3 px.
pub fn stroke(rect: NSRect, color: &NSColor) {
    let inset = NSRect::new(
        NSPoint::new(rect.origin.x + BORDER / 2.0, rect.origin.y + BORDER / 2.0),
        NSSize::new(
            (rect.size.width - BORDER).max(0.0),
            (rect.size.height - BORDER).max(0.0),
        ),
    );

    let path = NSBezierPath::bezierPathWithRect(inset);
    path.setLineWidth(BORDER);
    color.setStroke();
    path.stroke();
}

/// Fills a rectangle and puts a border on it.
pub fn box_filled(rect: NSRect, fill_color: &NSColor) {
    fill(rect, fill_color);
    stroke(rect, &ink());
}

/// Draws the hard offset shadow of a raised element.
///
/// The reference image uses a stipple, not a blur. Drawing it as a grid of dots
/// keeps the whole interface free of soft edges, which is the point of the
/// style.
///
/// The panel view is flipped, so "down" is a larger y.
pub fn hard_shadow(rect: NSRect) {
    let shadow = NSRect::new(
        NSPoint::new(rect.origin.x + SHADOW_OFFSET, rect.origin.y + SHADOW_OFFSET),
        rect.size,
    );

    stipple().setFill();

    // A 2 px grid, offset on alternate rows, reads as a halftone at this size.
    let step = 2.0;
    let mut y = shadow.origin.y;
    let mut row = 0usize;

    while y < shadow.origin.y + shadow.size.height {
        let mut x = shadow.origin.x
            + if row.is_multiple_of(2) {
                0.0
            } else {
                step / 2.0
            };
        while x < shadow.origin.x + shadow.size.width {
            // The part that sits under the element itself is not drawn.
            let inside_element =
                x < rect.origin.x + rect.size.width && y < rect.origin.y + rect.size.height;
            if !inside_element {
                NSBezierPath::fillRect(NSRect::new(NSPoint::new(x, y), NSSize::new(1.0, 1.0)));
            }
            x += step;
        }
        y += step;
        row += 1;
    }
}

/// Draws a horizontal rule.
pub fn rule(from: NSPoint, to: NSPoint, color: &NSColor) {
    let path = NSBezierPath::new();
    path.moveToPoint(from);
    path.lineToPoint(to);
    path.setLineWidth(BORDER);
    color.setStroke();
    path.stroke();
}

/// Draws a filled dot.
pub fn dot(center: NSPoint, radius: f64, color: &NSColor) {
    let rect = NSRect::new(
        NSPoint::new(center.x - radius, center.y - radius),
        NSSize::new(radius * 2.0, radius * 2.0),
    );
    let path = NSBezierPath::bezierPathWithOvalInRect(rect);
    color.setFill();
    path.fill();
}

/// The attribute dictionary for a run of text.
pub fn attributes(
    font: &NSFont,
    color: &NSColor,
    alignment: NSTextAlignment,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setAlignment(alignment);
    // A name longer than its column is cut, not wrapped. A wrapped roster row
    // would change height and break the grid.
    paragraph.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);

    let keys: [&NSAttributedStringKey; 3] = unsafe {
        [
            objc2_app_kit::NSFontAttributeName,
            objc2_app_kit::NSForegroundColorAttributeName,
            objc2_app_kit::NSParagraphStyleAttributeName,
        ]
    };

    // Every value is an Objective-C object, so the dictionary holds them as
    // `AnyObject`. The keys above say what each one must actually be.
    let values: [&AnyObject; 3] = [
        font.as_ref(),
        color.as_ref(),
        <NSMutableParagraphStyle as AsRef<NSParagraphStyle>>::as_ref(&paragraph).as_ref(),
    ];

    NSDictionary::from_slices(&keys, &values)
}

/// Draws text inside a rectangle.
pub fn text(value: &str, rect: NSRect, font: &NSFont, color: &NSColor, alignment: NSTextAlignment) {
    let string = NSString::from_str(value);
    let attrs = attributes(font, color, alignment);
    unsafe { string.drawInRect_withAttributes(rect, Some(&attrs)) };
}

/// Draws a section label sitting in a gap in a rule.
///
/// This is the signature detail of the reference image: the border line breaks
/// around the label rather than running behind it.
pub fn section(label: &str, origin: NSPoint, width: f64) {
    let font = mono_bold(SIZE_LABEL);
    let upper = label.to_uppercase();
    let string = NSString::from_str(&upper);

    let attrs = attributes(&font, &ink(), NSTextAlignment::Left);
    let size = unsafe { string.sizeWithAttributes(Some(&attrs)) };

    let text_rect = NSRect::new(
        NSPoint::new(origin.x, origin.y - size.height / 2.0),
        NSSize::new(size.width + 1.0, size.height),
    );
    unsafe { string.drawInRect_withAttributes(text_rect, Some(&attrs)) };

    let rule_start = origin.x + size.width + LABEL_GAP;
    let rule_end = origin.x + width;
    if rule_end > rule_start {
        rule(
            NSPoint::new(rule_start, origin.y),
            NSPoint::new(rule_end, origin.y),
            &ink(),
        );
    }
}

/// The label used when a peer has no name yet.
pub const PLACEHOLDER_NAME: &str = "(no name)";
