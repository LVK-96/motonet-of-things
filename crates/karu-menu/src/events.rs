#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionId(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    NextScreen,
    PrevScreen,
    Select,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEvent<A = ActionId> {
    NoOp,
    SelectionChanged(usize),
    EditStarted(usize),
    EditCompleted(usize),
    ValueChanged(usize),
    ActionSelected { index: usize, action: A },
}
