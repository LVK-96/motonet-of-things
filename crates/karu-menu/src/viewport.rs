use core::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollableViewport {
    width: u8,
    height: u8,
    header_height: u8,
    item_height: u8,
    max_visible: usize,
    scroll_offset: usize,
    selected_index: usize,
    total_items: usize,
}

impl ScrollableViewport {
    #[must_use]
    pub fn new(width: u8, height: u8) -> Self {
        Self::with_metrics(width, height, 14, 10)
    }

    #[must_use]
    pub fn with_metrics(width: u8, height: u8, header_height: u8, item_height: u8) -> Self {
        let item_height = item_height.max(1);
        let available = height.saturating_sub(header_height.saturating_add(2));
        let max_visible = (usize::from(available) / usize::from(item_height)).max(1);
        Self {
            width,
            height,
            header_height,
            item_height,
            max_visible,
            scroll_offset: 0,
            selected_index: 0,
            total_items: 0,
        }
    }

    pub fn set_total_items(&mut self, total_items: usize) {
        self.total_items = total_items;
        if self.total_items == 0 {
            self.selected_index = 0;
            self.scroll_offset = 0;
        } else {
            self.selected_index = self.selected_index.min(self.total_items.saturating_sub(1));
            self.ensure_visible();
        }
    }

    #[must_use]
    pub const fn width(&self) -> u8 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u8 {
        self.height
    }

    #[must_use]
    pub const fn header_height(&self) -> u8 {
        self.header_height
    }

    #[must_use]
    pub const fn item_height(&self) -> u8 {
        self.item_height
    }

    #[must_use]
    pub const fn max_visible(&self) -> usize {
        self.max_visible
    }

    #[must_use]
    pub const fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected_index
    }

    #[must_use]
    pub const fn total_items(&self) -> usize {
        self.total_items
    }

    #[must_use]
    pub fn select(&mut self, index: usize) -> bool {
        let previous = self.selected_index;
        if self.total_items == 0 {
            self.selected_index = 0;
            self.scroll_offset = 0;
            false
        } else {
            self.selected_index = index.min(self.total_items.saturating_sub(1));
            self.ensure_visible();
            self.selected_index != previous
        }
    }

    #[must_use]
    pub fn select_next(&mut self) -> bool {
        self.select(self.selected_index.saturating_add(1))
    }

    #[must_use]
    pub fn select_prev(&mut self) -> bool {
        self.select(self.selected_index.saturating_sub(1))
    }

    #[must_use]
    pub fn visible_range(&self) -> Range<usize> {
        if self.total_items == 0 {
            0..0
        } else {
            let start = self.scroll_offset.min(self.total_items.saturating_sub(1));
            let end = (start + self.max_visible).min(self.total_items);
            start..end
        }
    }

    #[must_use]
    pub fn item_y(&self, index: usize) -> i32 {
        let relative = index.saturating_sub(self.scroll_offset);
        let relative_i32 = i32::try_from(relative).map_or(i32::MAX, core::convert::identity);
        i32::from(self.header_height) + 2 + (relative_i32 * i32::from(self.item_height))
    }

    #[must_use]
    pub const fn has_more_below(&self) -> bool {
        self.scroll_offset + self.max_visible < self.total_items
    }

    #[must_use]
    pub const fn has_more_above(&self) -> bool {
        self.scroll_offset > 0
    }

    fn ensure_visible(&mut self) {
        if self.total_items == 0 {
            self.scroll_offset = 0;
            self.selected_index = 0;
        } else {
            if self.selected_index < self.scroll_offset {
                self.scroll_offset = self.selected_index;
            } else if self.selected_index >= self.scroll_offset + self.max_visible {
                self.scroll_offset = self.selected_index.saturating_sub(self.max_visible - 1);
            }
            let max_offset = self.total_items.saturating_sub(self.max_visible);
            self.scroll_offset = self.scroll_offset.min(max_offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScrollableViewport;

    #[test]
    fn viewport_scrolls_correctly() {
        let mut viewport = ScrollableViewport::new(128, 64);
        viewport.set_total_items(6);

        let _ = viewport.select(0);
        assert_eq!(viewport.visible_range(), 0..4);

        let _ = viewport.select(4);
        assert_eq!(viewport.visible_range(), 1..5);

        let _ = viewport.select(5);
        assert_eq!(viewport.visible_range(), 2..6);
    }

    #[test]
    fn viewport_reports_overflow_indicators() {
        let mut viewport = ScrollableViewport::new(128, 64);
        viewport.set_total_items(6);
        assert!(!viewport.has_more_above());
        assert!(viewport.has_more_below());

        let _ = viewport.select(5);
        assert!(viewport.has_more_above());
        assert!(!viewport.has_more_below());
    }
}
