//! Client pointer events to mouse events.
//!
//! An RFB pointer event is a position and the state of all buttons; macOS
//! wants separate events: moves, drags (a move with a button held is a
//! different event type), button presses and releases with a click count
//! (double and triple clicks are not detected by the system for synthetic
//! events), and scroll steps. [`PointerMapper`] keeps the previous state and
//! turns each RFB event into the list to post, in order.
//!
//! Positions stay in picture pixels; [`crate::layout::Layout::to_global`]
//! turns them into screen points when posting.

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Move,
    /// Moved with this button held.
    Drag(Button),
    Down(Button),
    Up(Button),
    /// A wheel step: lines to scroll, positive up and left (the sign of the
    /// horizontal axis is to be confirmed on a Mac).
    Scroll {
        dx: i32,
        dy: i32,
    },
}

/// One mouse event to post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// Position in picture pixels.
    pub x: u16,
    pub y: u16,
    /// Click count for `Down` and `Up` (1 single, 2 double, 3 triple), else 0.
    pub clicks: u8,
}

/// Lines scrolled by one wheel step.
const LINES_PER_STEP: i32 = 3;
/// A second press counts as a double click within this time and distance.
const DOUBLE_CLICK_MS: u64 = 500;
const DOUBLE_CLICK_PX: i32 = 4;

#[derive(Debug, Clone, Copy)]
struct Click {
    button: Button,
    x: u16,
    y: u16,
    at_ms: u64,
    count: u8,
}

/// Holds the pointer state between events.
#[derive(Debug, Default)]
pub struct PointerMapper {
    /// Button bits 0 to 2 as of the last event.
    buttons: u8,
    /// Wheel bits 3 to 6 as of the last event (a step is a press).
    wheel: u8,
    at: Option<(u16, u16)>,
    last_click: Option<Click>,
    /// Click count of each button now held, for its release.
    held_count: [u8; 3],
}

const BUTTONS: [(u8, Button); 3] = [(1, Button::Left), (2, Button::Middle), (4, Button::Right)];

impl PointerMapper {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The client's pointer is at (`x`, `y`) with `mask` buttons down (bit 0
    /// left, 1 middle, 2 right, 3 and 4 wheel up and down, 5 and 6 wheel
    /// left and right); `now_ms` is any steady clock in milliseconds.
    pub fn update(&mut self, x: u16, y: u16, mask: u8, now_ms: u64) -> Vec<MouseEvent> {
        let mut out = Vec::new();
        let ev = |kind, clicks| MouseEvent { kind, x, y, clicks };
        if self.at != Some((x, y)) {
            let held = BUTTONS.iter().find(|(bit, _)| self.buttons & bit != 0);
            out.push(ev(
                held.map_or(MouseKind::Move, |(_, b)| MouseKind::Drag(*b)),
                0,
            ));
            self.at = Some((x, y));
        }
        for (index, (bit, button)) in BUTTONS.iter().enumerate() {
            let (was, is) = (self.buttons & bit != 0, mask & bit != 0);
            if is && !was {
                let count = self.count_click(*button, x, y, now_ms);
                self.held_count[index] = count;
                out.push(ev(MouseKind::Down(*button), count));
            } else if was && !is {
                out.push(ev(MouseKind::Up(*button), self.held_count[index].max(1)));
            }
        }
        let wheel = (mask >> 3) & 0b1111;
        let pressed = wheel & !self.wheel;
        for (bit, dx, dy) in [
            (0b0001, 0, LINES_PER_STEP),
            (0b0010, 0, -LINES_PER_STEP),
            (0b0100, LINES_PER_STEP, 0),
            (0b1000, -LINES_PER_STEP, 0),
        ] {
            if pressed & bit != 0 {
                out.push(ev(MouseKind::Scroll { dx, dy }, 0));
            }
        }
        self.buttons = mask & 0b111;
        self.wheel = wheel;
        out
    }

    fn count_click(&mut self, button: Button, x: u16, y: u16, now_ms: u64) -> u8 {
        let count = match self.last_click {
            Some(c)
                if c.button == button
                    && now_ms.saturating_sub(c.at_ms) <= DOUBLE_CLICK_MS
                    && (i32::from(c.x) - i32::from(x)).abs() <= DOUBLE_CLICK_PX
                    && (i32::from(c.y) - i32::from(y)).abs() <= DOUBLE_CLICK_PX =>
            {
                c.count.saturating_add(1)
            }
            _ => 1,
        };
        self.last_click = Some(Click {
            button,
            x,
            y,
            at_ms: now_ms,
            count,
        });
        count
    }

