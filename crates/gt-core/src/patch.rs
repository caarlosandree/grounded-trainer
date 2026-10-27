//! Patches de bytes: validação, fronteiras de instrução e regra de perigo.
//!
//! Tudo aqui é puro: não toca em processo nem em arquivo. O motor (que escreve) usa
//! [`Validated`], [`is_dangerous`] e [`WritePolicy`].

use std::ops::Range;

use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Tamanho máximo de um patch de código (desvio), em bytes.
pub const MAX_PATCH_LEN: usize = 64;

/// Tamanho máximo de uma cave, em bytes (spec 008, 6.4).
pub const MAX_CAVE_LEN: usize = 4096;

/// Byte de preenchimento (`int3`) sobre o qual uma cave é escrita.
pub const CAVE_FILL: u8 = 0xCC;

/// Tipo de um patch: o que a regra de perigo e a validação esperam dele.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatchKind {
    /// Instruções inteiras do jogo trocadas por outras (1 a [`MAX_PATCH_LEN`] bytes); só os
    /// pontos de retomada no meio do trecho que perdem fronteira de instrução são perigosos.
    #[default]
    Code,
    /// Código nosso escrito sobre preenchimento `0xCC` (1 a [`MAX_CAVE_LEN`] bytes): qualquer
    /// `pc` dentro do trecho (inclusive o início) e qualquer posição desconhecida são perigosos.
    Cave,
}

impl PatchKind {
    /// `true` para [`PatchKind::Code`] (usado para não gravar o `kind` padrão no diário).
    pub fn is_code(&self) -> bool {
        *self == PatchKind::Code
    }
}

/// Um patch: troca `expected` por `replacement` a partir de `addr`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchSpec {
    /// Identificador usado em mensagens e no diário.
    pub id: String,
    /// Endereço absoluto do primeiro byte.
    pub addr: u64,
    /// Bytes que devem estar na memória antes de aplicar.
    pub expected: Vec<u8>,
    /// Bytes escritos ao aplicar (mesmo tamanho de `expected`).
    pub replacement: Vec<u8>,
    /// Tipo do patch (desvio ou cave).
    pub kind: PatchKind,
}

impl PatchSpec {
    /// Intervalo de endereços coberto pelo patch.
    pub fn range(&self) -> Range<u64> {
        let len = u64::try_from(self.expected.len()).unwrap_or(u64::MAX);
        self.addr..self.addr.saturating_add(len)
    }
}

/// Patch que passou na validação, com os inícios de instrução de cada lado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validated {
    /// O patch validado.
    pub spec: PatchSpec,
    /// Endereços absolutos de início de instrução nos bytes esperados (inclui `addr`).
    pub expected_starts: Vec<u64>,
    /// Endereços absolutos de início de instrução nos bytes novos (inclui `addr`).
    pub replacement_starts: Vec<u64>,
}

impl Validated {
    /// Inícios dos esperados que não são inícios dos novos (fronteiras perdidas ao aplicar).
    pub fn lost_on_apply(&self) -> Vec<u64> {
        difference(&self.expected_starts, &self.replacement_starts)
    }

    /// Inícios dos novos que não são inícios dos esperados (fronteiras perdidas ao desfazer).
    pub fn lost_on_undo(&self) -> Vec<u64> {
        difference(&self.replacement_starts, &self.expected_starts)
    }
}

fn difference(from: &[u64], other: &[u64]) -> Vec<u64> {
    from.iter()
        .copied()
        .filter(|a| !other.contains(a))
        .collect()
}

/// Sentido da escrita de um patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Esperados -> novos.
    Apply,
    /// Novos -> esperados.
    Undo,
}

/// Inícios de instrução de um lado e os endereços logo depois de cada `call` que não é a
/// última instrução.
struct Decoded {
    starts: Vec<u64>,
    call_returns: Vec<u64>,
}

