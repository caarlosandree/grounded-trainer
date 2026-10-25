//! Interpretação de endereços e tamanhos digitados pelo usuário.

use crate::error::{Error, Result};

/// Endereço pedido pelo usuário, antes de virar um endereço absoluto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddrSpec {
    /// Endereço absoluto, como `0x1456FC11C`.
    Absolute(u64),
    /// Deslocamento a partir da base de um módulo. `module: None` é o atalho `+offset`,
    /// que usa o executável principal do jogo.
    ModuleOffset { module: Option<String>, offset: u64 },
}

impl AddrSpec {
    /// Calcula o endereço absoluto. `base_of` é chamado só para `ModuleOffset`,
    /// com o nome do módulo (`None` para o atalho `+offset`).
    pub fn resolve(&self, base_of: impl Fn(Option<&str>) -> Result<u64>) -> Result<u64> {
        match self {
            AddrSpec::Absolute(addr) => Ok(*addr),
            AddrSpec::ModuleOffset { module, offset } => {
                let base = base_of(module.as_deref())?;
                base.checked_add(*offset).ok_or_else(|| Error::BadAddress {
                    input: format!("{base:#x}+{offset:#x}"),
                    reason: "a soma sai do intervalo de endereços de 64 bits".into(),
                })
            }
        }
    }
}

/// Interpreta um endereço: `0x1456FC11C`, `1456fc11c`, `modulo+56FC11C` ou `+0x56FC11C`.
///
/// O separador é o último `+`, porque nomes de módulo não contêm `+`.
pub fn parse_addr(input: &str) -> Result<AddrSpec> {
    match input.rfind('+') {
        Some(at) => {
            let module = &input[..at];
            let offset = parse_hex(input, &input[at + 1..])?;
            Ok(AddrSpec::ModuleOffset {
                module: (!module.is_empty()).then(|| module.to_string()),
                offset,
            })
        }
        None => Ok(AddrSpec::Absolute(parse_hex(input, input)?)),
    }
}

/// Interpreta um tamanho: decimal (`64`) ou hexadecimal com `0x` (`0x40`).
pub fn parse_size(input: &str) -> Result<u64> {
    let bad = |reason: &str| Error::BadAddress {
        input: input.to_string(),
        reason: reason.to_string(),
    };
    if has_hex_prefix(input) {
        return parse_hex(input, input);
    }
    if input.is_empty() || !input.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("use dígitos decimais ou hexadecimal com 0x"));
    }
    input
        .parse::<u64>()
        .map_err(|_| bad("valor fora do intervalo de 64 bits"))
}

fn has_hex_prefix(text: &str) -> bool {
    text.starts_with("0x") || text.starts_with("0X")
}

/// Lê um número hexadecimal, com ou sem `0x`. `input` é o texto completo, para a mensagem de erro.
fn parse_hex(input: &str, digits: &str) -> Result<u64> {
    let bad = |reason: &str| Error::BadAddress {
        input: input.to_string(),
        reason: reason.to_string(),
    };
    let digits = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .unwrap_or(digits);
    if digits.is_empty() {
        return Err(bad("faltam dígitos hexadecimais"));
    }
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad("caractere não hexadecimal"));
    }
    u64::from_str_radix(digits, 16).map_err(|_| bad("valor fora do intervalo de 64 bits"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_with_and_without_prefix() {
        assert_eq!(
            parse_addr("0x1456FC11C").unwrap(),
            AddrSpec::Absolute(0x1456FC11C)
        );
        assert_eq!(
            parse_addr("1456fc11c").unwrap(),
            AddrSpec::Absolute(0x1456FC11C)
        );
    }

    #[test]
    fn module_plus_offset() {
        assert_eq!(
            parse_addr("Grounded2Steam-Win64-Shipping.exe+56FC11C").unwrap(),
            AddrSpec::ModuleOffset {
                module: Some("Grounded2Steam-Win64-Shipping.exe".into()),
                offset: 0x56FC11C
            }
        );
    }

    #[test]
    fn shortcut_plus_offset() {
        assert_eq!(
            parse_addr("+0x56FC11C").unwrap(),
            AddrSpec::ModuleOffset {
                module: None,
                offset: 0x56FC11C
            }
        );
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "+", "xyz", "mod+", "+zz", "0x", "1-2"] {
            assert!(
                matches!(parse_addr(bad), Err(Error::BadAddress { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn resolve_overflow_is_error() {
        let spec = parse_addr("+FFFFFFFFFFFFFFFF").unwrap();
        assert!(matches!(
            spec.resolve(|_| Ok(0x140000000)),
            Err(Error::BadAddress { .. })
        ));
    }

    #[test]
    fn sizes_decimal_or_hex() {
        assert_eq!(parse_size("64").unwrap(), 64);
        assert_eq!(parse_size("0x40").unwrap(), 64);
        assert!(parse_size("40h").is_err());
        assert!(parse_size("-1").is_err());
    }
}
