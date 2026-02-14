use heapless::{String, Vec};

use crate::items::MenuItem;
use crate::menu::Menu;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollIndicator {
    UpArrow,
    DownArrow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderItem<'a> {
    pub y: i32,
    pub label: &'a str,
    pub value: String<32>,
    pub is_selected: bool,
    pub is_editing: bool,
}

pub struct MenuRenderer<'a, I, const MAX_ITEMS: usize>
where
    I: MenuItem,
{
    menu: &'a Menu<I, MAX_ITEMS>,
}

impl<'a, I, const MAX_ITEMS: usize> MenuRenderer<'a, I, MAX_ITEMS>
where
    I: MenuItem,
{
    #[must_use]
    pub const fn new(menu: &'a Menu<I, MAX_ITEMS>) -> Self {
        Self { menu }
    }

    #[must_use]
    pub fn render_items(&self) -> Vec<RenderItem<'a>, MAX_ITEMS> {
        let mut items = Vec::new();
        let range = self.menu.viewport().visible_range();
        for index in range {
            if let Some(item) = self.menu.items().get(index) {
                let rendered = RenderItem {
                    y: self.menu.viewport().item_y(index),
                    label: item.label(),
                    value: item.value_string(),
                    is_selected: index == self.menu.selected_index(),
                    is_editing: index == self.menu.selected_index() && self.menu.is_editing(),
                };
                if items.push(rendered).is_err() {
                    break;
                }
            }
        }
        items
    }

    #[must_use]
    pub fn scroll_indicator(&self) -> Option<ScrollIndicator> {
        if self.menu.viewport().has_more_below() {
            Some(ScrollIndicator::DownArrow)
        } else if self.menu.viewport().has_more_above() {
            Some(ScrollIndicator::UpArrow)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Write;

    use heapless::String;

    use crate::events::UiEvent;
    use crate::items::{MenuEntry, NumericItem};
    use crate::menu::Menu;
    use crate::renderer::{MenuRenderer, ScrollIndicator};
    use crate::viewport::ScrollableViewport;

    fn fmt_u8(value: u8) -> String<32> {
        let mut out = String::new();
        let _ = write!(out, "{value}");
        out
    }

    #[test]
    fn renderer_limits_output_to_visible_range() {
        let viewport = ScrollableViewport::new(128, 64);
        let mut menu: Menu<MenuEntry<u8>, 8> = Menu::new(viewport);

        for value in 0..6 {
            assert!(
                menu.add(MenuEntry::numeric(NumericItem::new(
                    "Item",
                    value,
                    0..=10,
                    1,
                    fmt_u8
                )))
                .is_ok()
            );
        }

        {
            let renderer = MenuRenderer::new(&menu);
            let initial = renderer.render_items();
            assert_eq!(initial.len(), 4);
            assert_eq!(
                renderer.scroll_indicator(),
                Some(ScrollIndicator::DownArrow)
            );
        }

        let _ = menu.handle_event(UiEvent::NextScreen);
        let _ = menu.handle_event(UiEvent::NextScreen);
        let _ = menu.handle_event(UiEvent::NextScreen);
        let _ = menu.handle_event(UiEvent::NextScreen);
        let _ = menu.handle_event(UiEvent::NextScreen);
        assert_eq!(menu.selected_index(), 5);

        {
            let renderer = MenuRenderer::new(&menu);
            let final_items = renderer.render_items();
            assert_eq!(final_items.len(), 4);
            assert_eq!(renderer.scroll_indicator(), Some(ScrollIndicator::UpArrow));
        }
    }
}
