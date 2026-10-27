//! Valores ajustáveis de um patch: grade `min + k·step` ou lista de pontos, codificação e
//! conferência do imediato.
//!
//! Tudo aqui é puro (sem processo). O valor é sempre um ponto da grade, calculado por `k`
//! (nunca somando `step` repetidas vezes), para o `f32` não acumular erro.

use crate::asm::immediate_slot;

/// Folga relativa a `step` ao decidir se um ponto ainda cabe em `max`.
const GRID_SLACK: f64 = 1e-4;

/// Tamanho, em bytes, do imediato de um patch com `slot` (sempre 4).
const SLOT_SIZE: usize = 4;

/// Tipo do valor (little-endian): 4 bytes, exceto `F64` (8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    /// Inteiro de 32 bits com sinal.
    I32,
    /// Inteiro de 32 bits sem sinal.
    U32,
    /// Ponto flutuante de 32 bits.
    F32,
    /// Ponto flutuante de 64 bits (só em grades por lista de pontos, fora de patches com `slot`).
    F64,
}

impl ValueType {
    /// Tamanho do valor em bytes: 4, ou 8 para `F64`.
    pub fn size(self) -> usize {
        match self {
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

/// Grade de um valor ajustável: `min + k·step` (ou uma lista de pontos), tipo, formatação e
/// codificação.
///
/// Os números em `f64` cobrem `i32`, `u32`, `f32` e `f64` exatamente o bastante. A igualdade
/// entre definições usa `default_k` (o índice na grade), não o valor do default.
///
/// Com `points`, a grade anda pela lista: `steps() == len - 1`, `value(k) == points[k]`, e
/// `min`/`max`/`step` ficam com o primeiro ponto, o último e `0.0` (sem uso).
#[derive(Debug, Clone, PartialEq)]
pub struct ValueGrid {
    /// Tipo do valor.
    pub ty: ValueType,
    /// Menor valor.
    pub min: f64,
    /// Maior valor.
    pub max: f64,
    /// Passo da grade.
    pub step: f64,
    /// Índice do valor padrão na grade.
    pub default_k: u64,
    /// Sufixo mostrado depois do número (ex.: `x`).
    pub unit: Option<String>,
    /// Casas decimais mostradas (só `F32` e `F64`).
    pub decimals: u8,
    /// Lista de pontos estritamente crescente no lugar de `min`/`max`/`step`.
    pub points: Option<Vec<f64>>,
}

impl ValueGrid {
    /// Maior `k` da grade: `len - 1` da lista de pontos, ou o maior `k` com
    /// `min + k·step <= max` (com folga de `step · 1e-4`).
    pub fn steps(&self) -> u64 {
        if let Some(points) = &self.points {
            return points.len().saturating_sub(1) as u64;
        }
        let k = ((self.max - self.min) / self.step + GRID_SLACK).floor();
        if k.is_finite() && k > 0.0 {
            // Conversão saturante: `k` já é inteiro e não negativo.
            k as u64
        } else {
            0
        }
    }

    /// Valor do ponto `k` da grade (`k` limitado a `steps()`; nunca passa de `max`).
    pub fn value(&self, k: u64) -> f64 {
        let k = k.min(self.steps());
        if let Some(points) = &self.points {
            // `k <= len - 1`; lista vazia só existe em grade que `check()` recusa.
            return usize::try_from(k)
                .ok()
                .and_then(|i| points.get(i))
                .copied()
                .unwrap_or(self.min);
        }
        (self.min + k as f64 * self.step).min(self.max)
    }

    /// `k` do ponto da grade mais próximo de `v`, em `[0, steps()]`.
    pub fn nearest_k(&self, v: f64) -> u64 {
        if v.is_nan() {
            return 0;
        }
        if let Some(points) = &self.points {
            // Ponto de menor distância; num empate, o menor `k`.
            let mut best = (0u64, f64::INFINITY);
            for (i, p) in points.iter().enumerate() {
                let dist = (p - v).abs();
                if dist < best.1 {
                    best = (i as u64, dist);
                }
            }
            return best.0;
        }
        let k = ((v - self.min) / self.step).round();
        if k <= 0.0 {
            0
        } else {
            // Saturante; o `min` limita ao fim da grade.
            (k as u64).min(self.steps())
        }
    }

    /// Codificação de 4 bytes do ponto `k`, para patches com `slot`: `I32`/`U32` arredondam o
    /// valor; `F32` converte. `F64` não cabe em 4 bytes: devolve zeros (`check_slot` recusa
    /// `F64`; use `encode_bytes`).
    pub fn encode(&self, k: u64) -> [u8; 4] {
        let v = self.value(k);
        match self.ty {
            // Conversões saturantes: `check()` garante que o valor cabe no tipo.
            ValueType::I32 => (v.round() as i32).to_le_bytes(),
            ValueType::U32 => (v.round() as u32).to_le_bytes(),
            ValueType::F32 => (v as f32).to_le_bytes(),
            ValueType::F64 => [0; 4],
        }
    }

    /// Codificação little-endian do ponto `k` com o tamanho do tipo: 4 bytes, ou 8 para `F64`.
    pub fn encode_bytes(&self, k: u64) -> Vec<u8> {
        match self.ty {
            ValueType::F64 => self.value(k).to_le_bytes().to_vec(),
            ValueType::I32 | ValueType::U32 | ValueType::F32 => self.encode(k).to_vec(),
        }
    }

    /// Texto do ponto `k`: número com `decimals` casas (inteiros sem casas) e a `unit`.
    pub fn format(&self, k: u64) -> String {
        let v = self.value(k);
        let mut text = match self.ty {
            ValueType::F32 | ValueType::F64 => format!("{v:.*}", usize::from(self.decimals)),
            ValueType::I32 | ValueType::U32 => format!("{v:.0}"),
        };
        // Um resíduo negativo minúsculo (ex.: -5e-17) vira "-0.0": mostra "0.0".
        if text.starts_with('-') && text.bytes().all(|b| matches!(b, b'-' | b'0' | b'.')) {
            text.remove(0);
        }
        if let Some(unit) = &self.unit {
            text.push_str(unit);
        }
        text
    }

    /// Confere os números: finitos, `min <= max`, `step > 0`, representáveis no tipo e
    /// `default_k <= steps()`. Com `points`: lista não vazia, finita, estritamente crescente e,
    /// em `I32`/`U32`, de inteiros da faixa do tipo (um ponto só é aceito).
    pub fn check(&self) -> Result<(), String> {
        if let Some(points) = &self.points {
            return self.check_points(points);
        }
        for (name, v) in [("min", self.min), ("max", self.max), ("step", self.step)] {
            if !v.is_finite() {
                return Err(format!("{name} não é um número finito"));
            }
        }
        if self.step <= 0.0 {
            return Err(format!("step {} precisa ser maior que 0", self.step));
        }
        if self.min > self.max {
            return Err(format!("min {} maior que max {}", self.min, self.max));
        }
        match self.ty {
            ValueType::I32 | ValueType::U32 => {
                let (lo, hi) = if self.ty == ValueType::I32 {
                    (f64::from(i32::MIN), f64::from(i32::MAX))
                } else {
                    (0.0, f64::from(u32::MAX))
                };
                for (name, v) in [("min", self.min), ("max", self.max), ("step", self.step)] {
                    if v.fract() != 0.0 {
                        return Err(format!("{name} {v} precisa ser inteiro neste tipo"));
                    }
                }
                if self.min < lo || self.max > hi {
                    return Err(format!(
                        "faixa {}..{} fora do tipo ({lo}..{hi})",
                        self.min, self.max
                    ));
                }
            }
            ValueType::F32 => {
                let limit = f64::from(f32::MAX);
                if self.min.abs() > limit || self.max.abs() > limit || self.step > limit {
                    return Err("valores fora da faixa de f32".to_owned());
                }
            }
            // Já finitos acima; `f64` representa qualquer um deles.
            ValueType::F64 => {}
        }
        self.check_default()
    }

    /// `check()` da grade por lista de pontos.
    fn check_points(&self, points: &[f64]) -> Result<(), String> {
        if points.is_empty() {
            return Err("a lista de pontos está vazia".to_owned());
        }
        if let Some(p) = points.iter().find(|p| !p.is_finite()) {
            return Err(format!("o ponto {p} não é um número finito"));
        }
        if let Some(w) = points.windows(2).find(|w| w[0] >= w[1]) {
            return Err(format!(
                "os pontos precisam ser estritamente crescentes ({} antes de {})",
                w[0], w[1]
            ));
        }
        match self.ty {
            ValueType::I32 | ValueType::U32 => {
                let (lo, hi) = if self.ty == ValueType::I32 {
                    (f64::from(i32::MIN), f64::from(i32::MAX))
                } else {
                    (0.0, f64::from(u32::MAX))
                };
                for &p in points {
                    if p.fract() != 0.0 {
                        return Err(format!("o ponto {p} precisa ser inteiro neste tipo"));
                    }
                    if p < lo || p > hi {
                        return Err(format!("o ponto {p} fora do tipo ({lo}..{hi})"));
                    }
                }
            }
            ValueType::F32 => {
                let limit = f64::from(f32::MAX);
                if let Some(p) = points.iter().find(|p| p.abs() > limit) {
                    return Err(format!("o ponto {p} fora da faixa de f32"));
                }
            }
            ValueType::F64 => {}
        }
        self.check_default()
    }

    /// `default_k <= steps()`.
    fn check_default(&self) -> Result<(), String> {
        if self.default_k > self.steps() {
            return Err(format!(
                "default_k {} além do fim da grade ({})",
                self.default_k,
                self.steps()
            ));
        }
        Ok(())
    }
}

/// Valor ajustável de um patch: a grade e onde ele mora em `replacement`.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueSpec {
    /// Grade de valores.
    pub grid: ValueGrid,
    /// Offset, dentro de `replacement`, onde os bytes do valor vão.
    pub slot: usize,
}

impl ValueSpec {
    /// `replacement` com `slot..slot+4` trocado pela codificação do ponto `k`.
    ///
    /// Se o `slot` não couber em `replacement`, devolve `replacement` sem mudança
    /// (`check_slot` recusa esse caso antes).
    pub fn bytes(&self, replacement: &[u8], k: u64) -> Vec<u8> {
        let mut out = replacement.to_vec();
        if let Some(dst) = self
            .slot
            .checked_add(SLOT_SIZE)
            .and_then(|end| out.get_mut(self.slot..end))
        {
            dst.copy_from_slice(&self.grid.encode(k));
        }
        out
    }

    /// Confere o `slot` em `replacement` com o `iced-x86`: coincide exatamente com o imediato de
    /// 4 bytes de uma instrução que não é desvio, e os bytes ali são o `default_k` codificado.
    ///
    /// Com `U32` num imediato estendido com sinal até 64 bits, `max` precisa caber em `i32::MAX`.
    pub fn check_slot(&self, replacement: &[u8]) -> Result<(), String> {
        if self.grid.ty == ValueType::F64 {
            return Err("f64 não cabe num imediato de 4 bytes".to_owned());
        }
        let end = self.slot.checked_add(SLOT_SIZE);
        let Some(current) = end.and_then(|end| replacement.get(self.slot..end)) else {
            return Err(format!(
                "slot {} + {SLOT_SIZE} não cabe nos {} bytes de replacement",
                self.slot,
                replacement.len()
            ));
        };
        let Some(imm) = immediate_slot(replacement, 0)
            .into_iter()
            .find(|i| i.offset == self.slot && i.size == SLOT_SIZE)
        else {
            return Err(format!(
                "o slot {} não coincide com um imediato de {SLOT_SIZE} bytes de uma instrução",
                self.slot
            ));
        };
        if imm.is_branch {
            return Err(format!(
                "o slot {} é o deslocamento de um desvio, não um valor",
                self.slot
            ));
        }
        if self.grid.ty == ValueType::U32
            && imm.sign_extended_to_64
            && self.grid.max > f64::from(i32::MAX)
        {
            return Err(format!(
                "u32 num imediato estendido com sinal exige max <= i32::MAX, mas max é {}",
                self.grid.max
            ));
        }
        if current != self.grid.encode(self.grid.default_k) {
            return Err(format!(
                "os bytes do slot {current:02X?} não são o default codificado ({:02X?})",
                self.grid.encode(self.grid.default_k)
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{PatchSpec, validate};

    fn f32_grid(min: f64, max: f64, step: f64, default_k: u64) -> ValueGrid {
        ValueGrid {
            ty: ValueType::F32,
            min,
            max,
            step,
            default_k,
            unit: None,
            decimals: 1,
            points: None,
        }
    }

    fn int_grid(ty: ValueType, min: f64, max: f64, step: f64) -> ValueGrid {
        ValueGrid {
            ty,
            min,
            max,
            step,
            default_k: 0,
            unit: None,
            decimals: 0,
            points: None,
        }
    }

    fn f32_spec(min: f64, max: f64, step: f64, default_k: u64, slot: usize) -> ValueSpec {
        ValueSpec {
            grid: f32_grid(min, max, step, default_k),
            slot,
        }
    }

    #[test]
    fn grid_by_k_has_no_drift() {
        let s = f32_grid(0.1, 10.0, 0.1, 0);
        assert_eq!(s.steps(), 99);
        assert!((s.value(99) - 10.0).abs() < 1e-6);
        assert!((s.value(0) - 0.1).abs() < 1e-12);
        // k acima do limite é limitado.
        assert!((s.value(1000) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn nearest_k_clamps() {
        let s = f32_grid(1.0, 5.0, 0.5, 0);
        assert_eq!(s.nearest_k(-3.0), 0);
        assert_eq!(s.nearest_k(1.0), 0);
        assert_eq!(s.nearest_k(1.26), 1);
        assert_eq!(s.nearest_k(99.0), s.steps());
        assert_eq!(s.nearest_k(f64::NAN), 0);
    }

    #[test]
    fn encode_types() {
        let i = int_grid(ValueType::I32, -10.0, 10.0, 1.0);
        // k = 9 → -1
        assert_eq!(i.encode(9), [0xFF, 0xFF, 0xFF, 0xFF]);
        let u = int_grid(ValueType::U32, 0.0, 100.0, 1.0);
        assert_eq!(u.encode(7), [0x07, 0, 0, 0]);
        let f = f32_grid(0.0, 10.0, 0.5, 0);
        assert_eq!(f.encode(3), [0x00, 0x00, 0xC0, 0x3F]);
    }

    #[test]
    fn format_units() {
        let mut f = f32_grid(0.0, 10.0, 0.5, 0);
        f.unit = Some("x".into());
        assert_eq!(f.format(3), "1.5x");
        let i = int_grid(ValueType::I32, 0.0, 1000.0, 100.0);
        assert_eq!(i.format(1), "100");
        let mut neg = f32_grid(-1.0, 1.0, 0.1, 0);
        neg.decimals = 1;
        assert_eq!(neg.format(10), "0.0");
    }

    #[test]
    fn grid_check_without_slot() {
        let g = ValueGrid {
            ty: ValueType::I32,
            min: 100.0,
            max: 10_000.0,
            step: 100.0,
            default_k: 9,
            unit: None,
            decimals: 0,
            points: None,
        };
        assert_eq!(g.check(), Ok(()));
        assert_eq!(g.format(g.default_k), "1000");
    }

    #[test]
    fn bytes_replace_only_the_slot() {
        let s = f32_spec(0.0, 10.0, 0.5, 0, 1);
        let out = s.bytes(&[0xB8, 0, 0, 0, 0, 0x90], 3);
        assert_eq!(out, vec![0xB8, 0x00, 0x00, 0xC0, 0x3F, 0x90]);
    }

    #[test]
    fn check_accepts_valid_and_rejects_bad_numbers() {
        assert!(f32_grid(0.0, 10.0, 0.5, 3).check().is_ok());
        assert!(f32_grid(5.0, 1.0, 0.5, 0).check().is_err());
        assert!(f32_grid(0.0, 10.0, 0.0, 0).check().is_err());
        assert!(f32_grid(0.0, 10.0, -1.0, 0).check().is_err());
        assert!(f32_grid(0.0, f64::INFINITY, 1.0, 0).check().is_err());
        assert!(f32_grid(0.0, f64::NAN, 1.0, 0).check().is_err());
        assert!(f32_grid(0.0, 1e300, 1.0, 0).check().is_err());
        // default_k além da grade
        assert!(f32_grid(0.0, 10.0, 0.5, 21).check().is_err());
        assert!(f32_grid(0.0, 10.0, 0.5, 20).check().is_ok());
        // inteiros: faixa do tipo e valores inteiros
        assert!(int_grid(ValueType::U32, -1.0, 10.0, 1.0).check().is_err());
        assert!(int_grid(ValueType::U32, 0.0, 4e9, 1.0).check().is_ok());
        assert!(int_grid(ValueType::U32, 0.0, 5e9, 1.0).check().is_err());
        assert!(int_grid(ValueType::I32, -3e9, 0.0, 1.0).check().is_err());
        assert!(int_grid(ValueType::I32, 0.0, 10.0, 0.5).check().is_err());
    }

    #[test]
    fn check_slot_accepts_mov_eax_imm32() {
        let s = f32_spec(0.0, 10.0, 0.5, 3, 1);
        assert_eq!(s.check_slot(&[0xB8, 0x00, 0x00, 0xC0, 0x3F]), Ok(()));
    }

    #[test]
    fn check_slot_rejects() {
        let s = f32_spec(0.0, 10.0, 0.5, 3, 1);
        let mov = [0xB8, 0x00, 0x00, 0xC0, 0x3F];
        // slot 0 (o opcode, não o imediato)
        let at0 = ValueSpec {
            slot: 0,
            ..s.clone()
        };
        assert!(at0.check_slot(&mov).is_err());
        // slot além do fim
        let far = ValueSpec {
            slot: 3,
            ..s.clone()
        };
        assert!(far.check_slot(&mov).is_err());
        // jmp rel32: o deslocamento é imediato, mas é desvio
        let jmp = [0xE9, 0x00, 0x00, 0xC0, 0x3F];
        let err = s.check_slot(&jmp).unwrap_err();
        assert!(err.contains("desvio"), "{err}");
        // push imm8: imediato menor que o tipo
        assert!(s.check_slot(&[0x6A, 0x05, 0x90, 0x90, 0x90]).is_err());
        // mov rax, imm32 com U32 e max > i32::MAX
        let u = ValueSpec {
            grid: ValueGrid {
                ty: ValueType::U32,
                min: 0.0,
                max: 4_000_000_000.0,
                step: 1.0,
                default_k: 1,
                unit: None,
                decimals: 0,
                points: None,
            },
            slot: 3,
        };
        let mov_rax = [0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00];
        let err = u.check_slot(&mov_rax).unwrap_err();
        assert!(err.contains("i32::MAX"), "{err}");
        // o mesmo com max dentro de i32::MAX passa
        let ok = ValueSpec {
            grid: ValueGrid {
                max: f64::from(i32::MAX),
                ..u.grid
            },
            ..u
        };
        assert_eq!(ok.check_slot(&mov_rax), Ok(()));
        // bytes do slot diferentes do default
        let wrong = [0xB8, 0x00, 0x00, 0x80, 0x3F];
        let err = s.check_slot(&wrong).unwrap_err();
        assert!(err.contains("default"), "{err}");
    }

    #[test]
    fn grid_keeps_instruction_boundaries() {
        let s = f32_spec(0.0, 10.0, 0.5, 0, 1);
        let replacement = [0xB8, 0x00, 0x00, 0x00, 0x00, 0x90];
        let expected = vec![0x90; 6];
        assert_eq!(s.check_slot(&replacement), Ok(()));
        let patch = |bytes: Vec<u8>| PatchSpec {
            id: "t".into(),
            addr: 0x1000,
            expected: expected.clone(),
            replacement: bytes,
            kind: crate::patch::PatchKind::Code,
        };
        let base = validate(patch(replacement.to_vec())).unwrap();
        assert_eq!(s.grid.steps(), 20);
        for k in 0..=s.grid.steps() {
            let v = validate(patch(s.bytes(&replacement, k))).unwrap();
            assert_eq!(v.replacement_starts, base.replacement_starts, "k = {k}");
            assert_eq!(v.expected_starts, base.expected_starts, "k = {k}");
        }
    }

    fn points_grid(ty: ValueType, points: &[f64], default_k: u64) -> ValueGrid {
        ValueGrid {
            ty,
            min: points.first().copied().unwrap_or(0.0),
            max: points.last().copied().unwrap_or(0.0),
            step: 0.0,
            default_k,
            unit: None,
            decimals: 0,
            points: Some(points.to_vec()),
        }
    }

    #[test]
    fn points_grid_walks_list() {
        let g = points_grid(ValueType::I32, &[2.0, 3.0, 5.0, 10.0], 0);
        assert_eq!(g.check(), Ok(()));
        assert_eq!(g.steps(), 3);
        assert_eq!(g.value(0), 2.0);
        assert_eq!(g.value(2), 5.0);
        // k acima do fim é limitado ao último ponto.
        assert_eq!(g.value(99), 10.0);
        assert_eq!(g.nearest_k(4.0), 1);
        assert_eq!(g.nearest_k(4.1), 2);
        assert_eq!(g.nearest_k(-5.0), 0);
        assert_eq!(g.nearest_k(1000.0), 3);
        assert_eq!(g.nearest_k(f64::NAN), 0);
        assert_eq!(g.encode(2), 5i32.to_le_bytes());
    }

    #[test]
    fn points_single_has_no_steps() {
        let g = points_grid(ValueType::F64, &[3600.0], 0);
        assert_eq!(g.check(), Ok(()));
        assert_eq!(g.steps(), 0);
        assert_eq!(g.value(0), 3600.0);
        assert_eq!(g.value(7), 3600.0);
        assert_eq!(g.nearest_k(1.0), 0);
    }

    #[test]
    fn points_rejects_unsorted() {
        assert!(
            points_grid(ValueType::I32, &[2.0, 2.0, 5.0], 0)
                .check()
                .is_err()
        );
        assert!(points_grid(ValueType::I32, &[5.0, 2.0], 0).check().is_err());
        assert!(points_grid(ValueType::I32, &[], 0).check().is_err());
        assert!(
            points_grid(ValueType::F64, &[1.0, f64::NAN], 0)
                .check()
                .is_err()
        );
        assert!(
            points_grid(ValueType::F64, &[1.0, f64::INFINITY], 0)
                .check()
                .is_err()
        );
        // default_k fora da lista
        assert!(points_grid(ValueType::I32, &[2.0, 3.0], 2).check().is_err());
    }

    #[test]
    fn points_rejects_non_integer_i32() {
        assert!(points_grid(ValueType::I32, &[2.0, 2.5], 0).check().is_err());
        assert!(points_grid(ValueType::I32, &[1.0, 3e9], 0).check().is_err());
        assert!(points_grid(ValueType::F32, &[2.0, 2.5], 0).check().is_ok());
    }

    #[test]
    fn f64_encodes_8_bytes() {
        let g = points_grid(ValueType::F64, &[3600.0], 0);
        assert_eq!(ValueType::F64.size(), 8);
        assert_eq!(ValueType::I32.size(), 4);
        assert_eq!(g.encode_bytes(0), 3600f64.to_le_bytes().to_vec());
        let i = int_grid(ValueType::I32, -10.0, 10.0, 1.0);
        assert_eq!(i.encode_bytes(9), vec![0xFF; 4]);
        // f64 não serve a um patch com slot.
        let spec = ValueSpec { grid: g, slot: 1 };
        assert!(spec.check_slot(&[0xB8, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn format_unit_suffix() {
        let mut g = points_grid(ValueType::I32, &[2.0, 3.0, 5.0, 10.0], 0);
        g.unit = Some("x".into());
        assert_eq!(g.format(2), "5x");
        let mut f = points_grid(ValueType::F64, &[3600.0], 0);
        f.decimals = 1;
        assert_eq!(f.format(0), "3600.0");
    }
}
