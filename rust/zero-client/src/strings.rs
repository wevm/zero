//! The two string orders Zero uses.
//!
//! - UTF-8 byte order (code point order) is what `compareUTF8` and SQLite
//!   use. Row values, primary keys, and AST normalization sort this way. It
//!   is Rust's own `str` order.
//! - UTF-16 code unit order is what JS `<` and `Array#sort` use. Object keys
//!   are sorted this way before hashing. It differs from code point order
//!   only between U+E000..=U+FFFF and the supplementary planes.

use std::cmp::Ordering;

/// `compareUTF8(a, b)`.
pub fn utf8(a: &str, b: &str) -> Ordering {
    a.as_bytes().cmp(b.as_bytes())
}

/// JS `a < b ? -1 : a > b ? 1 : 0` for strings.
pub fn utf16(a: &str, b: &str) -> Ordering {
    // The strings agree up to their first differing byte, which starts or is
    // inside the first differing character.
    let common = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let start = (0..=common)
        .rev()
        .find(|&i| a.is_char_boundary(i))
        .unwrap_or(0);
    a[start..].encode_utf16().cmp(b[start..].encode_utf16())
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering::*;

    #[test]
    fn utf16_puts_supplementary_planes_before_the_end_of_the_bmp() {
        assert_eq!(super::utf16("\u{ffff}", "\u{10000}"), Greater);
        assert_eq!(super::utf8("\u{ffff}", "\u{10000}"), Less);
        assert_eq!(super::utf16("ab\u{e000}", "ab\u{1f600}x"), Greater);
        assert_eq!(super::utf16("ab", "abc"), Less);
    }
}
