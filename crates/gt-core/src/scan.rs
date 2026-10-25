//! Padrões AOB com curingas e busca em blocos.

use crate::error::{Error, Result};

/// Padrão AOB: uma sequência de bytes fixos e curingas.
///
/// Cada posição é `Some(byte)` para um byte fixo ou `None` para um curinga (`?` ou `??`).
/// Um padrão nunca fica vazio nem só com curingas (`parse` recusa esses casos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<Option<u8>>,
}

impl Pattern {
    /// Interpreta um padrão como `48 8B ?? ? 05`. Tokens são separados por espaços.
    ///
    /// Cada token é `?`/`??` (curinga) ou exatamente dois dígitos hexadecimais. O erro
    /// `BadPattern` traz o índice do token inválido, a partir de zero.
    pub fn parse(text: &str) -> Result<Pattern> {
        let mut bytes = Vec::new();
        for (index, token) in text.split_whitespace().enumerate() {
            let entry = if matches!(token, "?" | "??") {
                None
            } else if token.len() == 2 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
                // Dígitos já validados: `from_str_radix` não aceita sinal aqui.
                Some(
                    u8::from_str_radix(token, 16).map_err(|_| Error::BadPattern {
                        token: token.to_string(),
                        index,
                    })?,
                )
            } else {
                return Err(Error::BadPattern {
                    token: token.to_string(),
                    index,
                });
            };
            bytes.push(entry);
        }
        if bytes.iter().all(Option::is_none) {
            return Err(Error::EmptyPattern);
        }
        Ok(Pattern { bytes })
    }

    /// Número de posições do padrão, contando os curingas.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// A posição `i` do padrão: `None` fora do padrão, `Some(None)` para curinga e `Some(Some(b))`
    /// para um byte fixo.
    pub fn token(&self, i: usize) -> Option<Option<u8>> {
        self.bytes.get(i).copied()
    }

    /// Sempre `false` para um padrão criado por `parse`. Existe por causa do `len`.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Verifica se o padrão casa exatamente com o início de `window`.
    /// Quem chama garante que `window.len() >= self.len()`.
    fn matches_at(&self, window: &[u8]) -> bool {
        self.bytes
            .iter()
            .zip(window)
            .all(|(expected, actual)| expected.is_none_or(|b| b == *actual))
    }
}

/// Todas as ocorrências de `pattern` em `haystack`, como offsets crescentes.
///
/// Ocorrências podem se sobrepor. Custo: linear no tamanho de `haystack` vezes o tamanho do padrão.
pub fn find_all(haystack: &[u8], pattern: &Pattern) -> Vec<usize> {
    let plen = pattern.len();
    if haystack.len() < plen {
        return Vec::new();
    }
    (0..=haystack.len() - plen)
        .filter(|&at| pattern.matches_at(&haystack[at..]))
        .collect()
}

/// Busca `pattern` em `[0, total_len)` lendo a memória em blocos de até `chunk` bytes.
///
/// Invólucro de [`scan_chunked_many`] para um único padrão; veja lá a semântica dos blocos.
pub fn scan_chunked(
    total_len: u64,
    chunk: usize,
    pattern: &Pattern,
    read: impl FnMut(u64, &mut [u8]) -> Result<()>,
) -> Result<Vec<u64>> {
    let mut all = scan_chunked_many(total_len, chunk, std::slice::from_ref(pattern), read)?;
    Ok(all.pop().unwrap_or_default())
}

/// Busca vários padrões em `[0, total_len)` numa única passada, em blocos de até `chunk` bytes.
///
/// Devolve um vetor de ocorrências por padrão, na ordem de `patterns`. Os blocos se sobrepõem em
/// `max_len - 1` bytes (`max_len` é o tamanho do maior padrão), então uma ocorrência que cruza a
/// fronteira entre dois blocos é encontrada. Cada ocorrência é reportada uma única vez, em ordem
/// crescente e sem duplicatas. `chunk` menor ou igual ao maior padrão é ajustado para
/// `max_len + 1`, para que cada bloco avance pelo menos um byte. Sem padrões, nada é lido.
///
/// `read(offset, buf)` preenche `buf` com os bytes a partir de `offset`. Erros do `read`
/// são devolvidos sem alteração.
pub fn scan_chunked_many(
    total_len: u64,
    chunk: usize,
    patterns: &[Pattern],
    mut read: impl FnMut(u64, &mut [u8]) -> Result<()>,
) -> Result<Vec<Vec<u64>>> {
    let mut hits: Vec<Vec<u64>> = vec![Vec::new(); patterns.len()];
    let Some(max_len) = patterns.iter().map(Pattern::len).max() else {
        return Ok(hits);
    };
    let chunk = chunk.max(max_len + 1);
    // Cada bloco "é dono" dos offsets em [start, start + step). A sobreposição garante que
    // qualquer ocorrência desse intervalo, de qualquer padrão, cabe inteira no bloco.
    let step = (chunk - (max_len - 1)) as u64;
    let mut buf = vec![0u8; chunk.min(usize::try_from(total_len).unwrap_or(usize::MAX))];
    let mut start = 0u64;
    while start < total_len {
        let window_len = (total_len - start).min(chunk as u64) as usize;
        let window = &mut buf[..window_len];
        read(start, window)?;
        let is_last = start + step >= total_len;
        let owned_end = if is_last { u64::MAX } else { start + step };
        for (pattern, found) in patterns.iter().zip(&mut hits) {
            for at in find_all(window, pattern) {
                let absolute = start + at as u64;
                if absolute < owned_end {
                    found.push(absolute);
                }
            }
        }
        if is_last {
            break;
        }
        start += step;
    }
    Ok(hits)
}

