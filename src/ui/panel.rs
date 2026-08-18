//! The floating panel.
//!
//! It appears under the menu bar icon, takes keyboard focus, and closes on
//! escape or on focus loss. It is a panel rather than a window so it can float
//! above other applications without a title bar or a Dock entry.

use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSPanel, NSScreen, NSTextField, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::state::UiState;

use super::roster_view::{Action, RosterView};
use super::style;

/// Carries the field's Return key to a closure.
pub struct FieldIvars {
    on_submit: Rc<dyn Fn()>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - FieldTarget does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WalkieFieldTarget"]
    #[ivars = FieldIvars]
    struct FieldTarget;

    unsafe impl NSObjectProtocol for FieldTarget {}

    impl FieldTarget {
        /// An `NSTextField` sends its action on Return, which is exactly the
        /// moment a pasted key should be added.
        #[unsafe(method(submit:))]
        fn submit(&self, _sender: Option<&AnyObject>) {
            (self.ivars().on_submit)();
        }
    }
);

impl FieldTarget {
    fn new(mtm: MainThreadMarker, on_submit: Rc<dyn Fn()>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(FieldIvars { on_submit });
        unsafe { msg_send![super(this), init] }
    }
}

/// Everything the panel owns.
pub struct Panel {
    window: Retained<NSPanel>,
    roster: Retained<RosterView>,
    field: Retained<NSTextField>,
    /// Kept alive for as long as the field can fire.
    _field_target: Retained<FieldTarget>,
    mtm: MainThreadMarker,
}

impl Panel {
    /// Builds the panel. It stays hidden until `show` is called.
    pub fn new(mtm: MainThreadMarker, actions: Rc<dyn Fn(Action)>) -> Self {
        let submit_actions = actions.clone();
        // The window is larger than the content by the shadow offset. The
        // reference style draws a hard stipple shadow down and to the right,
        // and it needs somewhere to land.
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(
                style::PANEL_WIDTH + style::SHADOW_OFFSET,
                style::PANEL_HEIGHT + style::SHADOW_OFFSET,
            ),
        );

        // Borderless, because the whole point of the style is that the border
        // is ours. `NonactivatingPanel` would refuse key focus, and the panel
        // exists to take digits, so it is not used.
        let style_mask = NSWindowStyleMask::Borderless;

