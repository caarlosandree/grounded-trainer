//! `gt-patch restore`: só recupera o diário (spec 6.8) e sai.

use std::io::{self, Write};

use super::{begin, print_recovery};

pub fn run() -> anyhow::Result<()> {
    let (_session, report) = begin()?;
    let mut out = io::stdout().lock();
    if report.is_empty() {
        writeln!(out, "nada a restaurar")?;
    } else {
        print_recovery(&mut out, &report)?;
    }
    out.flush()?;
    Ok(())
}
