/// The input scope of the focused text field (DESIGN.md §9): password and
/// similar sensitive scopes disable learning and the commit-cache history
/// layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InputScope {
    Normal = 0,
    Password = 1,
    Url = 2,
    Email = 3,
    Digits = 4,
    /// The TSF host could not positively classify the focused range. This is
    /// deliberately distinct from `Normal`: callers must not turn a failed
    /// scope read into permission to persist user input.
    Unclassified = 5,
}

impl InputScope {
    /// All `InputScope` variants, in declaration order.
    pub const ALL: [InputScope; 6] = [
        InputScope::Normal,
        InputScope::Password,
        InputScope::Url,
        InputScope::Email,
        InputScope::Digits,
        InputScope::Unclassified,
    ];
}
