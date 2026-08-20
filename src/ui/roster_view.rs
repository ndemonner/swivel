//! The roster, drawn by hand.
//!
//! Everything here is a rectangle with a 2 px border and monospace text. See
//! `DESIGN.md` §6.2 for the layout and `reference/roster-layout.png` for the
//! structure it came from.
//!
//! The view is **flipped**, so the origin is the top left and a larger y is
//! further down. A roster is a top-down list, and fighting Cocoa's default
//! bottom-left origin for every row is not worth it.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSTextAlignment, NSView};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use crate::state::{MicState, PathKind, PeerView, UiState};

use super::style;

/// What the roster asks the core to do.
#[derive(Debug, Clone)]
pub enum Action {
    /// A digit was pressed. Add or remove that contact.
    ToggleSlot(u8),
    /// End the session.
    EndSession,
    /// Approve the first waiting endpoint.
    ApproveFirstKnock,
    /// Reject the first waiting endpoint.
    RejectFirstKnock,
    /// Move keyboard focus to the search and add field.
    FocusField,
    /// Close the panel without touching the session.
    Close,
    /// Return was pressed in the search and add field.
    Submit,
    /// Put your own key on the clipboard.
    CopyKey,
}

/// The data the view draws, and the channel it reports to.
pub struct RosterIvars {
    pub state: RefCell<UiState>,
    /// What the user has typed. An empty filter shows everyone.
    pub filter: RefCell<String>,
    pub actions: Rc<dyn Fn(Action)>,
}

define_class!(
    // SAFETY:
    // - NSView has no subclassing requirement beyond the main thread.
    // - RosterView does not implement Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SwivelRosterView"]
    #[ivars = RosterIvars]
    pub struct RosterView;

    impl RosterView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        /// The origin is the top left. A roster reads downwards.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The view takes keystrokes, so digits reach the session rather than
        /// the text field.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.handle_key(event);
        }

        /// Clicking the roster takes focus back from the search field, so
        /// digits work again without pressing escape first.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
        }

        /// Accept the click that activates the application, rather than
        /// swallowing it. Otherwise the first click after the panel appears
        /// does nothing.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }
    }
);

/// The y at which the roster starts, below the header and the field.
const BODY_TOP: f64 = style::MARGIN + style::HEADER_HEIGHT + style::FIELD_HEIGHT + 16.0;

/// The peers the filter lets through.
///
/// A `sv1` key in the field is not a search, so it hides nobody. The field does
/// two jobs and this is where they part.
fn visible_peers<'a>(peers: &'a [PeerView], filter: &str) -> Vec<&'a PeerView> {
    if filter.is_empty() || filter.starts_with(crate::config::TICKET_PREFIX) {
        return peers.iter().collect();
    }

    peers
        .iter()
        .filter(|p| {
            p.name.to_lowercase().contains(filter)
                || p.slot.is_some_and(|s| s.to_string() == filter)
        })
        .collect()
}

/// The roster in the order it is drawn: a section label and its rows, with the
/// empty sections left out.
///
/// `draw` and `content_height_for` both read this. A group cannot be measured
/// as one set and drawn as another.
fn groups<'a>(shown: &[&'a PeerView]) -> Vec<(&'static str, Vec<&'a PeerView>)> {
    let pick = |keep: fn(&PeerView) -> bool| -> Vec<&'a PeerView> {
        shown.iter().copied().filter(|p| keep(p)).collect()
    };

    [
        ("live", pick(|p| p.live)),
        ("online", pick(|p| p.online && !p.live)),
        ("offline", pick(|p| !p.online)),
    ]
    .into_iter()
    .filter(|(_, group)| !group.is_empty())
    .collect()
}

/// The height a roster needs, so the panel can size itself to it.
///
/// It must match `draw` step for step. A mismatch shows as a gap at the bottom
/// or a clipped last row.
fn content_height_for(state: &UiState, filter: &str) -> f64 {
    let mut height = BODY_TOP;

    if !state.knocks.is_empty() {
        height += 14.0 + state.knocks.len().min(3) as f64 * (style::ROW_HEIGHT - 4.0) + 10.0;
    }

    let shown = visible_peers(&state.peers, filter);

    if shown.is_empty() {
        if !filter.is_empty() && !state.peers.is_empty() {
            return height + 40.0 + style::MARGIN;
        }
        // The empty state carries the key box.
        return height + 12.0 + 26.0 + style::KEY_BOX_HEIGHT + 14.0 + 26.0 + style::MARGIN;
    }

    for (_, group) in groups(&shown) {
        height += 14.0 + group.len() as f64 * style::ROW_HEIGHT + 8.0;
    }

    height + style::MARGIN
}

