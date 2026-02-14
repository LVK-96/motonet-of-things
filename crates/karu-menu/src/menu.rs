use heapless::Vec;

use crate::events::{MenuEvent, UiEvent};
use crate::items::MenuItem;
use crate::viewport::ScrollableViewport;

pub struct Menu<I, const MAX_ITEMS: usize>
where
    I: MenuItem,
{
    items: Vec<I, MAX_ITEMS>,
    viewport: ScrollableViewport,
    editing: bool,
}

impl<I, const MAX_ITEMS: usize> Menu<I, MAX_ITEMS>
where
    I: MenuItem,
{
    #[must_use]
    pub fn new(viewport: ScrollableViewport) -> Self {
        Self {
            items: Vec::new(),
            viewport,
            editing: false,
        }
    }

    /// Adds an item to the menu.
    ///
    /// # Errors
    ///
    /// Returns the original `item` when the fixed-capacity menu is full.
    pub fn add(&mut self, item: I) -> Result<(), I> {
        let result = self.items.push(item);
        if result.is_ok() {
            self.viewport.set_total_items(self.items.len());
        }
        result
    }

    #[must_use]
    pub fn handle_event(&mut self, event: UiEvent) -> MenuEvent<I::Action> {
        if self.items.is_empty() {
            MenuEvent::NoOp
        } else {
            match event {
                UiEvent::NextScreen => self.on_next(),
                UiEvent::PrevScreen => self.on_prev(),
                UiEvent::Select => self.on_select(),
            }
        }
    }

    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.items.iter().any(MenuItem::is_modified)
    }

    pub fn commit_all(&mut self) {
        self.items.iter_mut().for_each(MenuItem::commit);
    }

    pub fn discard_all(&mut self) {
        self.items.iter_mut().for_each(MenuItem::discard);
    }

    #[must_use]
    pub fn set_selected(&mut self, index: usize) -> bool {
        let changed = self.viewport.select(index);
        if changed {
            self.editing = false;
        }
        changed
    }

    #[must_use]
    pub const fn is_editing(&self) -> bool {
        self.editing
    }

    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.viewport.selected_index()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn items(&self) -> &[I] {
        self.items.as_slice()
    }

    #[must_use]
    pub fn items_mut(&mut self) -> &mut [I] {
        self.items.as_mut_slice()
    }

    #[must_use]
    pub const fn viewport(&self) -> &ScrollableViewport {
        &self.viewport
    }

    #[must_use]
    pub fn viewport_mut(&mut self) -> &mut ScrollableViewport {
        &mut self.viewport
    }

    fn on_next(&mut self) -> MenuEvent<I::Action> {
        let index = self.viewport.selected_index();
        if self.editing {
            self.items.get_mut(index).map_or(MenuEvent::NoOp, |item| {
                if item.increment() {
                    MenuEvent::ValueChanged(index)
                } else {
                    MenuEvent::NoOp
                }
            })
        } else if self.viewport.select_next() {
            MenuEvent::SelectionChanged(self.viewport.selected_index())
        } else {
            MenuEvent::NoOp
        }
    }

    fn on_prev(&mut self) -> MenuEvent<I::Action> {
        let index = self.viewport.selected_index();
        if self.editing {
            self.items.get_mut(index).map_or(MenuEvent::NoOp, |item| {
                if item.decrement() {
                    MenuEvent::ValueChanged(index)
                } else {
                    MenuEvent::NoOp
                }
            })
        } else if self.viewport.select_prev() {
            MenuEvent::SelectionChanged(self.viewport.selected_index())
        } else {
            MenuEvent::NoOp
        }
    }

    fn on_select(&mut self) -> MenuEvent<I::Action> {
        let selected_index = self.viewport.selected_index();
        let item_data = self
            .items
            .get(selected_index)
            .map(|item| (item.is_editable(), item.action_id()));

        item_data.map_or(MenuEvent::NoOp, |(editable, action_id)| {
            if editable {
                if self.editing {
                    self.editing = false;
                    if let Some(item) = self.items.get_mut(selected_index) {
                        item.on_exit_edit();
                    }
                    MenuEvent::EditCompleted(selected_index)
                } else {
                    self.editing = true;
                    if let Some(item) = self.items.get_mut(selected_index) {
                        item.on_enter_edit();
                    }
                    MenuEvent::EditStarted(selected_index)
                }
            } else {
                action_id.map_or(MenuEvent::NoOp, |action| MenuEvent::ActionSelected {
                    index: selected_index,
                    action,
                })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Write;

    use heapless::String;

    use crate::events::{ActionId, MenuEvent, UiEvent};
    use crate::items::{ActionItem, MenuEntry, MenuItem, NumericItem};
    use crate::menu::Menu;
    use crate::viewport::ScrollableViewport;

    fn fmt_u8(value: u8) -> String<32> {
        let mut out = String::new();
        let _ = write!(out, "{value}");
        out
    }

    fn mk_numeric(label: &'static str, value: u8) -> MenuEntry<u8> {
        MenuEntry::numeric(NumericItem::new(label, value, 0..=10, 1, fmt_u8))
    }

    #[test]
    fn menu_editing_and_action_flow() {
        let viewport = ScrollableViewport::new(128, 64);
        let mut menu: Menu<MenuEntry<u8>, 4> = Menu::new(viewport);
        assert!(menu.add(mk_numeric("A", 1)).is_ok());
        assert!(menu.add(mk_numeric("B", 2)).is_ok());
        assert!(
            menu.add(MenuEntry::action(ActionItem::new("Save", ActionId(7))))
                .is_ok()
        );

        assert_eq!(menu.selected_index(), 0);
        assert_eq!(
            menu.handle_event(UiEvent::Select),
            MenuEvent::EditStarted(0)
        );
        assert!(menu.is_editing());

        assert_eq!(
            menu.handle_event(UiEvent::NextScreen),
            MenuEvent::ValueChanged(0)
        );
        assert_eq!(menu.items()[0].value_string().as_str(), "2");
        assert!(menu.is_modified());

        assert_eq!(
            menu.handle_event(UiEvent::Select),
            MenuEvent::EditCompleted(0)
        );
        assert!(!menu.is_editing());

        assert_eq!(
            menu.handle_event(UiEvent::NextScreen),
            MenuEvent::SelectionChanged(1)
        );
        assert_eq!(
            menu.handle_event(UiEvent::NextScreen),
            MenuEvent::SelectionChanged(2)
        );
        assert_eq!(
            menu.handle_event(UiEvent::Select),
            MenuEvent::ActionSelected {
                index: 2,
                action: ActionId(7)
            }
        );
    }

    #[test]
    fn menu_navigation_clamps_at_edges() {
        let viewport = ScrollableViewport::new(128, 64);
        let mut menu: Menu<MenuEntry<u8>, 2> = Menu::new(viewport);
        assert!(menu.add(mk_numeric("A", 1)).is_ok());
        assert!(menu.add(mk_numeric("B", 2)).is_ok());

        assert_eq!(menu.handle_event(UiEvent::PrevScreen), MenuEvent::NoOp);
        assert_eq!(menu.selected_index(), 0);
        assert_eq!(
            menu.handle_event(UiEvent::NextScreen),
            MenuEvent::SelectionChanged(1)
        );
        assert_eq!(menu.handle_event(UiEvent::NextScreen), MenuEvent::NoOp);
        assert_eq!(menu.selected_index(), 1);
    }

    #[test]
    fn menu_commit_and_discard_changes() {
        let viewport = ScrollableViewport::new(128, 64);
        let mut menu: Menu<MenuEntry<u8>, 2> = Menu::new(viewport);
        assert!(menu.add(mk_numeric("A", 1)).is_ok());

        let _ = menu.handle_event(UiEvent::Select);
        let _ = menu.handle_event(UiEvent::NextScreen);
        let _ = menu.handle_event(UiEvent::Select);
        assert!(menu.is_modified());

        menu.commit_all();
        assert!(!menu.is_modified());

        let _ = menu.handle_event(UiEvent::Select);
        let _ = menu.handle_event(UiEvent::NextScreen);
        let _ = menu.handle_event(UiEvent::Select);
        assert!(menu.is_modified());

        menu.discard_all();
        assert!(!menu.is_modified());
    }
}
