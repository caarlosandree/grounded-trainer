//! Agrupamento de diferenças entre dois blocos de bytes.

use std::ops::Range;

/// Agrupa em trechos os índices onde `a != b`.
///
/// Duas diferenças nos índices `i < j` ficam no mesmo trecho se `j - i < gap`.
/// Cada trecho vai do primeiro ao último índice diferente, com fim exclusivo.
///
/// # Panics
///
/// Se `a.len() != b.len()` (pré-condição: os dois blocos têm o mesmo tamanho).
pub fn group(a: &[u8], b: &[u8], gap: usize) -> Vec<Range<usize>> {
    assert_eq!(a.len(), b.len(), "os blocos devem ter o mesmo tamanho");
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, _) in a.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y) {
        match out.last_mut() {
            Some(r) if i - (r.end - 1) < gap => r.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_with_gap() {
        let a = [0u8; 64];
        let mut b = a;
        b[2] = 1;
        b[10] = 1;
        b[40] = 1;
        assert_eq!(group(&a, &b, 16), vec![2..11, 40..41]);
        assert!(group(&a, &a, 16).is_empty());
    }
}
