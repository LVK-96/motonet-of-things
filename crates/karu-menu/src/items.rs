use core::ops::RangeInclusive;

use heapless::String;

use crate::events::ActionId;

/// Any item that can appear in a menu must implement this trait.
pub trait MenuItem {
    type Action: Copy + Eq;

    /// Display label (e.g., "Threshold", "Magn Target").
    fn label(&self) -> &str;

    /// Current value as string for display (e.g., "16 dB").
    fn value_string(&self) -> String<32>;

    /// Whether this item can be edited.
    fn is_editable(&self) -> bool {
        true
    }

    /// Increment value, return true if changed.
    fn increment(&mut self) -> bool;

    /// Decrement value, return true if changed.
    fn decrement(&mut self) -> bool;

    /// Optional lifecycle callback when entering edit mode.
    fn on_enter_edit(&mut self) {}

    /// Optional lifecycle callback when exiting edit mode.
    fn on_exit_edit(&mut self) {}

    /// Whether this item currently differs from its committed value.
    fn is_modified(&self) -> bool {
        false
    }

    /// Commit current pending value.
    fn commit(&mut self) {}

    /// Discard pending value and restore last committed value.
    fn discard(&mut self) {}

    /// Optional action identifier for non-editable action items.
    fn action_id(&self) -> Option<Self::Action> {
        None
    }

    /// Estimate pixel width needed for layout.
    fn estimated_width(&self) -> u16 {
        let label_chars =
            u16::try_from(self.label().len()).map_or(u16::MAX / 6, core::convert::identity);
        let value_chars =
            u16::try_from(self.value_string().len()).map_or(u16::MAX / 6, core::convert::identity);
        let label_width = label_chars.saturating_mul(6);
        let value_width = value_chars.saturating_mul(6);
        label_width + 16 + value_width
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionItem<A = ActionId>
where
    A: Copy + Eq,
{
    label: &'static str,
    action_id: A,
}

impl<A> ActionItem<A>
where
    A: Copy + Eq,
{
    #[must_use]
    pub const fn new(label: &'static str, action_id: A) -> Self {
        Self { label, action_id }
    }
}

impl<A> MenuItem for ActionItem<A>
where
    A: Copy + Eq,
{
    type Action = A;

    fn label(&self) -> &str {
        self.label
    }

    fn value_string(&self) -> String<32> {
        String::new()
    }

    fn is_editable(&self) -> bool {
        false
    }

    fn increment(&mut self) -> bool {
        false
    }

    fn decrement(&mut self) -> bool {
        false
    }

    fn action_id(&self) -> Option<Self::Action> {
        Some(self.action_id)
    }
}

pub trait NumericBounds: Copy + Ord + Eq {
    #[must_use]
    fn add_clamped(self, step: Self, max: Self) -> Self;
    #[must_use]
    fn sub_clamped(self, step: Self, min: Self) -> Self;
}

macro_rules! impl_numeric_bounds_for_int {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl NumericBounds for $ty {
                fn add_clamped(self, step: Self, max: Self) -> Self {
                    self.checked_add(step).map_or(max, |candidate| candidate.min(max))
                }

                fn sub_clamped(self, step: Self, min: Self) -> Self {
                    self.checked_sub(step).map_or(min, |candidate| candidate.max(min))
                }
            }
        )+
    };
}

impl_numeric_bounds_for_int!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

#[derive(Debug, Clone, Copy)]
pub struct NumericItem<T>
where
    T: NumericBounds,
{
    label: &'static str,
    current: T,
    pending: T,
    min: T,
    max: T,
    step: T,
    formatter: fn(T) -> String<32>,
}

impl<T> NumericItem<T>
where
    T: NumericBounds,
{
    #[must_use]
    pub fn new(
        label: &'static str,
        initial: T,
        range: RangeInclusive<T>,
        step: T,
        formatter: fn(T) -> String<32>,
    ) -> Self {
        let (range_min, range_max) = range.into_inner();
        let clamped_initial = initial.clamp(range_min, range_max);
        Self {
            label,
            current: clamped_initial,
            pending: clamped_initial,
            min: range_min,
            max: range_max,
            step,
            formatter,
        }
    }

    #[must_use]
    pub const fn current(&self) -> T {
        self.current
    }

    #[must_use]
    pub const fn pending(&self) -> T {
        self.pending
    }

    pub fn set_pending(&mut self, value: T) {
        self.pending = value.clamp(self.min, self.max);
    }
}