impl RosterView {
    pub fn new(mtm: MainThreadMarker, actions: Rc<dyn Fn(Action)>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RosterIvars {
            state: RefCell::new(UiState::default()),
            filter: RefCell::new(String::new()),
            actions,
        });

        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(style::PANEL_WIDTH, style::PANEL_HEIGHT),
        );
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Replaces the snapshot and asks for a redraw.
    pub fn set_state(&self, state: UiState) {
        *self.ivars().state.borrow_mut() = state;
        self.setNeedsDisplay(true);
    }

    /// Sets the roster filter. Redraws only when it actually changed.
    pub fn set_filter(&self, filter: &str) {
        let trimmed = filter.trim().to_lowercase();
        if *self.ivars().filter.borrow() == trimmed {
            return;
        }
        *self.ivars().filter.borrow_mut() = trimmed;
        self.setNeedsDisplay(true);
    }

    fn send(&self, action: Action) {
        (self.ivars().actions)(action);
    }

    fn handle_key(&self, event: &NSEvent) {
        let Some(characters) = event.charactersIgnoringModifiers() else {
            return;
        };
        let text = characters.to_string();
        let Some(key) = text.chars().next() else {
            return;
        };

        match key {
            '1'..='9' => self.send(Action::ToggleSlot(key as u8 - b'0')),
            // Escape.
            '\u{1b}' => self.send(Action::Close),
            '/' => self.send(Action::FocusField),
            'a' | 'A' => self.send(Action::ApproveFirstKnock),
            'x' | 'X' => self.send(Action::RejectFirstKnock),
            'c' | 'C' => self.send(Action::CopyKey),
            '0' => self.send(Action::EndSession),
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    fn draw(&self) {
        let state = self.ivars().state.borrow();
        let bounds = self.bounds();

        // The view covers the whole window. The content box is inset from the
        // right and the bottom, and the stipple shadow falls into the gap.
        let content = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(
                bounds.size.width - style::SHADOW_OFFSET,
                bounds.size.height - style::SHADOW_OFFSET,
            ),
        );

        style::hard_shadow(content);
        style::fill(content, &style::paper());
        // The outer border. The panel is borderless at the window level, so
        // this is the only thing separating it from whatever is behind it.
        style::stroke(content, &style::ink());

        let mut y = style::MARGIN;
        let width = content.size.width - style::MARGIN * 2.0;
        let x = style::MARGIN;

        y = self.draw_header(&state, x, y, width);

        // The field itself is a real NSTextField placed by the panel. Its
        // border is drawn here so it matches every other element rather than
        // wearing AppKit's soft grey bezel.
        let field_rect = NSRect::new(NSPoint::new(x, y), NSSize::new(width, style::FIELD_HEIGHT));
        style::box_filled(field_rect, &style::card());
        y += style::FIELD_HEIGHT + 16.0;

        if !state.knocks.is_empty() {
            y = self.draw_knocks(&state, x, y, width);
            y += 10.0;
        }

        // The filtered set, not every peer. The panel is sized to what the
        // filter lets through, so drawing the whole roster into it puts rows
        // past the bottom edge, where the height check below drops them.
        let shown = visible_peers(&state.peers, &self.ivars().filter.borrow());

        if shown.is_empty() {
            self.draw_empty(&state, x, y, width);
            return;
        }

        for (label, group) in groups(&shown) {
            style::section(label, NSPoint::new(x, y), width);
            y += 14.0;

            for peer in group {
                if y + style::ROW_HEIGHT > content.size.height - style::MARGIN {
                    break;
                }
                self.draw_row(peer, x, y, width);
                y += style::ROW_HEIGHT;
            }
            y += 8.0;
        }
    }

    /// The height this roster needs, so the panel can size itself to it.
    pub fn content_height(&self) -> f64 {
        content_height_for(&self.ivars().state.borrow(), &self.ivars().filter.borrow())
    }

    fn draw_header(&self, state: &UiState, x: f64, y: f64, width: f64) -> f64 {
        style::section("swivel", NSPoint::new(x, y + 6.0), width);

        let status = match (state.online, state.dnd, state.mic) {
            (false, ..) => "connecting".to_string(),
            (_, true, _) => "do not disturb".to_string(),
            (_, _, MicState::Live) => format!(
                "live  {}",
                state
                    .live_slots
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            (_, _, MicState::Muted) => "muted".to_string(),
            (_, _, MicState::Armed) => format!("{}   mic open", state.my_name),
            _ => state.my_name.clone(),
        };

        let color = if state.mic == MicState::Live {
            style::live()
        } else {
            style::muted_ink()
        };

        style::text(
            &status,
            NSRect::new(NSPoint::new(x, y + 15.0), NSSize::new(width, 16.0)),
            &style::mono(style::SIZE_LABEL),
            &color,
            NSTextAlignment::Right,
        );

        y + style::HEADER_HEIGHT
    }

    fn draw_empty(&self, state: &UiState, x: f64, y: f64, width: f64) {
        let searching = !self.ivars().filter.borrow().is_empty() && !state.peers.is_empty();

        if searching {
            style::text(
                "Nobody matches that.",
                NSRect::new(NSPoint::new(x, y + 20.0), NSSize::new(width, 18.0)),
                &style::mono(style::SIZE_BODY),
                &style::muted_ink(),
                NSTextAlignment::Left,
            );
            return;
        }

        let mut row = y + 12.0;

        style::text(
            "No contacts yet. Send someone this key.",
            NSRect::new(NSPoint::new(x, row), NSSize::new(width, 18.0)),
            &style::mono(style::SIZE_BODY),
            &style::muted_ink(),
            NSTextAlignment::Left,
        );
        row += 26.0;

        // The key itself, not an instruction to go and run a command. Telling
        // someone to open a terminal to read a value the application already
        // holds is not an interface.
        row = self.draw_key(state, x, row, width);
        row += 14.0;

        style::text(
            "Then paste theirs above.",
            NSRect::new(NSPoint::new(x, row), NSSize::new(width, 18.0)),
            &style::mono(style::SIZE_BODY),
            &style::muted_ink(),
            NSTextAlignment::Left,
        );
    }

    /// Draws your own key in a box, with the shortcut that copies it.
    ///
    /// Returns the y below the box.
    fn draw_key(&self, state: &UiState, x: f64, y: f64, width: f64) -> f64 {
        let key = if state.my_key.is_empty() {
            "…"
        } else {
            state.my_key.as_str()
        };

        let box_rect = NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(width, style::KEY_BOX_HEIGHT),
        );
        style::box_filled(box_rect, &style::card());

        // A key is 63 characters and does not fit the panel on one line, so it
        // is split across two. It is never truncated: a key you cannot read in
        // full is a key you cannot pass on.
        let mut line_top = y + 7.0;
        let mut rest = key;
        while !rest.is_empty() {
            let take = rest
                .char_indices()
                .nth(style::KEY_CHARS_PER_LINE)
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            let (line, remainder) = rest.split_at(take);
            rest = remainder;

            style::text(
                line,
                NSRect::new(
                    NSPoint::new(x + 8.0, line_top),
                    NSSize::new(width - 16.0, 14.0),
                ),
                &style::mono(style::SIZE_LABEL),
                &style::ink(),
                NSTextAlignment::Left,
            );
            line_top += 14.0;
        }

        let (hint, colour) = if state.key_copied {
            ("copied", style::online())
        } else {
            ("press c to copy", style::muted_ink())
        };

        style::text(
            hint,
            NSRect::new(
                NSPoint::new(x + 8.0, y + style::KEY_BOX_HEIGHT - 18.0),
                NSSize::new(width - 16.0, 14.0),
            ),
            &style::mono(style::SIZE_LABEL),
            &colour,
            NSTextAlignment::Right,
        );

        y + style::KEY_BOX_HEIGHT
    }

    fn draw_knocks(&self, state: &UiState, x: f64, y: f64, width: f64) -> f64 {
        style::section("waiting", NSPoint::new(x, y), width);
        let mut row = y + 14.0;

        for knock in state.knocks.iter().take(3) {
            let rect = NSRect::new(
                NSPoint::new(x, row),
                NSSize::new(width, style::ROW_HEIGHT - 8.0),
            );
            style::box_filled(rect, &style::control());

            let name = knock
                .claimed
                .clone()
                .unwrap_or_else(|| style::PLACEHOLDER_NAME.to_string());

            style::text(
                &format!("{name}  {}", knock.endpoint_id.fmt_short()),
                NSRect::new(
                    NSPoint::new(x + 10.0, row + 6.0),
                    NSSize::new(width - 100.0, 16.0),
                ),
                &style::mono(style::SIZE_BODY),
                &style::ink(),
                NSTextAlignment::Left,
            );

            style::text(
                "a accept   x block",
                NSRect::new(
                    NSPoint::new(x + width - 150.0, row + 6.0),
                    NSSize::new(140.0, 16.0),
                ),
                &style::mono(style::SIZE_LABEL),
                &style::muted_ink(),
                NSTextAlignment::Right,
            );

            row += style::ROW_HEIGHT - 4.0;
        }

        row
    }

    fn draw_row(&self, peer: &crate::state::PeerView, x: f64, y: f64, width: f64) {
        let rect = NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(width, style::ROW_HEIGHT - 6.0),
        );

        // A live row is inverted. It is the only strong state in the interface,
        // so it must be unmistakable at a glance.
        let (background, foreground) = if peer.live {
            (style::ink(), style::paper())
        } else {
            (style::paper(), style::ink())
        };

        if peer.live {
            style::fill(rect, &background);
        }

        // The slot box, with the presence badge on its corner. The reference
        // layout puts presence on the avatar rather than out at the right edge,
        // which keeps the identity and its state together.
        if let Some(slot) = peer.slot {
            let box_rect = NSRect::new(
                NSPoint::new(
                    x + 6.0,
                    y + (style::ROW_HEIGHT - 6.0 - style::SLOT_BOX) / 2.0,
                ),
                NSSize::new(style::SLOT_BOX, style::SLOT_BOX),
            );

            if peer.live {
                style::fill(box_rect, &style::paper());
                style::stroke(box_rect, &style::paper());
            } else {
                style::box_filled(box_rect, &style::control());
            }

            style::text(
                &slot.to_string(),
                NSRect::new(
                    NSPoint::new(box_rect.origin.x, box_rect.origin.y + 6.0),
                    NSSize::new(style::SLOT_BOX, 18.0),
                ),
                &style::mono_bold(style::SIZE_SLOT),
                &style::ink(),
                NSTextAlignment::Center,
            );

            if peer.online {
                // A square badge, not a circle. Nothing in this interface is
                // round.
                let badge = NSRect::new(
                    NSPoint::new(
                        box_rect.origin.x + box_rect.size.width - 5.0,
                        box_rect.origin.y + box_rect.size.height - 5.0,
                    ),
                    NSSize::new(8.0, 8.0),
                );
                style::fill(badge, &style::paper());
                style::fill(
                    NSRect::new(
                        NSPoint::new(badge.origin.x + 1.0, badge.origin.y + 1.0),
                        NSSize::new(6.0, 6.0),
                    ),
                    &style::online(),
                );
            }
        }

        let name_x = x + 6.0 + style::SLOT_BOX + 12.0;

        style::text(
            &peer.name,
            NSRect::new(
                NSPoint::new(name_x, y + 8.0),
                NSSize::new(width - (name_x - x) - 130.0, 18.0),
            ),
            &style::mono_bold(style::SIZE_BODY),
            &foreground,
            NSTextAlignment::Left,
        );

        // Presence, round trip time, and path, right aligned.
        let right = x + width - 10.0;

        let detail = match (peer.online, peer.rtt_ms) {
            (false, _) => String::new(),
            (true, Some(ms)) => format!("{ms}ms  {}", peer.path.short()),
            (true, None) => match peer.path {
                PathKind::Unknown => "connecting".into(),
                other => other.short().to_string(),
            },
        };

        let detail_color = if peer.live {
            style::paper()
        } else {
            style::muted_ink()
        };

        // A state word replaces the measurement when it matters more. Two
        // stacked lines in a 38 px row read as clutter, and the state is always
        // the more important of the two.
        let flag = match (peer.online, peer.dnd, peer.muted, peer.speaking) {
            (true, true, ..) => Some("dnd"),
            (true, _, true, _) => Some("muted"),
            (true, .., true) => Some("speaking"),
            _ => None,
        };

        let right_text = flag.map(str::to_string).unwrap_or(detail);
        let right_color = if flag == Some("speaking") && !peer.live {
            style::live()
        } else {
            detail_color
        };

        style::text(
            &right_text,
            NSRect::new(
                NSPoint::new(right - 110.0, y + 13.0),
                NSSize::new(110.0, 16.0),
            ),
            &style::mono(style::SIZE_LABEL),
            &right_color,
            NSTextAlignment::Right,
        );

        if !peer.live {
            // A hairline under each row, rather than a box around it. Boxes on
            // every row would fight the section rules.
            style::rule(
                NSPoint::new(x, y + style::ROW_HEIGHT - 6.0),
                NSPoint::new(x + width, y + style::ROW_HEIGHT - 6.0),
                &style::control(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PathKind;

    fn peer(n: u8, slot: u8, name: &str) -> PeerView {
        PeerView {
            endpoint_id: iroh::SecretKey::from_bytes(&[n; 32]).public(),
            slot: Some(slot),
            name: name.to_string(),
            online: false,
            rtt_ms: None,
            path: PathKind::Unknown,
            dnd: false,
            muted: false,
            live: false,
            speaking: false,
        }
    }

    /// Nine offline contacts, which is one group of nine rows.
    fn nine() -> UiState {
        let names = [
            "Maggie", "Will", "Ada", "Tomas", "David", "Priya", "Jun", "Lena", "Omar",
        ];
        UiState {
            peers: names
                .iter()
                .enumerate()
                .map(|(i, name)| peer(i as u8 + 1, i as u8 + 1, name))
                .collect(),
            ..Default::default()
        }
    }

    fn rows(state: &UiState, filter: &str) -> usize {
        groups(&visible_peers(&state.peers, filter))
            .iter()
            .map(|(_, group)| group.len())
            .sum()
    }

    #[test]
    fn an_empty_filter_shows_everyone() {
        assert_eq!(rows(&nine(), ""), 9);
    }

    #[test]
    fn a_key_in_the_field_hides_nobody() {
        // The field searches and adds. A pasted key is not a search.
        let key = format!("{}abc", crate::config::TICKET_PREFIX);
        assert_eq!(rows(&nine(), &key), 9);
    }

    #[test]
    fn a_filter_matches_a_name_or_a_slot() {
        assert_eq!(rows(&nine(), "ada"), 1);
        assert_eq!(rows(&nine(), "4"), 1);
        assert_eq!(rows(&nine(), "zz"), 0);
    }

    #[test]
    fn the_height_measures_the_rows_that_are_drawn() {
        // The reported defect: the height measured the filtered set and the
        // drawing used every peer, so a panel sized for one row drew nine and
        // dropped the rest at the bottom edge.
        let state = nine();
        let one_group =
            |n: usize| BODY_TOP + 14.0 + n as f64 * style::ROW_HEIGHT + 8.0 + style::MARGIN;

        assert_eq!(content_height_for(&state, ""), one_group(rows(&state, "")));
        assert_eq!(
            content_height_for(&state, "ada"),
            one_group(rows(&state, "ada"))
        );
    }

    #[test]
    fn each_group_is_measured_once() {
        let mut state = nine();
        state.peers[0].online = true;
        state.peers[1].online = true;
        state.peers[1].live = true;

        let shown = visible_peers(&state.peers, "");
        let by_group = groups(&shown);
        assert_eq!(
            by_group.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            vec!["live", "online", "offline"]
        );
        assert_eq!(by_group.iter().map(|(_, g)| g.len()).sum::<usize>(), 9);

        let expected: f64 = BODY_TOP
            + by_group
                .iter()
                .map(|(_, g)| 14.0 + g.len() as f64 * style::ROW_HEIGHT + 8.0)
                .sum::<f64>()
            + style::MARGIN;
        assert_eq!(content_height_for(&state, ""), expected);
    }

    #[test]
    fn a_search_that_matches_nobody_keeps_the_panel_small() {
        let state = nine();
        assert!(content_height_for(&state, "zz") < content_height_for(&state, ""));
        // An empty roster shows the key box instead, which is taller than the
        // "nobody matches that" line.
        let empty = UiState::default();
        assert!(content_height_for(&empty, "") > content_height_for(&state, "zz"));
    }
}
