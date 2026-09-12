//! Fixed-capacity, allocation-free string and vector containers.
//!
//! The engine's hot path must never allocate (DESIGN.md §5.7): preedit
//! text, commit text, and segment lists all live in buffers whose capacity
//! is a compile-time constant. `FixedStr<N>` and `FixedVec<T, N>` are the
//! two containers that make that possible, with an explicit, atomic
//! [`Overflow`] error instead of panicking or reallocating.

use core::fmt;

/// Error returned when a fixed-capacity buffer cannot hold a write.
///
/// Every method that can overflow documents an atomic guarantee: on
/// `Err(Overflow)` the buffer is left exactly as it was before the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow;

impl fmt::Display for Overflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("fixed-capacity buffer overflow")
    }
}

impl std::error::Error for Overflow {}

/// A fixed-capacity, stack-allocated UTF-8 string with capacity `N` bytes.
///
/// `FixedStr` never allocates and never reallocates: `push_str`/`push`
/// either fit within `N` bytes or fail atomically with [`Overflow`],
/// leaving the buffer unchanged. This is the container used for preedit
/// and commit text on the engine's zero-allocation hot path.
#[derive(Clone, PartialEq, Eq)]
pub struct FixedStr<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> FixedStr<N> {
    /// Creates an empty `FixedStr`.
    pub const fn new() -> Self {
        FixedStr {
            buf: [0; N],
            len: 0,
        }
    }

    /// Empties the buffer without changing its capacity.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Returns the number of bytes currently stored.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the buffer holds no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the maximum number of bytes this buffer can hold.
    pub fn capacity(&self) -> usize {
        N
    }

    /// Returns the contents as a `&str`.
    ///
    /// The buffer only ever grows by appending whole `&str`/`char` values
    /// (see [`FixedStr::push_str`] and [`FixedStr::push`]), so `buf[..len]`
    /// is always valid UTF-8; `from_utf8` failing here would indicate a
    /// bug elsewhere in this module, not attacker-controlled input, so we
    /// fall back to `""` rather than reach for `unsafe` or `unwrap`.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// Returns the contents as raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Appends `s` if it fits, otherwise leaves the buffer unchanged.
    ///
    /// This is atomic: either all of `s` is appended, or none of it is.
    pub fn push_str(&mut self, s: &str) -> Result<(), Overflow> {
        let bytes = s.as_bytes();
        let new_len = self.len.checked_add(bytes.len()).ok_or(Overflow)?;
        if new_len > N {
            return Err(Overflow);
        }
        self.buf[self.len..new_len].copy_from_slice(bytes);
        self.len = new_len;
        Ok(())
    }

    /// Appends a single character if it fits, otherwise leaves the buffer
    /// unchanged.
    pub fn push(&mut self, c: char) -> Result<(), Overflow> {
        let mut tmp = [0u8; 4];
        self.push_str(c.encode_utf8(&mut tmp))
    }

    /// Inserts `value` at the UTF-8 byte boundary `at`.
    ///
    /// The operation is atomic: an invalid boundary or insufficient capacity
    /// returns [`Overflow`] without changing the string.
    pub fn insert_str(&mut self, at: usize, value: &str) -> Result<(), Overflow> {
        if at > self.len || !self.as_str().is_char_boundary(at) {
            return Err(Overflow);
        }
        let new_len = self.len.checked_add(value.len()).ok_or(Overflow)?;
        if new_len > N {
            return Err(Overflow);
        }
        self.buf.copy_within(at..self.len, at + value.len());
        self.buf[at..at + value.len()].copy_from_slice(value.as_bytes());
        self.len = new_len;
        Ok(())
    }

    /// Removes the character beginning at UTF-8 byte boundary `at`.
    ///
    /// Returns the removed character, or `None` when `at` is not a character
    /// boundary inside the current string. No allocation or temporary string
    /// is needed; the tail is shifted in place.
    pub fn remove_char_at(&mut self, at: usize) -> Option<char> {
        if at >= self.len || !self.as_str().is_char_boundary(at) {
            return None;
        }
        let character = self.as_str()[at..].chars().next()?;
        let end = at + character.len_utf8();
        self.buf.copy_within(end..self.len, at);
        self.len -= character.len_utf8();
        Some(character)
    }

    /// Returns the UTF-8 byte boundary at character offset `index`.
    pub fn byte_index(&self, index: usize) -> Option<usize> {
        if index == self.as_str().chars().count() {
            return Some(self.len);
        }
        self.as_str().char_indices().nth(index).map(|(at, _)| at)
    }

    /// Drops the last `n` *characters* (not bytes) from the buffer.
    ///
    /// Char-boundary safe: used for backspace over multi-byte kana. If `n`
    /// is larger than the number of characters stored, the buffer becomes
    /// empty.
    pub fn truncate_chars(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let s = self.as_str();
        let char_count = s.chars().count();
        if n >= char_count {
            self.len = 0;
            return;
        }
        let keep = char_count - n;
        let new_len = match s.char_indices().nth(keep) {
            Some((idx, _)) => idx,
            None => self.len,
        };
        self.len = new_len;
    }

    /// Removes and returns the last character, or `None` if empty.
    pub fn pop_char(&mut self) -> Option<char> {
        let s = self.as_str();
        let c = s.chars().next_back()?;
        self.len -= c.len_utf8();
        Some(c)
    }
}

impl<const N: usize> Default for FixedStr<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Debug for FixedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FixedStr").field(&self.as_str()).finish()
    }
}

