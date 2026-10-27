//! Montagem comum dos testes que usam o módulo sintético com o motor de patches: o alvo, o
//! `Target` do `gt-grounded2` (com o módulo confiável), o "disco", o diário num diretório
//! temporário e a trava dele.

use std::ops::Range;

use std::collections::HashMap;

use gt_core::cave::{CaveAllocator, cc_runs, hook_bytes};
use gt_core::engine::Engine;
use gt_core::journal::{JournalLock, JournalStore};
use gt_core::mem::MemReader;
use gt_core::modules::Module;
use gt_core::patch::{CAVE_FILL, PatchKind, PatchSpec, validate};
use gt_core::pe::DiskImage;
use gt_grounded2::caves::{self, CaveCtx, Register};
use gt_grounded2::resolve::Target;
use gt_ue::UeLayout;

use crate::{TargetOpts, TestTarget};

/// Nome do módulo sintético.
pub use crate::module::MODULE_NAME;

/// O alvo com o módulo sintético, pronto para abrir o motor.
pub struct SynthRig {
    pub t: TestTarget,
    /// `Target` como o `gt-patch` o abriria para o jogo.
    pub target: Target,
    /// O módulo sintético (confiável).
    pub module: Module,
    /// O arquivo do módulo, realocado para a base atual.
    pub disk: DiskImage,
    /// Diário e trava, num diretório temporário.
    pub store: JournalStore,
    pub lock: JournalLock,
    _state: tempfile::TempDir,
}

impl SynthRig {
    /// Inicia o alvo com o módulo e monta tudo.
    pub fn new() -> SynthRig {
        let t = TestTarget::spawn_with(TargetOpts {
            module: true,
            ..TargetOpts::default()
        });
        let target = t.module_target();
        let (module, mem_headers) = target.module(MODULE_NAME).unwrap();
        let module = module.clone();
        let disk = target.open_disk(&module, &mem_headers).unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = JournalStore::new(state.path().join("state"));
        let lock = store.lock().unwrap();
        SynthRig {
            t,
            target,
            module,
            disk,
            store,
            lock,
            _state: state,
        }
    }

    /// O motor aberto como o `gt-patch` o abre (`gt_grounded2::patching::open_engine`). Quem chama
    /// o larga antes de `self`.
    pub fn engine(&self) -> Engine {
        gt_grounded2::patching::open_engine(
            &self.target,
            self.store.clone(),
            &self.lock,
            &self.module,
            self.disk.headers.clone(),
        )
        .unwrap()
    }

    /// Folga da `.data` para as variáveis (`gt_grounded2::policy::var_region`).
    pub fn var_region(&self) -> Range<u64> {
        gt_grounded2::policy::var_region(&self.module, &self.disk.headers, &self.target.maps)
            .expect("folga da .data do módulo sintético")
    }
}

/// Um desvio e a cave dele, instalados por [`SynthRig::install_observe`].
#[derive(Debug, Clone)]
pub struct InstalledHook {
    pub hook_id: String,
    pub cave_id: String,
    /// Endereço e tamanho da instrução trocada.
    pub hook_at: u64,
    pub hook_len: usize,
    /// Bytes originais da instrução.
    pub original: Vec<u8>,
    pub cave_at: u64,
    pub cave: Vec<u8>,
}

impl SynthRig {
    /// Alocador de caves sobre as sequências de `CC` (64 bytes ou mais) da `.text` do "disco".
    pub fn cave_allocator(&self) -> CaveAllocator {
        let text = self.disk.headers.section(".text").unwrap();
        let bytes = self
            .disk
            .bytes(text.rva, text.virtual_size as usize)
            .unwrap();
        CaveAllocator::new(cc_runs(bytes, self.module.base + u64::from(text.rva), 64))
    }

    /// Instala a cave de observação de `reg` num desvio no começo da função `function` (símbolo do
    /// módulo, primeira instrução de 5 bytes): reclama a folga, escreve a cave e o desvio em
    /// lotes separados, com os ids `<id>-cave` e `<id>-hook`.
    pub fn install_observe(
        &self,
        engine: &mut Engine,
        alloc: &mut CaveAllocator,
        id: &str,
        function: &str,
        reg: Register,
    ) -> InstalledHook {
        let region = self.var_region();
        engine.claim_vars(region.clone()).unwrap();
        let hook_at = self.t.symbol(function);
        let mut original = vec![0u8; 5];
        MemReader::new(self.t.pid)
            .read_exact(hook_at, &mut original)
            .unwrap();
        let ctx = |cave_at| CaveCtx {
            hook_at,
            cave_at,
            insn: original.clone(),
            vars: region.start,
            captures: Vec::new(),
            fields: HashMap::new(),
            layout: UeLayout::observed(),
        };
        let probe = caves::build_observe(&ctx(hook_at), reg).unwrap();
        let cave_at = alloc.alloc(probe.len()).expect("sequência de CC livre");
        let cave = caves::build_observe(&ctx(cave_at), reg).unwrap();
        let (cave_id, hook_id) = (format!("{id}-cave"), format!("{id}-hook"));
        engine
            .apply(vec![
                validate(PatchSpec {
                    id: cave_id.clone(),
                    addr: cave_at,
                    expected: vec![CAVE_FILL; cave.len()],
                    replacement: cave.clone(),
                    kind: PatchKind::Cave,
                })
                .unwrap(),
            ])
            .unwrap();
        engine
            .apply(vec![
                validate(PatchSpec {
                    id: hook_id.clone(),
                    addr: hook_at,
                    expected: original.clone(),
                    replacement: hook_bytes(hook_at, original.len(), cave_at).unwrap(),
                    kind: PatchKind::Code,
                })
                .unwrap(),
            ])
            .unwrap();
        InstalledHook {
            hook_id,
            cave_id,
            hook_at,
            hook_len: original.len(),
            original,
            cave_at,
            cave,
        }
    }
}

impl Default for SynthRig {
    fn default() -> Self {
        Self::new()
    }
}
