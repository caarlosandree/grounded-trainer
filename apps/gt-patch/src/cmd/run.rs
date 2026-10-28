//! `gt-patch run <id>...`: aplica, espera um pedido de encerramento e desfaz (spec 6.10).

use std::io::{self, Write};
use std::time::Duration;

use gt_core::session::run_until_shutdown;

use super::{
    begin_and_show, location, open_catalog, open_engine, report_undone, select_patches,
    warn_unverified,
};

/// O laço acorda a cada 200 ms (spec 12).
const TICK: Duration = Duration::from_millis(200);

pub fn run(pid: Option<i32>, ids: &[String]) -> anyhow::Result<()> {
    let session = begin_and_show()?;
    let (target, scan) = open_catalog(pid)?;
    let selected = select_patches(&scan, ids)?;
    // Declarado depois da `session`: o motor cai antes de a trava ser solta.
    let mut engine = open_engine(&target, &scan, &session)?;
    engine.apply(selected.iter().map(|s| s.patch.clone()).collect())?;
    warn_unverified(&selected);
    {
        let mut out = io::stdout().lock();
        for s in &selected {
            let spec = &s.patch.spec;
            writeln!(
                out,
                "ativo: {} em {}",
                spec.id,
                location(&target, spec.addr)
            )?;
        }
        out.flush()?;
    }

    // Erro do laço: o `Drop` do motor desfaz.
    run_until_shutdown(&mut engine, &session.shutdown, TICK)?;
    let undone = engine.undo_all();

    // Daqui em diante o terminal pode ter fechado: falha ao escrever não interrompe nada.
    report_undone(&engine, undone, &mut io::stdout().lock())
}
