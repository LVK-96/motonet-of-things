#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod events;
pub mod items;
pub mod menu;
pub mod renderer;
pub mod viewport;

pub use events::{ActionId, MenuEvent, UiEvent};
pub use items::{ActionItem, MenuEntry, MenuItem, NumericBounds, NumericItem};
pub use menu::Menu;
pub use renderer::{MenuRenderer, RenderItem, ScrollIndicator};
pub use viewport::ScrollableViewport;