fn decode_side(id: &str, side: &str, bytes: &[u8], addr: u64) -> Result<Decoded> {
    let bad = |reason: String| Error::BadPatch {
        id: id.to_owned(),
        reason,
    };
    let end = addr + bytes.len() as u64;
    let mut decoder = Decoder::with_ip(64, bytes, addr, DecoderOptions::NONE);
    let mut starts = Vec::new();
    let mut call_returns = Vec::new();
    while decoder.can_decode() {
        let insn = decoder.decode();
        if insn.is_invalid() {
            return Err(bad(format!(
                "os bytes {side} não formam instruções inteiras (inválida em {:#x})",
                insn.ip()
            )));
        }
        starts.push(insn.ip());
        let next = insn.next_ip();
        // `syscall` tem `FlowControl::Call` no iced-x86, mas não empilha endereço de
        // retorno: quem vigia é a regra `pc`/`pc - 2` da pausa.
        if insn.mnemonic() == Mnemonic::Call && next < end {
            call_returns.push(next);
        }
    }
    Ok(Decoded {
        starts,
        call_returns,
    })
}

/// Valida um patch: tamanhos, instruções inteiras e a regra do `call` nos dois sentidos.
pub fn validate(spec: PatchSpec) -> Result<Validated> {
    let bad = |reason: String| Error::BadPatch {
        id: spec.id.clone(),
        reason,
    };
    if spec.expected.len() != spec.replacement.len() {
        return Err(bad(format!(
            "tamanhos diferentes ({} esperados, {} novos)",
            spec.expected.len(),
            spec.replacement.len()
        )));
    }
    let len = spec.expected.len();
    let max_len = match spec.kind {
        PatchKind::Code => MAX_PATCH_LEN,
        PatchKind::Cave => MAX_CAVE_LEN,
    };
    if len == 0 || len > max_len {
        return Err(bad(format!(
            "tamanho {len} fora do limite de 1 a {max_len} bytes"
        )));
    }
    if spec.kind == PatchKind::Cave && spec.expected.iter().any(|b| *b != CAVE_FILL) {
        return Err(bad(format!(
            "os bytes esperados de uma cave devem ser todos {CAVE_FILL:#04X}"
        )));
    }
    if spec.addr.checked_add(len as u64).is_none() {
        return Err(bad(format!(
            "endereço {:#x} estoura o espaço de 64 bits",
            spec.addr
        )));
    }

    let expected = decode_side(&spec.id, "esperados", &spec.expected, spec.addr)?;
    let replacement = decode_side(&spec.id, "novos", &spec.replacement, spec.addr)?;

    // Numa cave os bytes esperados são só preenchimento e nada aponta para ela quando é
    // escrita ou desfeita: não há retorno de `call` a conferir nem fronteira a comparar.
    if spec.kind == PatchKind::Cave {
        return Ok(Validated {
            spec,
            expected_starts: expected.starts,
            replacement_starts: replacement.starts,
        });
    }

    for (side, other_side, ret_points, other_starts) in [
        (
            "esperados",
            "novos",
            &expected.call_returns,
            &replacement.starts,
        ),
        (
            "novos",
            "esperados",
            &replacement.call_returns,
            &expected.starts,
        ),
    ] {
        if let Some(ret) = ret_points.iter().find(|r| !other_starts.contains(r)) {
            return Err(bad(format!(
                "o retorno do call dos bytes {side} ({ret:#x}) não é início de instrução nos bytes {other_side}"
            )));
        }
    }

    Ok(Validated {
        spec,
        expected_starts: expected.starts,
        replacement_starts: replacement.starts,
    })
}

/// Diz se um ponto de retomada de thread é perigoso para o patch no sentido dado.
///
/// `point` é um `pc` (ou `pc - 2` em syscall); `None` é posição ilegível (ou thread rodando).
///
/// [`PatchKind::Code`]: `None` é perigoso só se o sentido tem fronteira perdida, e o início do
/// trecho nunca é perigoso. [`PatchKind::Cave`]: qualquer ponto de `range.start` a
/// `range.end - 1` (inclusive o início) e `None` são sempre perigosos, nos dois sentidos.
pub fn is_dangerous(v: &Validated, dir: Direction, point: Option<u64>) -> bool {
    if v.spec.kind == PatchKind::Cave {
        return match point {
            None => true,
            Some(p) => v.spec.range().contains(&p),
        };
    }
    let (written_starts, lost) = match dir {
        Direction::Apply => (&v.replacement_starts, v.lost_on_apply()),
        Direction::Undo => (&v.expected_starts, v.lost_on_undo()),
    };
    match point {
        None => !lost.is_empty(),
        Some(p) => {
            let range = v.spec.range();
            range.start < p && p < range.end && !written_starts.contains(&p)
        }
    }
}