impl<T> MenuItem for NumericItem<T>
where
    T: NumericBounds,
{
    type Action = ActionId;

    fn label(&self) -> &str {
        self.label
    }

    fn value_string(&self) -> String<32> {
        (self.formatter)(self.pending)
    }

    fn increment(&mut self) -> bool {
        let next = self.pending.add_clamped(self.step, self.max);
        let changed = next != self.pending;
        self.pending = next;
        changed
    }

    fn decrement(&mut self) -> bool {
        let next = self.pending.sub_clamped(self.step, self.min);
        let changed = next != self.pending;
        self.pending = next;
        changed
    }

    fn is_modified(&self) -> bool {
        self.pending != self.current
    }

    fn commit(&mut self) {
        self.current = self.pending;
    }

    fn discard(&mut self) {
        self.pending = self.current;
    }
}

pub enum MenuEntry<T, A = ActionId>
where
    T: NumericBounds,
    A: Copy + Eq,
{
    Numeric(NumericItem<T>),
    Command(ActionItem<A>),
}

impl<T, A> MenuEntry<T, A>
where
    T: NumericBounds,
    A: Copy + Eq,
{
    #[must_use]
    pub const fn numeric(item: NumericItem<T>) -> Self {
        Self::Numeric(item)
    }

    #[must_use]
    pub const fn action(item: ActionItem<A>) -> Self {
        Self::Command(item)
    }
}

impl<T, A> MenuItem for MenuEntry<T, A>
where
    T: NumericBounds,
    A: Copy + Eq,
{
    type Action = A;

    fn label(&self) -> &str {
        match self {
            Self::Numeric(item) => item.label(),
            Self::Command(item) => item.label(),
        }
    }

    fn value_string(&self) -> String<32> {
        match self {
            Self::Numeric(item) => item.value_string(),
            Self::Command(item) => item.value_string(),
        }
    }

    fn is_editable(&self) -> bool {
        match self {
            Self::Numeric(item) => item.is_editable(),
            Self::Command(item) => item.is_editable(),
        }
    }

    fn increment(&mut self) -> bool {
        match self {
            Self::Numeric(item) => item.increment(),
            Self::Command(item) => item.increment(),
        }
    }

    fn decrement(&mut self) -> bool {
        match self {
            Self::Numeric(item) => item.decrement(),
            Self::Command(item) => item.decrement(),
        }
    }

    fn on_enter_edit(&mut self) {
        match self {
            Self::Numeric(item) => item.on_enter_edit(),
            Self::Command(item) => item.on_enter_edit(),
        }
    }

    fn on_exit_edit(&mut self) {
        match self {
            Self::Numeric(item) => item.on_exit_edit(),
            Self::Command(item) => item.on_exit_edit(),
        }
    }

    fn is_modified(&self) -> bool {
        match self {
            Self::Numeric(item) => item.is_modified(),
            Self::Command(item) => item.is_modified(),
        }
    }

    fn commit(&mut self) {
        match self {
            Self::Numeric(item) => item.commit(),
            Self::Command(item) => item.commit(),
        }
    }

    fn discard(&mut self) {
        match self {
            Self::Numeric(item) => item.discard(),
            Self::Command(item) => item.discard(),
        }
    }

    fn action_id(&self) -> Option<Self::Action> {
        match self {
            Self::Numeric(_) => None,
            Self::Command(item) => item.action_id(),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Write;

    use super::{MenuItem, NumericItem};
    use heapless::String;

    fn fmt_u8(value: u8) -> String<32> {
        let mut out = String::new();
        let _ = write!(out, "{value}");
        out
    }

    #[test]
    fn numeric_item_clamps_at_bounds() {
        let mut item = NumericItem::new("Test", 5, 0..=10, 1, fmt_u8);

        for _ in 0..10 {
            let _ = item.increment();
        }
        assert_eq!(item.pending(), 10);
        assert_eq!(item.value_string().as_str(), "10");

        assert!(!item.increment());
        assert_eq!(item.pending(), 10);

        for _ in 0..20 {
            let _ = item.decrement();
        }
        assert_eq!(item.pending(), 0);
        assert_eq!(item.value_string().as_str(), "0");

        assert!(!item.decrement());
        assert_eq!(item.pending(), 0);
    }

    #[test]
    fn numeric_item_tracks_pending_and_commit() {
        let mut item = NumericItem::new("Threshold", 4, 0..=10, 2, fmt_u8);
        assert_eq!(item.current(), 4);
        assert_eq!(item.pending(), 4);
        assert!(!item.is_modified());

        assert!(item.increment());
        assert_eq!(item.pending(), 6);
        assert!(item.is_modified());

        item.commit();
        assert_eq!(item.current(), 6);
        assert!(!item.is_modified());

        assert!(item.decrement());
        assert!(item.is_modified());
        item.discard();
        assert_eq!(item.pending(), 6);
        assert!(!item.is_modified());
    }
}
