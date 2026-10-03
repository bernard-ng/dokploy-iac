/// Where a node sits in the source text.
///
/// A span is a position, not content: two spans always compare equal, so documents that
/// differ only in layout compare equal.
#[derive(Clone, Copy, Debug, Default)]
pub struct Span {
    /// Byte offset from the start of the source.
    pub offset: usize,
    /// Length in bytes.
    pub len: usize,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
}

impl PartialEq for Span {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for Span {}