/// Intervalos de memória em que o motor pode escrever.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WritePolicy {
    ranges: Vec<Range<u64>>,
}

impl WritePolicy {
    /// Cria a política, fundindo intervalos que se tocam ou se sobrepõem (VMAs vizinhas da
    /// mesma seção não podem recusar um patch que cruza a emenda). Intervalos vazios caem.
    pub fn new(mut ranges: Vec<Range<u64>>) -> Self {
        ranges.retain(|r| r.start < r.end);
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
        for r in ranges {
            match merged.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        Self { ranges: merged }
    }

    /// `true` se o intervalo inteiro cabe dentro de **um** intervalo da política.
    pub fn allows(&self, r: &Range<u64>) -> bool {
        r.start < r.end
            && self
                .ranges
                .iter()
                .any(|p| p.start <= r.start && r.end <= p.end)
    }
}

/// `true` se os dois intervalos (semiabertos) têm algum endereço em comum.
pub fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < a.end && b.start < b.end && a.start < b.end && b.start < a.end
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: u64 = 0x1000;

    fn spec(expected: &[u8], replacement: &[u8]) -> PatchSpec {
        PatchSpec {
            id: "t".into(),
            addr: ADDR,
            expected: expected.to_vec(),
            replacement: replacement.to_vec(),
            kind: PatchKind::Code,
        }
    }

    fn cave_spec(len: usize, replacement: &[u8]) -> PatchSpec {
        let mut replacement = replacement.to_vec();
        replacement.resize(len, 0x90);
        PatchSpec {
            id: "cave".into(),
            addr: ADDR,
            expected: vec![CAVE_FILL; len],
            replacement,
            kind: PatchKind::Cave,
        }
    }

    fn bad_reason(r: Result<Validated>) -> String {
        match r {
            Err(Error::BadPatch { reason, .. }) => reason,
            other => panic!("esperava BadPatch, veio {other:?}"),
        }
    }

    #[test]
    fn equivalent_mov_has_no_lost_boundaries() {
        let v = validate(spec(&[0x48, 0x8B, 0xF9], &[0x48, 0x89, 0xCF])).unwrap();
        assert_eq!(v.expected_starts, vec![ADDR]);
        assert_eq!(v.replacement_starts, vec![ADDR]);
        assert!(v.lost_on_apply().is_empty());
        assert!(v.lost_on_undo().is_empty());
    }

    #[test]
    fn rejects_size_mismatch_empty_and_too_long() {
        let r = validate(spec(&[0x48, 0x8B, 0xF9], &[0x66, 0x90]));
        assert!(matches!(r, Err(Error::BadPatch { .. })));
        let r = validate(spec(&[], &[]));
        assert!(matches!(r, Err(Error::BadPatch { .. })));
        let long = vec![0x90; MAX_PATCH_LEN + 1];
        let r = validate(spec(&long, &long));
        assert!(matches!(r, Err(Error::BadPatch { .. })));
        let ok = vec![0x90; MAX_PATCH_LEN];
        assert!(validate(spec(&ok, &ok)).is_ok());
    }

    #[test]
    fn rejects_partial_instruction() {
        let r = validate(spec(&[0x48, 0x8B], &[0x66, 0x90]));
        assert!(matches!(r, Err(Error::BadPatch { .. })));
        let r = validate(spec(&[0x66, 0x90], &[0x48, 0x8B]));
        assert!(matches!(r, Err(Error::BadPatch { .. })));
    }

    #[test]
    fn rejects_address_overflow() {
        let mut s = spec(&[0x90, 0x90], &[0x66, 0x90]);
        s.addr = u64::MAX;
        assert!(matches!(validate(s), Err(Error::BadPatch { .. })));
    }

    #[test]
    fn two_into_one_loses_middle_boundary() {
        let v = validate(spec(&[0x90, 0x90], &[0x66, 0x90])).unwrap();
        assert_eq!(v.lost_on_apply(), vec![ADDR + 1]);
        assert!(v.lost_on_undo().is_empty());
    }