/// Resultado da contagem de ocorrências de um padrão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uniqueness {
    /// Nenhuma ocorrência: o padrão está indisponível.
    None,
    /// Exatamente uma ocorrência, no offset indicado.
    Unique(u64),
    /// Duas ou mais ocorrências: o padrão é ambíguo.
    Many(usize),
}

impl Uniqueness {
    /// Classifica a lista de ocorrências.
    pub fn from_hits(hits: &[u64]) -> Uniqueness {
        match hits {
            [] => Uniqueness::None,
            [only] => Uniqueness::Unique(*only),
            many => Uniqueness::Many(many.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wildcards() {
        let p = Pattern::parse("48 8B ?? ? 05").unwrap();
        assert_eq!(p.len(), 5);
    }

    #[test]
    fn token_accessor() {
        let p = Pattern::parse("48 ?? 05").unwrap();
        assert_eq!(p.token(0), Some(Some(0x48)));
        assert_eq!(p.token(1), Some(None));
        assert_eq!(p.token(2), Some(Some(0x05)));
        assert_eq!(p.token(3), None);
    }

    #[test]
    fn bad_token_reports_index() {
        assert!(matches!(
            Pattern::parse("48 GG 05"),
            Err(Error::BadPattern { index: 1, .. })
        ));
        assert!(matches!(
            Pattern::parse("48 8"),
            Err(Error::BadPattern { index: 1, .. })
        ));
    }

    #[test]
    fn rejects_empty_or_only_wildcards() {
        assert!(matches!(Pattern::parse(""), Err(Error::EmptyPattern)));
        assert!(matches!(
            Pattern::parse("?? ? ??"),
            Err(Error::EmptyPattern)
        ));
        assert!(
            Error::EmptyPattern
                .to_string()
                .contains("padrão sem bytes fixos")
        );
    }

    #[test]
    fn finds_all_with_wildcards() {
        let hay = [0x48, 0x8B, 0x01, 0x05, 0x00, 0x48, 0x8B, 0xFF, 0x05];
        assert_eq!(
            find_all(&hay, &Pattern::parse("48 8B ?? 05").unwrap()),
            vec![0, 5]
        );
    }

    #[test]
    fn chunked_finds_match_across_boundary() {
        let mut data = [0u8; 100];
        data[62..66].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let hits = scan_chunked(
            100,
            64,
            &Pattern::parse("DE AD BE EF").unwrap(),
            |off, buf| {
                buf.copy_from_slice(&data[off as usize..off as usize + buf.len()]);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(hits, vec![62]);
    }

    #[test]
    fn uniqueness() {
        assert!(matches!(Uniqueness::from_hits(&[]), Uniqueness::None));
        assert!(matches!(Uniqueness::from_hits(&[7]), Uniqueness::Unique(7)));
        assert!(matches!(
            Uniqueness::from_hits(&[1, 2]),
            Uniqueness::Many(2)
        ));
    }

    #[test]
    fn chunked_matches_whole_buffer_without_duplicates() {
        // "00 ??" casa em quase todo offset, então duplicatas apareceriam logo nas fronteiras.
        let mut data = vec![0u8; 300];
        data[10] = 0xAB;
        data[150] = 0xAB;
        data[299] = 0xAB;
        let pattern = Pattern::parse("00 ??").unwrap();
        let whole: Vec<u64> = find_all(&data, &pattern)
            .into_iter()
            .map(|at| at as u64)
            .collect();
        for chunk in [3, 4, 7, 64, 299, 300, 1000] {
            let hits = scan_chunked(data.len() as u64, chunk, &pattern, |off, buf| {
                buf.copy_from_slice(&data[off as usize..off as usize + buf.len()]);
                Ok(())
            })
            .unwrap();
            assert_eq!(hits, whole, "chunk {chunk}");
        }
    }

    #[test]
    fn many_patterns_share_one_pass() {
        // Padrões de 3 e 7 bytes, com ocorrências em volta das fronteiras de blocos de 16 bytes.
        let mut data = vec![0u8; 80];
        for at in [0usize, 13, 14, 29, 40, 70] {
            data[at..at + 3].copy_from_slice(&[0xAA, 0xBB, 0xCC]);
        }
        for at in [9usize, 25, 33, 60] {
            data[at..at + 7].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
        }
        let patterns = [
            Pattern::parse("AA BB CC").unwrap(),
            Pattern::parse("01 02 03 04 05 06 07").unwrap(),
        ];
        let many = scan_chunked_many(data.len() as u64, 16, &patterns, |off, buf| {
            buf.copy_from_slice(&data[off as usize..off as usize + buf.len()]);
            Ok(())
        })
        .unwrap();
        assert_eq!(many.len(), 2);
        for (pattern, hits) in patterns.iter().zip(&many) {
            let whole: Vec<u64> = find_all(&data, pattern)
                .into_iter()
                .map(|at| at as u64)
                .collect();
            assert!(!whole.is_empty());
            assert_eq!(hits, &whole);
        }
    }

    #[test]
    fn many_without_patterns_reads_nothing() {
        let result = scan_chunked_many(100, 16, &[], |_, _| panic!("não deveria ler"));
        assert_eq!(result.unwrap(), Vec::<Vec<u64>>::new());
    }

    #[test]
    fn chunked_propagates_read_error() {
        let pattern = Pattern::parse("00 00").unwrap();
        let result = scan_chunked(100, 16, &pattern, |off, _buf| {
            if off >= 32 {
                Err(Error::Unmapped { addr: off, len: 16 })
            } else {
                Ok(())
            }
        });
        // O primeiro bloco que começa em 32 ou depois é quem falha; o erro é repassado sem trocar.
        assert!(matches!(result, Err(Error::Unmapped { addr, len: 16 }) if addr >= 32));
    }
}
