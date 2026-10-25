//! Abertura do motor de patches para o jogo (feature `write`).

use gt_core::engine::{Engine, EngineOptions, PolicyFn};
use gt_core::journal::{JournalLock, JournalStore};
use gt_core::modules::Module;
use gt_core::pe::PeHeaders;
use gt_core::suspend::PauseConfig;

use crate::Result;
use crate::policy::write_policy;
use crate::resolve::Target;

/// Abre o motor de patches no processo `target`.
///
/// A política de escrita é recalculada a cada lote com o `maps` novo ([`write_policy`]), a
/// partir do `module` e do cabeçalho do disco `disk` (calculados uma vez, spec 6.6). Quem chama
/// guarda o `lock` do diário até depois do `drop` do [`Engine`].
pub fn open_engine(
    target: &Target,
    store: JournalStore,
    lock: &JournalLock,
    module: &Module,
    disk: PeHeaders,
) -> Result<Engine> {
    let version_key = disk.version_key();
    let module = module.clone();
    let policy: PolicyFn = Box::new(move |maps| write_policy(&module, &disk, maps));
    let options = EngineOptions {
        version_key,
        pause: PauseConfig::default(),
    };
    Ok(Engine::open(target.pid, store, lock, policy, options)?)
}