    #[test]
    fn call_return_point_must_exist_on_both_sides() {
        let call_nop = [0xE8, 0, 0, 0, 0, 0x90];
        let nop5_nop = [0x0F, 0x1F, 0x44, 0x00, 0x00, 0x90];
        let nop6 = [0x66, 0x0F, 0x1F, 0x44, 0x00, 0x00];
        assert!(validate(spec(&call_nop, &nop5_nop)).is_ok());
        assert!(bad_reason(validate(spec(&call_nop, &nop6))).contains("call"));
        assert!(bad_reason(validate(spec(&nop6, &call_nop))).contains("call"));
    }

    #[test]
    fn call_as_last_instruction_needs_no_return_point() {
        let call = [0xE8, 0, 0, 0, 0];
        let nop5 = [0x0F, 0x1F, 0x44, 0x00, 0x00];
        assert!(validate(spec(&call, &nop5)).is_ok());
    }

    #[test]
    fn indirect_call_counts_as_call() {
        // call rax; nop  ->  nop2; nop2 (o retorno em +2 existe nos dois lados)
        let call_rax = [0xFF, 0xD0, 0x90];
        let one_nop3 = [0x0F, 0x1F, 0x00];
        assert!(bad_reason(validate(spec(&call_rax, &one_nop3))).contains("call"));
        let nop2_nop = [0x66, 0x90, 0x90];
        assert!(validate(spec(&call_rax, &nop2_nop)).is_ok());
    }

    #[test]
    fn danger_rule() {
        let v = validate(spec(
            &[0x90, 0x0F, 0x05, 0x90, 0x90],
            &[0x66, 0x90, 0x0F, 0x1F, 0x00],
        ))
        .unwrap();
        assert_eq!(v.expected_starts, vec![ADDR, ADDR + 1, ADDR + 3, ADDR + 4]);
        assert_eq!(v.replacement_starts, vec![ADDR, ADDR + 2]);
        let d = |dir, p| is_dangerous(&v, dir, p);
        assert!(!d(Direction::Apply, Some(0x1000)));
        assert!(d(Direction::Apply, Some(0x1003)));
        assert!(d(Direction::Apply, Some(0x1001)));
        assert!(!d(Direction::Apply, Some(0x1005)));
        assert!(!d(Direction::Apply, Some(0x0fff)));
        assert!(d(Direction::Apply, None));
        // Desfazer: o lado escrito são os bytes esperados; o início 0x1002 dos novos se perde.
        assert_eq!(v.lost_on_undo(), vec![ADDR + 2]);
        assert!(!d(Direction::Undo, Some(0x1001)));
        assert!(d(Direction::Undo, Some(0x1002)));
        assert!(d(Direction::Undo, None));

        let same = validate(spec(&[0x48, 0x8B, 0xF9], &[0x48, 0x89, 0xCF])).unwrap();
        assert!(!is_dangerous(&same, Direction::Apply, None));
        assert!(!is_dangerous(&same, Direction::Undo, None));
    }

    #[test]
    fn cave_validate_requires_cc() {
        // mov rax, 1; ret (+ nops até 8 bytes)
        let code = [0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00, 0xC3];
        assert!(validate(cave_spec(8, &code)).is_ok());
        let mut s = cave_spec(8, &code);
        s.expected[3] = 0x90;
        assert!(bad_reason(validate(s)).contains("0xCC"));
        // O mesmo conteúdo como `Code` passa pela regra de 64 bytes (não exige CC).
        let mut as_code = cave_spec(8, &code);
        as_code.kind = PatchKind::Code;
        assert!(validate(as_code).is_ok());
        // Cave com instrução cortada nos bytes novos não é decodificável inteira.
        let mut cut = cave_spec(8, &[0x48, 0xC7, 0xC0, 0x01]);
        cut.replacement = vec![0x48, 0xC7, 0xC0, 0x01, 0, 0, 0, 0x48];
        assert!(matches!(validate(cut), Err(Error::BadPatch { .. })));
    }

