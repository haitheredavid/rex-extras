//! Size classes: the frame's shape decides how much each lane row carries. A sidebar is narrow,
//! a band is a short wide strip (the pinned split), anything else is a panel.

use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    /// Short and wide: groups move into a column beside the gutter.
    Band,
    /// Narrow: lanes may take two rows, the gutter first and the strip under it.
    Sidebar,
    Panel,
}

pub fn size_class(area: Rect) -> SizeClass {
    if area.width < 80 {
        SizeClass::Sidebar
    } else if area.height <= 20 && area.width >= 100 {
        SizeClass::Band
    } else {
        SizeClass::Panel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_pick_their_class() {
        assert_eq!(size_class(Rect::new(0, 0, 50, 45)), SizeClass::Sidebar);
        assert_eq!(size_class(Rect::new(0, 0, 160, 14)), SizeClass::Band);
        assert_eq!(size_class(Rect::new(0, 0, 90, 14)), SizeClass::Panel);
        assert_eq!(size_class(Rect::new(0, 0, 160, 40)), SizeClass::Panel);
    }
}