    /// Releases for every button still held (a client left, or sharing paused).
    pub fn release_all(&mut self) -> Vec<MouseEvent> {
        let (x, y) = self.at.unwrap_or((0, 0));
        let out = BUTTONS
            .iter()
            .enumerate()
            .filter(|(_, (bit, _))| self.buttons & bit != 0)
            .map(|(index, (_, button))| MouseEvent {
                kind: MouseKind::Up(*button),
                x,
                y,
                clicks: self.held_count[index].max(1),
            })
            .collect();
        self.buttons = 0;
        self.wheel = 0;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(events: &[MouseEvent]) -> Vec<MouseKind> {
        events.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn moves_then_press_then_drag_then_release() {
        let mut m = PointerMapper::new();
        assert_eq!(kinds(&m.update(10, 10, 0, 0)), [MouseKind::Move]);
        assert!(
            m.update(10, 10, 0, 1).is_empty(),
            "same state, nothing to post"
        );
        // Press at a new place: move first, then the press there.
        let press = m.update(20, 30, 1, 2);
        assert_eq!(
            kinds(&press),
            [MouseKind::Move, MouseKind::Down(Button::Left)]
        );
        assert!(press.iter().all(|e| (e.x, e.y) == (20, 30)));
        assert_eq!(
            kinds(&m.update(25, 30, 1, 3)),
            [MouseKind::Drag(Button::Left)]
        );
        let release = m.update(25, 30, 0, 4);
        assert_eq!(kinds(&release), [MouseKind::Up(Button::Left)]);
    }

    #[test]
    fn quick_repeated_presses_count_as_double_and_triple_clicks() {
        let mut m = PointerMapper::new();
        let mut clicks = Vec::new();
        for (i, t) in [0u64, 200, 400].iter().enumerate() {
            let down = m.update(5, 5, 1, *t);
            clicks.push(down.last().unwrap().clicks);
            let up = m.update(5, 5, 0, *t + 50);
            assert_eq!(
                up[0].clicks, clicks[i],
                "the release repeats the press count"
            );
        }
        assert_eq!(clicks, [1, 2, 3]);
    }

    #[test]
    fn slow_far_or_other_button_clicks_start_over() {
        let mut m = PointerMapper::new();
        m.update(5, 5, 1, 0);
        m.update(5, 5, 0, 10);
        assert_eq!(
            m.update(5, 5, 1, 1000).last().unwrap().clicks,
            1,
            "too slow"
        );
        m.update(5, 5, 0, 1010);
        assert_eq!(
            m.update(50, 5, 1, 1100).last().unwrap().clicks,
            1,
            "too far"
        );
        m.update(50, 5, 0, 1110);
        assert_eq!(
            m.update(50, 5, 4, 1200).last().unwrap().clicks,
            1,
            "other button"
        );
    }

    #[test]
    fn wheel_steps_fire_once_per_press() {
        let mut m = PointerMapper::new();
        m.update(0, 0, 0, 0);
        let up = m.update(0, 0, 0b1000, 1);
        assert_eq!(kinds(&up), [MouseKind::Scroll { dx: 0, dy: 3 }]);
        assert!(m.update(0, 0, 0b1000, 2).is_empty(), "held: no repeat");
        assert!(m.update(0, 0, 0, 3).is_empty(), "release does nothing");
        assert_eq!(
            kinds(&m.update(0, 0, 0b1_0000, 4)),
            [MouseKind::Scroll { dx: 0, dy: -3 }]
        );
        assert_eq!(
            kinds(&m.update(0, 0, 0b10_0000, 5)),
            [MouseKind::Scroll { dx: 3, dy: 0 }]
        );
        assert_eq!(
            kinds(&m.update(0, 0, 0b100_0000, 6)),
            [MouseKind::Scroll { dx: -3, dy: 0 }]
        );
    }

    #[test]
    fn the_right_button_drags_as_right_and_two_buttons_press_in_order() {
        let mut m = PointerMapper::new();
        m.update(0, 0, 0, 0);
        let both = m.update(0, 0, 0b101, 1);
        assert_eq!(
            kinds(&both),
            [
                MouseKind::Down(Button::Left),
                MouseKind::Down(Button::Right)
            ]
        );
        assert_eq!(
            kinds(&m.update(1, 0, 0b100, 2)).len(),
            2,
            "drag, then left up"
        );
        assert_eq!(
            kinds(&m.update(2, 0, 0b100, 3)),
            [MouseKind::Drag(Button::Right)]
        );
    }

    #[test]
    fn release_all_lets_go_of_held_buttons() {
        let mut m = PointerMapper::new();
        m.update(7, 8, 0b011, 0);
        let up = m.release_all();
        assert_eq!(
            kinds(&up),
            [MouseKind::Up(Button::Left), MouseKind::Up(Button::Middle)]
        );
        assert!(up.iter().all(|e| (e.x, e.y) == (7, 8)));
        assert_eq!(m.release_all(), []);
    }
}