    #[test]
    fn cave_validate_max_len() {
        assert!(validate(cave_spec(MAX_CAVE_LEN, &[0xC3])).is_ok());
        assert!(matches!(
            validate(cave_spec(MAX_CAVE_LEN + 1, &[0xC3])),
            Err(Error::BadPatch { .. })
        ));
        assert!(validate(cave_spec(MAX_PATCH_LEN + 1, &[0xC3])).is_ok());
        assert!(matches!(
            validate(PatchSpec {
                expected: Vec::new(),
                replacement: Vec::new(),
                ..cave_spec(1, &[0xC3])
            }),
            Err(Error::BadPatch { .. })
        ));
    }

    #[test]
    fn cave_any_pc_inside_is_dangerous() {
        let v = validate(cave_spec(16, &[0xC3])).unwrap();
        for dir in [Direction::Apply, Direction::Undo] {
            let d = |p| is_dangerous(&v, dir, p);
            assert!(d(Some(ADDR)), "o início conta");
            assert!(d(Some(ADDR + 7)), "o meio conta");
            assert!(d(Some(ADDR + 15)), "o último byte conta");
            assert!(!d(Some(ADDR + 16)));
            assert!(!d(Some(ADDR - 1)));
            assert!(d(None), "posição desconhecida conta");
        }
    }

    #[test]
    fn code_rules_unchanged() {
        // Desvio de 5 bytes: jmp rel32 no lugar de uma instrução de 5 bytes; sem fronteira
        // perdida, o início e `None` não são perigosos.
        let v = validate(spec(&[0xB8, 1, 0, 0, 0], &[0xE9, 0x10, 0x00, 0x00, 0x00])).unwrap();
        assert_eq!(v.spec.kind, PatchKind::Code);
        assert!(!is_dangerous(&v, Direction::Apply, Some(ADDR)));
        assert!(!is_dangerous(&v, Direction::Apply, None));
        assert!(!is_dangerous(&v, Direction::Undo, None));
        // Instrução de 5 bytes + nop de 3: o meio é perigoso só se perde fronteira.
        let lost = validate(spec(&[0x90, 0x90, 0x90], &[0x0F, 0x1F, 0x00])).unwrap();
        assert!(is_dangerous(&lost, Direction::Apply, Some(ADDR + 1)));
        assert!(!is_dangerous(&lost, Direction::Apply, Some(ADDR)));
        assert!(is_dangerous(&lost, Direction::Apply, None));
        assert!(!is_dangerous(&lost, Direction::Undo, None));
        // O limite de 64 bytes continua valendo para `Code`.
        let long = vec![0x90; MAX_PATCH_LEN + 1];
        assert!(matches!(
            validate(spec(&long, &long)),
            Err(Error::BadPatch { .. })
        ));
    }

    #[test]
    fn syscall_is_not_a_call() {
        let v = validate(spec(
            &[0x0F, 0x05, 0x90, 0x90, 0x90, 0x90],
            &[0x66, 0x66, 0x66, 0x66, 0x66, 0x90],
        ))
        .unwrap();
        assert!(v.lost_on_apply().contains(&(ADDR + 2)));
    }

    #[test]
    fn policy_allows_only_whole_ranges() {
        let section: Range<u64> = 0x1000..0x2000;
        let p = WritePolicy::new(vec![section]);
        assert!(!p.allows(&(0x1ffe..0x2002)));
        assert!(p.allows(&(0x1000..0x1003)));
        assert!(p.allows(&(0x1ffd..0x2000)));
        assert!(!p.allows(&(0x0fff..0x1002)));
        assert!(!p.allows(&(0x1500..0x1500)));
    }

    #[test]
    fn policy_merges_touching_and_overlapping_ranges() {
        let p = WritePolicy::new(vec![0x2000..0x3000, 0x1000..0x2000, 0x2800..0x3800]);
        assert!(p.allows(&(0x1ffe..0x2002)));
        assert!(p.allows(&(0x1000..0x3800)));
        let gap = WritePolicy::new(vec![0x1000..0x2000, 0x2001..0x3000]);
        assert!(!gap.allows(&(0x1ffe..0x2002)));
    }

    #[test]
    fn overlaps_is_half_open() {
        assert!(overlaps(&(0..10), &(9..20)));
        assert!(!overlaps(&(0..10), &(10..20)));
        assert!(overlaps(&(5..6), &(0..10)));
        assert!(!overlaps(&(5..5), &(0..10)));
    }
}
