//! `gt-patch toggle <id> --times N`: liga e desliga um patch e mede a pausa (spec 6.10).

use std::io::{self, Write};
use std::time::Duration;

use gt_core::session::{ToggleReport, toggle};
use gt_grounded2::Error;
use gt_grounded2::signatures::Signatures;

use super::{
    begin_and_show, open_catalog, open_engine, report_unrestored, select_patches, warn_unverified,
};

/// Pausa máxima a partir da qual o relatório avisa.
const PAUSE_WARN: Duration = Duration::from_millis(50);

/// Milissegundos com uma casa decimal.
fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

/// Texto do relatório do `toggle`.
pub fn toggle_report_text(report: &ToggleReport) -> String {
    let mut text = format!(
        "ciclos: {}\nnovas tentativas: {} (threads no trecho) + {} (prazo)\npausa média: {} ms, máxima: {} ms\n",
        report.cycles,
        report.danger_retries,
        report.timeout_retries,
        ms(report.pause_avg),
        ms(report.pause_max),
    );
    if report.pause_max >= PAUSE_WARN {
        text.push_str("aviso: pausa máxima acima de 50 ms\n");
    }
    text
}

/// Recusa o `id` de um cheat de valor (`[[field]]`): ele só vale na TUI, não é um patch de bytes.
pub fn refuse_field_cheat(sigs: &Signatures, id: &str) -> Result<(), Error> {
    if sigs.fields().iter().any(|f| f.id == id) {
        return Err(Error::FieldCheat { id: id.to_string() });
    }
    Ok(())
}

pub fn run(pid: Option<i32>, id: &str, times: u32, interval_ms: u64) -> anyhow::Result<()> {
    // Antes de abrir o jogo ou a sessão: a recusa não depende deles.
    refuse_field_cheat(&Signatures::embedded()?, id)?;
    let session = begin_and_show()?;
    let (target, scan) = open_catalog(pid)?;
    let mut selected = select_patches(&scan, &[id.to_string()])?;
    // Declarado depois da `session`: o motor cai antes de a trava ser solta.
    let mut engine = open_engine(&target, &scan, &session)?;
    // `select_patches` devolve exatamente o patch pedido.
    let Some(chosen) = selected.pop() else {
        anyhow::bail!("patch {id} não selecionado");
    };
    let patch = chosen.patch.clone();
    let interval = Duration::from_millis(interval_ms);
    // O aviso de não verificado só sai depois do primeiro `apply` que deu certo.
    match toggle(
        &mut engine,
        patch,
        times,
        interval,
        &session.shutdown,
        || warn_unverified(std::slice::from_ref(&chosen)),
    ) {
        Ok(report) => {
            let mut out = io::stdout().lock();
            write!(out, "{}", toggle_report_text(&report))?;
            out.flush()?;
            Ok(())
        }
        Err(err) => {
            // O patch pode ter ficado ligado: tenta desfazer e diz o que sobrou.
            if !engine.active().is_empty() && engine.undo_all().is_err() {
                report_unrestored(&engine);
            }
            Err(err.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERIC: &str = r#"
[[patch]]
id = "p"
name = "Patch"
category = "itens"
description = "d"
pattern = "48 8B F9"
offset = 0
expected = "48 8B F9"
replacement = "48 89 CF"

[[field]]
id = "vida-infinita"
name = "Vida infinita"
category = "sobrevivencia"
description = "d"
object = "pawn"
object_class = "/Script/Maine.SurvivalPlayerCharacter"
rule = "lock"
field = "CurrentDamage"
kind = "float"
mode = "lower"
target = 0
min = 0
max = 10000
"#;

    #[test]
    fn field_id_is_refused_and_patch_id_is_not() {
        let sigs = Signatures::parse(&[("generic.toml", GENERIC)]).unwrap();
        let err = refuse_field_cheat(&sigs, "vida-infinita").unwrap_err();
        assert!(matches!(&err, Error::FieldCheat { id } if id == "vida-infinita"));
        assert_eq!(
            err.to_string(),
            "vida-infinita é um cheat de valor; use o gt-tui"
        );
        assert!(refuse_field_cheat(&sigs, "p").is_ok());
        // Id desconhecido segue para o erro de sempre (patch desconhecido), depois de abrir o jogo.
        assert!(refuse_field_cheat(&sigs, "nao-existe").is_ok());
    }
}