        let window = {
            NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                frame,
                style_mask,
                NSBackingStoreType::Buffered,
                false,
            )
        };

        {
            window.setLevel(objc2_app_kit::NSFloatingWindowLevel);
            // Transparent, so the shadow margin shows what is behind it and the
            // stipple reads as a shadow rather than a grey band.
            window.setOpaque(false);
            window.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
            // The system's soft drop shadow would sit under our hard one and
            // undo the whole style.
            window.setHasShadow(false);
            window.setHidesOnDeactivate(false);
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            // A borderless window refuses key focus unless it is asked to
            // accept it. Without this, digits go nowhere.
            window.setMovableByWindowBackground(true);
        }

        let roster = RosterView::new(mtm, actions.clone());

        let field = {
            let field = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                // Placed by `layout`, which knows the panel's current height.
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
            );
            field.setPlaceholderString(Some(&NSString::from_str("search, or paste a wt1 key")));
            field.setFont(Some(&style::mono(style::SIZE_BODY)));
            // AppKit's own bezel is a soft grey rounded rectangle. The roster
            // draws a 2 px square border behind the field instead, so the
            // field matches every other element.
            field.setBordered(false);
            field.setBezeled(false);
            // A single-line field draws its text at the top of its frame. The
            // frame is therefore sized to the text and centred inside the box,
            // rather than filling the box and leaving the text stranded.
            field.setUsesSingleLineMode(true);
            field.setBackgroundColor(Some(&style::card()));
            field.setTextColor(Some(&style::ink()));
            field.setFocusRingType(objc2_app_kit::NSFocusRingType::None);
            field.setDrawsBackground(true);
            field
        };

        let content = window
            .contentView()
            .expect("a panel always has a content view");
        let field_target = FieldTarget::new(mtm, Rc::new(move || submit_actions(Action::Submit)));
        unsafe {
            field.setTarget(Some(&field_target));
            field.setAction(Some(sel!(submit:)));
        }

        content.addSubview(&roster);
        content.addSubview(&field);

        let panel = Panel {
            window,
            roster,
            field,
            _field_target: field_target,
            mtm,
        };
        panel.layout(style::PANEL_HEIGHT);
        panel
    }

    /// Places the subviews for a given panel height.
    ///
    /// The panel resizes to its content, so this runs on every show rather than
    /// once at construction.
    fn layout(&self, window_height: f64) {
        let width = style::PANEL_WIDTH;

        self.roster.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width + style::SHADOW_OFFSET, window_height),
        ));

        // The roster draws the field's border, so the field itself sits inside
        // it. Its height is the text height, not the box height, and it is
        // centred in the box. A single-line NSTextField draws its text at the
        // top of whatever frame it is given, so a full-height frame would leave
        // the text stranded against the top edge.
        let text_height = style::mono(style::SIZE_BODY)
            .boundingRectForFont()
            .size
            .height;
        let text_height = text_height.clamp(14.0, style::FIELD_HEIGHT - style::BORDER * 2.0);

        let box_top = style::MARGIN + style::HEADER_HEIGHT;
        let box_bottom_from_top = box_top + style::FIELD_HEIGHT;
        let centred =
            window_height - box_bottom_from_top + (style::FIELD_HEIGHT - text_height) / 2.0;

        self.field.setFrame(NSRect::new(
            NSPoint::new(style::MARGIN + 8.0, centred),
            NSSize::new(width - style::MARGIN * 2.0 - 16.0, text_height),
        ));
    }

    /// Renders the panel to a PNG.
    ///
    /// The interface can then be checked without a screenshot, which matters
    /// because a terminal without the screen recording permission captures the
    /// desktop with every window missing, and because it makes the visual
    /// design reviewable in a loop.
    pub fn write_png(&self, path: &std::path::Path) -> crate::error::Result<()> {
        use objc2_app_kit::NSBitmapImageFileType;

        let content = self.window.contentView().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("the panel has no content"))
        })?;

        let bounds = content.bounds();

        let rep = content
            .bitmapImageRepForCachingDisplayInRect(bounds)
            .ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!("cannot make a bitmap for the panel"))
            })?;

        content.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);

        let data = unsafe {
            rep.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &objc2_foundation::NSDictionary::new(),
            )
        }
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("cannot encode the panel")))?;

        let bytes = data.to_vec();
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Shows the panel just under a point on the screen, usually the menu bar
    /// icon.
    ///
    /// The anchor is not trusted. `NSStatusItem` reports a button window at
    /// `y = -24` before the menu bar has laid it out, and a user whose status
    /// item sits in an overflow area gets an off-screen point for the life of
    /// the process. Either way the panel would be placed where nobody can see
    /// it, so the result is always clamped to the visible screen.
    pub fn show(&self, anchor: Option<NSPoint>) {
        let visible = self.visible_frame();
        self.resize_to_content(visible);

        let wanted = anchor
            .filter(|point| Self::anchor_is_usable(*point, visible))
            .map(|point| {
                NSPoint::new(
                    point.x - style::PANEL_WIDTH / 2.0,
                    point.y - self.height() - 6.0,
                )
            })
            .unwrap_or_else(|| self.under_menu_bar(visible));

        let origin = self.clamp(wanted, visible);

        self.window.setFrameOrigin(origin);
        // `makeKeyAndOrderFront` alone does nothing when the application is not
        // active, and an accessory application often is not. Ordering front
        // regardless is what actually puts the panel on screen.
        self.window.orderFrontRegardless();
        self.window.makeKeyAndOrderFront(None);
        tracing::debug!(
            x = origin.x,
            y = origin.y,
            anchored = anchor.is_some(),
            "panel placed"
        );
        // The roster, not the field, takes the keys. Digits must reach the
        // session rather than the search box.
        self.window.makeFirstResponder(Some(&self.roster));
    }

    /// Sizes the panel to what it actually has to show.
    ///
    /// A roster of two people in a panel built for nine is mostly empty space,
    /// and empty space in a small floating panel reads as a fault.
    fn resize_to_content(&self, screen: NSRect) {
        let content = self.roster.content_height().clamp(
            style::PANEL_MIN_HEIGHT,
            screen.size.height - style::MARGIN * 2.0,
        );
        let wanted = content + style::SHADOW_OFFSET;

        let current = self.window.frame();
        if (current.size.height - wanted).abs() < 1.0 {
            return;
        }

        self.window.setFrame_display(
            NSRect::new(
                current.origin,
                NSSize::new(style::PANEL_WIDTH + style::SHADOW_OFFSET, wanted),
            ),
            false,
        );
        self.layout(wanted);
    }

    /// The panel's current height, so callers can place it.
    pub fn height(&self) -> f64 {
        self.window.frame().size.height
    }

    /// True when a point sits inside the screen and can anchor the panel.
    fn anchor_is_usable(point: NSPoint, screen: NSRect) -> bool {
        point.y > screen.origin.y
            && point.y <= screen.origin.y + screen.size.height
            && point.x >= screen.origin.x
            && point.x <= screen.origin.x + screen.size.width
    }

    /// The default position: the top right, where the menu bar icon lives.
    fn under_menu_bar(&self, screen: NSRect) -> NSPoint {
        NSPoint::new(
            screen.origin.x + screen.size.width - style::PANEL_WIDTH - style::MARGIN,
            screen.origin.y + screen.size.height - self.height() - style::MARGIN,
        )
    }

    /// Keeps the whole panel on screen.
    fn clamp(&self, origin: NSPoint, screen: NSRect) -> NSPoint {
        let max_x = screen.origin.x + screen.size.width - style::PANEL_WIDTH;
        let max_y = screen.origin.y + screen.size.height - self.height();

        NSPoint::new(
            origin.x.clamp(screen.origin.x, max_x.max(screen.origin.x)),
            origin.y.clamp(screen.origin.y, max_y.max(screen.origin.y)),
        )
    }

    /// The part of the screen not covered by the menu bar or the Dock.
    fn visible_frame(&self) -> NSRect {
        match NSScreen::mainScreen(self.mtm) {
            Some(screen) => screen.visibleFrame(),
            // No screen at all. Any value works, because nothing will be seen.
            None => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)),
        }
    }

    pub fn hide(&self) {
        self.window.orderOut(None);
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn toggle(&self, anchor: Option<NSPoint>) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show(anchor);
        }
    }

    /// Publishes a new snapshot to the roster, and applies the current filter.
    pub fn set_state(&self, state: UiState) {
        self.roster.set_filter(&self.field_text());
        self.roster.set_state(state);
    }

    /// Moves keyboard focus to the search and add field.
    pub fn focus_field(&self) {
        self.window.makeFirstResponder(Some(&self.field));
    }

    /// Moves keyboard focus back to the roster, so digits work again.
    pub fn focus_roster(&self) {
        self.window.makeFirstResponder(Some(&self.roster));
    }

    /// The current contents of the field.
    pub fn field_text(&self) -> String {
        self.field.stringValue().to_string()
    }

    pub fn clear_field(&self) {
        self.field.setStringValue(&NSString::from_str(""));
    }
}