/// A fixed-capacity, stack-allocated vector with capacity `N` elements.
///
/// Requires `T: Copy + Default` so that unused slots need no drop handling
/// and `new()` can zero-initialize without `unsafe`.
#[derive(Clone, PartialEq, Eq)]
pub struct FixedVec<T: Copy + Default, const N: usize> {
    buf: [T; N],
    len: usize,
}

impl<T: Copy + Default, const N: usize> FixedVec<T, N> {
    /// Creates an empty `FixedVec`.
    pub fn new() -> Self {
        FixedVec {
            buf: [T::default(); N],
            len: 0,
        }
    }

    /// Empties the vector without changing its capacity.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Returns the number of elements currently stored.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the vector holds no elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the maximum number of elements this vector can hold.
    pub fn capacity(&self) -> usize {
        N
    }

    /// Appends `value` if there is room, otherwise leaves the vector
    /// unchanged and returns `Err(Overflow)`.
    pub fn push(&mut self, value: T) -> Result<(), Overflow> {
        if self.len >= N {
            return Err(Overflow);
        }
        self.buf[self.len] = value;
        self.len += 1;
        Ok(())
    }

    /// Returns the stored elements as a slice.
    pub fn as_slice(&self) -> &[T] {
        &self.buf[..self.len]
    }

    /// Returns the element at `index`, or `None` if out of bounds.
    pub fn get(&self, index: usize) -> Option<&T> {
        self.as_slice().get(index)
    }

    /// Returns the element at `index` mutably, or `None` if out of bounds.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.buf[..self.len].get_mut(index)
    }

    /// Returns the last element, or `None` if empty.
    pub fn last(&self) -> Option<&T> {
        self.as_slice().last()
    }

    /// Removes and returns the last element, or `None` if empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.buf[self.len])
    }
}

impl<T: Copy + Default, const N: usize> Default for FixedVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default + fmt::Debug, const N: usize> fmt::Debug for FixedVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice().iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_str_push_and_read() {
        let mut s: FixedStr<8> = FixedStr::new();
        assert!(s.is_empty());
        assert_eq!(s.push_str("ab"), Ok(()));
        assert_eq!(s.as_str(), "ab");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn fixed_str_push_str_overflow_is_atomic() {
        let mut s: FixedStr<4> = FixedStr::new();
        assert_eq!(s.push_str("ab"), Ok(()));
        assert_eq!(s.push_str("xyz"), Err(Overflow));
        // Unchanged after failed push.
        assert_eq!(s.as_str(), "ab");
    }

    #[test]
    fn fixed_str_push_char_overflow_is_atomic() {
        let mut s: FixedStr<2> = FixedStr::new();
        assert_eq!(s.push('a'), Ok(()));
        // 'あ' is 3 bytes in UTF-8, only 1 byte free.
        assert_eq!(s.push('あ'), Err(Overflow));
        assert_eq!(s.as_str(), "a");
    }

    #[test]
    fn fixed_str_truncate_chars_char_boundary_safe() {
        let mut s: FixedStr<16> = FixedStr::new();
        s.push_str("あいうえお").unwrap();
        s.truncate_chars(2);
        assert_eq!(s.as_str(), "あいう");
        s.truncate_chars(100);
        assert_eq!(s.as_str(), "");
    }

    #[test]
    fn fixed_str_pop_char() {
        let mut s: FixedStr<16> = FixedStr::new();
        s.push_str("ab🍣").unwrap();
        assert_eq!(s.pop_char(), Some('🍣'));
        assert_eq!(s.pop_char(), Some('b'));
        assert_eq!(s.pop_char(), Some('a'));
        assert_eq!(s.pop_char(), None);
    }

    #[test]
    fn fixed_str_clear() {
        let mut s: FixedStr<8> = FixedStr::new();
        s.push_str("ab").unwrap();
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.as_str(), "");
    }

    #[test]
    fn fixed_str_inserts_and_removes_at_unicode_boundaries() {
        let mut value: FixedStr<16> = FixedStr::new();
        value.push_str("あう").unwrap();
        value.insert_str(3, "い").unwrap();
        assert_eq!(value.as_str(), "あいう");
        assert_eq!(value.byte_index(2), Some(6));
        assert_eq!(value.remove_char_at(3), Some('い'));
        assert_eq!(value.as_str(), "あう");
    }

    #[test]
    fn fixed_str_insert_failure_is_atomic() {
        let mut value: FixedStr<4> = FixedStr::new();
        value.push_str("abc").unwrap();
        assert_eq!(value.insert_str(1, "xy"), Err(Overflow));
        assert_eq!(value.insert_str(2, "あ"), Err(Overflow));
        assert_eq!(value.as_str(), "abc");
    }

    #[test]
    fn fixed_vec_push_and_overflow() {
        let mut v: FixedVec<u32, 2> = FixedVec::new();
        assert_eq!(v.push(1), Ok(()));
        assert_eq!(v.push(2), Ok(()));
        assert_eq!(v.push(3), Err(Overflow));
        assert_eq!(v.as_slice(), &[1, 2]);
    }

    #[test]
    fn fixed_vec_get_last_pop() {
        let mut v: FixedVec<u32, 4> = FixedVec::new();
        v.push(10).unwrap();
        v.push(20).unwrap();
        assert_eq!(v.get(0), Some(&10));
        assert_eq!(v.get(5), None);
        assert_eq!(v.last(), Some(&20));
        assert_eq!(v.pop(), Some(20));
        assert_eq!(v.len(), 1);
        assert_eq!(v.pop(), Some(10));
        assert_eq!(v.pop(), None);
    }

    #[test]
    fn overflow_display() {
        assert_eq!(Overflow.to_string(), "fixed-capacity buffer overflow");
    }
}
