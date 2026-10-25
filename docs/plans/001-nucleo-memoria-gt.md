# Plano 001 — Núcleo de memória e ferramenta `gt`

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** construir o `gt-core` (processo, `maps`, memória, PE, busca AOB, desmontagem, guarda) e o binário `gt` com `inspect`, `read`, `scan`, `disasm`, `diff`, `dump` e `fling`, todos somente leitura no jogo.

**Arquitetura:** funções puras (parse de `maps`, endereço, padrão, PE, agrupamento de diferenças, guarda) ficam em `gt-core` e são testadas sem o jogo. O conhecimento do Grounded 2 (nome do processo, política de módulos confiáveis, `symbols.ini`) fica em `gt-grounded2`. O `gt-cli` só monta o contexto do alvo, chama a biblioteca e formata texto/JSON. Os testes que precisam de um processo vivo usam um alvo próprio (`gt-testtarget`), nunca o jogo.

**Tecnologias:** Rust 1.99 stable (edition 2024), `nix` (`process_vm_readv`), `goblin` (PE), `iced-x86` (desmontagem), `clap`, `serde`/`serde_json`, `thiserror`/`anyhow`, `tracing`, `libc` (só no apoio a testes).

**Spec:** `docs/specs/001-nucleo-memoria-gt.md`. Quem executa lê a spec e este plano. As seções da spec são citadas como "spec 6.x".

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens ao usuário em pt-BR; `unsafe` só em `crates/gt-core/src/mem/` com `// SAFETY:`; `thiserror` nas libs e `anyhow` nos binários; sem `unwrap()`/`expect()` fora de testes; `tracing`, nunca `println!` em libs.
- O `cargo` está em `~/.cargo/bin`. Em shells sem o PATH configurado, use `export PATH=$HOME/.cargo/bin:$PATH`.
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` e `cargo test --workspace`. A partir da Tarefa 5, também `cargo build -p gt-cli` **isolado** (prova que o `gt` não usa o escritor).
- Nenhum endereço fixo do jogo no código de produção (spec 4). Testes `#[ignore]` podem citar endereços e a chave `c935e799-0b27c000`.
- O `gt` nunca escreve no jogo. A feature `write` do `gt-core` só é ativada em `[dev-dependencies]`.
- Versões novas fixadas no `Cargo.toml` da raiz: `serde_json = "1.0.154"`, `libc = "0.2.191"` (a 1.0 ainda é alpha), `tempfile = "3.27.0"` (só em testes). Features do `nix`: `uio`, `process`.
- Tamanhos e contagens: decimal, ou hex com `0x`. Endereços e offsets: sempre hex, com ou sem `0x` (spec 6.2).
- Saída de comando em stdout via `writeln!` em `std::io::stdout().lock()` (o lint `print_stdout` continua ativo). Logs em stderr.
- Commits ao fim de cada tarefa, em Conventional Commits pt-BR, terminando com a linha `Claude-Session: https://claude.ai/code/session_01QbN63TRmB122uJ9mjGwZur`. **Autorização:** o dono autoriza esses commits ao aprovar este plano (`AGENTS.md` 8); push, merge e rebase continuam exigindo pedido explícito.
- Filtros do `cargo test`: um único nome de teste antes do `--` (ex. `cargo test -p gt-core maps`). Com dois pacotes, rode sem filtro (`cargo test -p gt-core -p gt-grounded2`).
- Mensagens de erro exatas (os testes de CLI procuram estes trechos): `ProcessGone` → "o processo {pid} terminou"; `ModuleNotFound` → "módulo {name} não encontrado no processo"; `NotPe` → "o módulo {module} não é uma imagem PE"; `ThirdPartyCode` → "código fora dos módulos do jogo/Wine (pode ser de outro trainer): bloqueado pela regra do projeto"; `Unmapped` → "endereço {addr:#x} fora de qualquer região mapeada"; jogo ausente (no `gt-cli`) → "jogo não encontrado; abra o Grounded 2 e tente de novo"; limite de tamanho (no `gt-cli`) → "tamanho acima do limite de 1 MiB" (`read`) / "tamanho acima do limite de 1 GiB" (`dump`).

## Foco de revisão

Entradas e falhas que a spec implica, mas que nenhum outro teste exercita, em ordem de risco. Cada uma tem um teste na tarefa dona:

1. **O processo some no meio do comando** (o jogo fecha). Esperado: erro "o processo terminou", código ≠ 0, sem pânico. Teste na Tarefa 10 (`read_after_target_exits_reports_gone`).
2. **Leitura que cruza uma página não mapeada.** Esperado: `??` (no `read`) ou zeros (no `dump`) só na página faltante, com o resto lido. Teste na Tarefa 5 (`lossy_read_over_hole_marks_only_the_hole`).
3. **Endereço ou offset absurdo** (`+FFFFFFFFFFFFFFFF`, `xyz`, vazio). Esperado: erro de parse claro, nunca overflow. Teste na Tarefa 2.
4. **Tamanho ou contagem absurdos, ou intervalo que passa de `u64::MAX`** (`read <end> 0xFFFFFFFF`, `read FFFFFFFFFFFFFFF0 32`, `disasm -n 4294967295`). Esperado: recusa com limite explícito (`read` até 1 MiB, `dump` de intervalo até 1 GiB, `disasm -n` até 10000), sem alocar e sem pânico de overflow. Toda soma `addr + len` no `gt-core` (`guard::check`, `MemReader::read_exact`/`read_lossy`) usa `checked_add` e devolve `Unmapped` ao estourar. Testes nas Tarefas 5 (`read_past_u64_max_is_unmapped`), 7 (`range_past_u64_max_is_rejected`), 10 (`read_rejects_size_over_limit`, `read_rejects_address_overflow`) e 11 (`disasm_rejects_huge_count`).
5. **Padrão AOB só de curingas ou vazio.** Esperado: erro "padrão sem bytes fixos", porque casaria em todo lugar. Teste na Tarefa 3.

---

## Mapa de arquivos

Toda tarefa que cria um módulo também o declara (`pub mod ...` no `lib.rs` do `gt-core`/`gt-grounded2`, `mod ...` em `main.rs`/`cmd/mod.rs` do `gt-cli`), mesmo quando a lista de arquivos da tarefa não repete isso.


```
Cargo.toml                                  deps novas no workspace
crates/gt-core/
  Cargo.toml                                features: write, test-support
  src/lib.rs                                módulos + reexport de Error
  src/error.rs                              enum Error (thiserror)
  src/maps.rs                               parse de /proc/<pid>/maps
  src/addr.rs                               parse de endereço e tamanho
  src/scan.rs                               Pattern, busca, Uniqueness
  src/pe.rs                                 cabeçalho PE em memória, imagem em disco, realocações, versão
  src/mem/mod.rs                            MemReader (process_vm_readv), leitura com buracos
  src/mem/writer.rs                         MemWriter (feature write)
  src/mem/test_support.rs                   mmap/prctl para o alvo de teste (feature test-support)
  src/process.rs                            listar processos, cmdline, ptrace_scope
  src/modules.rs                            Module, TrustPolicy, descoberta de imagens PE
  src/guard.rs                              guarda de código de terceiros
  src/asm.rs                                desmontagem e destino de desvios
  src/diff.rs                               agrupamento de diferenças
  tests/fixtures/game-maps.txt              maps real (usuário trocado por `user`)
  tests/fixtures/cmdline/{game,launcher,steam-exe,proton,reaper}.bin
crates/gt-grounded2/
  src/lib.rs                                constantes do jogo
  src/target.rs                             regra de casamento do processo
  src/trust.rs                              política de módulos confiáveis do Grounded 2
  src/fling.rs                              symbols.ini: parse, localização, critério "alterado"
  tests/fixtures/symbols.ini                cópia do arquivo real
apps/gt-cli/
  Cargo.toml                                [[bin]] gt e gt-testtarget
  src/main.rs                               clap + despacho + saída de erro
  src/context.rs                            resolução do alvo (pid, maps, módulos)
  src/output.rs                             texto/JSON, hexdump, formatação de endereço
  src/cmd/{inspect,read,scan,disasm,diff,dump,fling}.rs
  src/bin/gt-testtarget.rs                  processo-alvo de teste
  tests/support/mod.rs                      inicia/encerra o alvo
  tests/core_live.rs                        integração da API do gt-core contra o alvo
  tests/cli.rs                              integração do binário gt contra o alvo
  tests/game.rs                             #[ignore], exige o jogo aberto
```

**Onde mora o alvo de teste** (decisão do plano, spec 5; efeito colateral: `libc` vira dependência do binário `gt` via `test-support`, aceito): `gt-testtarget` é um segundo binário do pacote `gt-cli`, porque `CARGO_BIN_EXE_<nome>` só existe para binários do próprio pacote. Por isso o `gt-cli` depende do `gt-core` com a feature `test-support` (só `mmap`/`prctl` de apoio, nunca escrita). A feature `write` entra só em `[dev-dependencies]`. Os testes da API que exigem processo vivo (`core_live.rs`) também ficam no `gt-cli`.

---

### Tarefa 1: Workspace, erro comum, `maps` e fixtures

**Arquivos:**
- Modificar: `Cargo.toml` (deps do workspace), `crates/gt-core/Cargo.toml`, `crates/gt-core/src/lib.rs`, `docs/ARCHITECTURE.md` (tabela 4: `serde_json`, `libc`, `tempfile`; árvore da seção 3: `maps.rs`, `addr.rs`, `modules.rs`, `guard.rs`, `diff.rs` e o binário `gt-testtarget`)
- Criar: `clippy.toml` (raiz), `crates/gt-core/src/error.rs`, `crates/gt-core/src/maps.rs`, `crates/gt-core/tests/fixtures/game-maps.txt`, `crates/gt-core/tests/fixtures/cmdline/*.bin`

**Interfaces:**
- Produz:
  - `gt_core::Error` com as variantes (mensagens em pt-BR):
    `ProcessGone { pid: i32 }`, `PermissionDenied { pid: i32, ptrace_scope: Option<u8> }`, `Unmapped { addr: u64, len: usize }`, `ThirdPartyCode { addr: u64 }`, `ModuleNotFound { name: String }`, `NotPe { module: String }`, `BadPe(String)`, `BadPattern { token: String, index: usize }` ("token {index} inválido no padrão: {token}"), `EmptyPattern` ("padrão sem bytes fixos"), `BadAddress { input: String, reason: String }`, `BadMaps { line: String }`, `VersionMismatch { memory: String, disk: String }`, `Io(#[from] std::io::Error)`. Também `pub type Result<T> = std::result::Result<T, Error>`.
  - `gt_core::maps::{MapRegion, Perms, MapPath, parse_maps, read_maps}`:
    - `pub struct Perms { pub read: bool, pub write: bool, pub exec: bool, pub shared: bool }`
    - `pub enum MapPath { Anonymous, Pseudo(String), File { path: PathBuf, deleted: bool } }`. `Pseudo` cobre `[heap]`, `[stack]`, `[vdso]`, `[vsyscall]`, `[vvar]`, `[anon:...]`.
    - `pub struct MapRegion { pub start: u64, pub end: u64, pub perms: Perms, pub offset: u64, pub path: MapPath }` com `pub fn is_anonymous(&self) -> bool` (`Anonymous`, `[heap]`, `[stack]` e qualquer pseudo-caminho que comece com `[anon`, como `[anon:...]` e `[anon_shmem:...]`) e `pub fn file_name(&self) -> Option<&OsStr>`.
    - `pub fn parse_maps(text: &str) -> Result<Vec<MapRegion>>`
    - `pub fn read_maps(pid: i32) -> Result<Vec<MapRegion>>`

- [x] **Passo 1: Adicionar deps e fixtures**

No `[workspace.dependencies]` da raiz: `serde_json = "1.0.154"`, `libc = "0.2.191"`, `tempfile = "3.27.0"`; trocar `nix = "0.31.3"` por `nix = { version = "0.31.3", features = ["uio", "process"] }`. Atualizar a tabela 4 do `ARCHITECTURE.md` com as três crates novas e o motivo de cada uma.

Criar `clippy.toml` na raiz com `allow-unwrap-in-tests = true`, `allow-expect-in-tests = true` e `allow-print-in-tests = true` (os lints do workspace barrariam `unwrap`/`eprintln!` nos testes).

As fixtures capturadas em 2026-10-24 (já com o usuário trocado por `user`) **já estão no repositório**, ainda não commitadas: `crates/gt-core/tests/fixtures/game-maps.txt`, `crates/gt-core/tests/fixtures/cmdline/{game,launcher,steam-exe,proton,reaper}.bin` e `crates/gt-grounded2/tests/fixtures/symbols.ini`. Esta tarefa as inclui no commit. Conferir com `command grep -rac caarlosandree crates/*/tests/fixtures` (esperado: 0 em todas; o `-a` é necessário para os `.bin`). Commits que mudam dependências incluem o `Cargo.lock`.

- [x] **Passo 2: Escrever os testes que falham** (em `maps.rs`, `#[cfg(test)]`)

```rust
const REAL: &str = include_str!("../tests/fixtures/game-maps.txt");

#[test]
fn parses_real_game_maps() {
    let regions = parse_maps(REAL).unwrap();
    assert!(regions.len() > 6000);
    let text = regions.iter().find(|r| r.start == 0x140001000).unwrap();
    assert_eq!(text.end, 0x147a90000);
    assert!(text.perms.exec && !text.perms.write);
    assert!(text.is_anonymous());
    let header = regions.iter().find(|r| r.start == 0x140000000).unwrap();
    assert_eq!(header.offset, 0);
    assert_eq!(header.file_name().unwrap(), "Grounded2Steam-Win64-Shipping.exe");
}

#[test]
fn keeps_spaces_in_paths() {
    let r = parse_maps("7f00-7f01 r--p 00000000 08:02 12 /home/user/.local/share/Steam/steamapps/common/Proton - Experimental/files/lib/wine/x86_64-windows/ntdll.dll\n").unwrap();
    assert!(matches!(&r[0].path, MapPath::File { path, deleted: false } if path.ends_with("x86_64-windows/ntdll.dll") && path.to_string_lossy().contains("Proton - Experimental")));
}

#[test]
fn classifies_deleted_and_pseudo_paths() {
    let r = parse_maps(concat!(
        "1000-2000 rw-s 00000000 00:01 5 /memfd:wine-mapping (deleted)\n",
        "2000-3000 rw-p 00000000 00:00 0 [heap]\n",
        "3000-4000 r-xp 00000000 00:00 0 [vdso]\n",
        "4000-5000 rwxp 00000000 00:00 0 [anon:jit]\n",
        "5000-6000 r-xp 00000000 00:00 0 \n",
    )).unwrap();
    assert!(matches!(&r[0].path, MapPath::File { deleted: true, .. }));
    assert!(r[1].is_anonymous());
    assert!(!r[2].is_anonymous());
    assert!(r[3].is_anonymous() && r[3].perms.exec);
    assert!(matches!(r[4].path, MapPath::Anonymous));
}

#[test]
fn rejects_garbage_line() {
    assert!(matches!(parse_maps("isso não é maps\n"), Err(Error::BadMaps { .. })));
}
```

- [x] **Passo 3: Rodar e ver falhar** — `cargo test -p gt-core maps` → falha de compilação (`parse_maps` não existe).

- [x] **Passo 4: Implementar `error.rs` e `maps.rs`**

O caminho é tudo o que vem depois da 5ª coluna, preservando espaços internos. O sufixo ` (deleted)` vira `deleted: true`. `read_maps` lê `/proc/<pid>/maps` como bytes e converte com `from_utf8_lossy`; `ENOENT`/`ESRCH` → `ProcessGone`; arquivo **vazio** também → `ProcessGone` (é o que um processo zumbi devolve, verificado); `EACCES`/`EPERM` → `PermissionDenied`.

- [x] **Passo 5: Rodar e ver passar** — `cargo test -p gt-core maps` → 4 testes ok; depois a validação global.

- [x] **Passo 6: Commit** — `feat(core): parse de /proc/<pid>/maps e erro comum`

---

### Tarefa 2: Endereços e tamanhos

**Arquivos:** Criar `crates/gt-core/src/addr.rs`

**Interfaces:**
- Produz:
  - `pub enum AddrSpec { Absolute(u64), ModuleOffset { module: Option<String>, offset: u64 } }`. `module: None` é o atalho `+offset`.
  - `pub fn parse_addr(input: &str) -> Result<AddrSpec>`
  - `pub fn parse_size(input: &str) -> Result<u64>` (decimal, ou hex com `0x`)
  - `impl AddrSpec { pub fn resolve(&self, base_of: impl Fn(Option<&str>) -> Result<u64>) -> Result<u64> }`. Soma com `checked_add`; overflow → `BadAddress`.

- [x] **Passo 1: Testes que falham**

```rust
#[test] fn absolute_with_and_without_prefix() {
    assert_eq!(parse_addr("0x1456FC11C").unwrap(), AddrSpec::Absolute(0x1456FC11C));
    assert_eq!(parse_addr("1456fc11c").unwrap(), AddrSpec::Absolute(0x1456FC11C));
}
#[test] fn module_plus_offset() {
    assert_eq!(parse_addr("Grounded2Steam-Win64-Shipping.exe+56FC11C").unwrap(),
        AddrSpec::ModuleOffset { module: Some("Grounded2Steam-Win64-Shipping.exe".into()), offset: 0x56FC11C });
}
#[test] fn shortcut_plus_offset() {
    assert_eq!(parse_addr("+0x56FC11C").unwrap(), AddrSpec::ModuleOffset { module: None, offset: 0x56FC11C });
}
#[test] fn rejects_garbage() {
    for bad in ["", "+", "xyz", "mod+", "+zz", "0x", "1-2"] {
        assert!(matches!(parse_addr(bad), Err(Error::BadAddress { .. })), "{bad}");
    }
}
#[test] fn resolve_overflow_is_error() {
    let spec = parse_addr("+FFFFFFFFFFFFFFFF").unwrap();
    assert!(matches!(spec.resolve(|_| Ok(0x140000000)), Err(Error::BadAddress { .. })));
}
#[test] fn sizes_decimal_or_hex() {
    assert_eq!(parse_size("64").unwrap(), 64);
    assert_eq!(parse_size("0x40").unwrap(), 64);
    assert!(parse_size("40h").is_err());
    assert!(parse_size("-1").is_err());
}
```

- [x] **Passo 2:** `cargo test -p gt-core addr` → falha.
- [x] **Passo 3: Implementar.** O separador `+` é o último `+` da string (nomes de módulo não têm `+`). `resolve` chama `base_of(module)` só para `ModuleOffset`.
- [x] **Passo 4:** `cargo test -p gt-core addr` → ok; validação global.
- [x] **Passo 5: Commit** — `feat(core): parse de endereços e tamanhos`

---

### Tarefa 3: Padrões AOB e busca

**Arquivos:** Criar `crates/gt-core/src/scan.rs`

**Interfaces:**
- Produz:
  - `pub struct Pattern` com `pub fn parse(text: &str) -> Result<Pattern>`, `pub fn len(&self) -> usize` e `pub fn is_empty(&self) -> bool` (sempre `false` após `parse`; existe por causa do `clippy::len_without_is_empty`).
  - `pub fn find_all(haystack: &[u8], pattern: &Pattern) -> Vec<usize>` (offsets, em ordem).
  - `pub fn scan_chunked(total_len: u64, chunk: usize, pattern: &Pattern, read: impl FnMut(u64, &mut [u8]) -> Result<()>) -> Result<Vec<u64>>`. Lê `[0, total_len)` em blocos sobrepostos por `pattern.len() - 1` bytes e devolve offsets sem duplicatas.
  - `pub enum Uniqueness { None, Unique(u64), Many(usize) }` com `pub fn from_hits(hits: &[u64]) -> Uniqueness`.

- [x] **Passo 1: Testes que falham**

```rust
#[test] fn parses_wildcards() {
    let p = Pattern::parse("48 8B ?? ? 05").unwrap();
    assert_eq!(p.len(), 5);
}
#[test] fn bad_token_reports_index() {
    assert!(matches!(Pattern::parse("48 GG 05"), Err(Error::BadPattern { index: 1, .. })));
    assert!(matches!(Pattern::parse("48 8"), Err(Error::BadPattern { index: 1, .. })));
}
#[test] fn rejects_empty_or_only_wildcards() {
    assert!(matches!(Pattern::parse(""), Err(Error::EmptyPattern)));
    assert!(matches!(Pattern::parse("?? ? ??"), Err(Error::EmptyPattern)));
    assert!(Error::EmptyPattern.to_string().contains("padrão sem bytes fixos"));
}
#[test] fn finds_all_with_wildcards() {
    let hay = [0x48, 0x8B, 0x01, 0x05, 0x00, 0x48, 0x8B, 0xFF, 0x05];
    assert_eq!(find_all(&hay, &Pattern::parse("48 8B ?? 05").unwrap()), vec![0, 5]);
}
#[test] fn chunked_finds_match_across_boundary() {
    let mut data = vec![0u8; 100];
    data[62..66].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let hits = scan_chunked(100, 64, &Pattern::parse("DE AD BE EF").unwrap(), |off, buf| {
        buf.copy_from_slice(&data[off as usize..off as usize + buf.len()]); Ok(())
    }).unwrap();
    assert_eq!(hits, vec![62]);
}
#[test] fn uniqueness() {
    assert!(matches!(Uniqueness::from_hits(&[]), Uniqueness::None));
    assert!(matches!(Uniqueness::from_hits(&[7]), Uniqueness::Unique(7)));
    assert!(matches!(Uniqueness::from_hits(&[1, 2]), Uniqueness::Many(2)));
}
```

- [x] **Passo 2:** `cargo test -p gt-core scan` → falha.
- [x] **Passo 3: Implementar.** Busca ingênua com máscara; se ficar lento, ancore no primeiro byte fixo com `iter().position`. Pré-condição documentada de `scan_chunked`: `chunk > pattern.len()`. O `Vec` de ocorrências não tem limite (padrões curtos como `00` podem gerar milhões); a exibição é que é limitada no `gt scan`. Desempenho-alvo: a `.text` (128 MB) em menos de 5 s em release.
- [x] **Passo 4:** `cargo test -p gt-core scan` → ok; validação global.
- [x] **Passo 5: Commit** — `feat(core): padrões AOB com curingas e busca em blocos`

---

### Tarefa 4: PE — cabeçalho em memória, imagem em disco, realocações e versão

**Arquivos:** Criar `crates/gt-core/src/pe.rs`; adicionar `goblin.workspace = true` ao `gt-core`.

**Interfaces:**
- Produz:
  - `pub struct Section { pub name: String, pub rva: u32, pub virtual_size: u32, pub raw_size: u32, pub raw_offset: u32, pub characteristics: u32 }` com `is_executable()` (`IMAGE_SCN_MEM_EXECUTE` `0x2000_0000`; seções lidas com `goblin` `Header::parse` + `coff_header.sections(..)`), `is_readable()`, `is_writable()`, e `perms_string() -> String` (ex. `r-x`).
  - `pub struct PeHeaders { pub image_base: u64, pub size_of_image: u32, pub time_date_stamp: u32, pub size_of_headers: u32, pub sections: Vec<Section> }` com:
    - `pub fn parse(header_bytes: &[u8]) -> Result<PeHeaders>`: usa até `min(header_bytes.len(), 0x1000)` bytes; usa `goblin::pe::header::Header::parse` e lê a tabela de seções logo após o optional header. Não é PE → `BadPe`.
    - `pub fn version_key(&self) -> String`: formato `{:08x}-{:08x}` de `time_date_stamp` e `size_of_image`, ex. `c935e799-0b27c000`.
    - `pub fn section(&self, name: &str) -> Option<&Section>`.
  - `pub struct DiskImage { pub headers: PeHeaders, image: Vec<u8> }`, a imagem do arquivo montada no layout de RVA: os primeiros `size_of_headers` bytes do arquivo vão para o RVA 0 (sem o `MZ` ali o `goblin` falha), cada seção copia `min(raw_size, virtual_size)` bytes de `raw_offset` para `rva`, e o resto fica zerado. Aloca `SizeOfImage` (≈187 MB no jogo):
    - `pub fn load(path: &Path) -> Result<DiskImage>` (lê o arquivo e chama `from_file_bytes`)
    - `pub fn from_file_bytes(file: &[u8]) -> Result<DiskImage>`
    - `pub fn bytes(&self, rva: u32, len: usize) -> Option<&[u8]>` (`None` fora da imagem; soma com `checked_add`)
    - `pub fn relocate(&mut self, actual_base: u64) -> Result<()>`: aplica `IMAGE_REL_BASED_DIR64` (tipo 10) do `.reloc` com delta `actual_base - <base atual da imagem>` (começa em `image_base` e é atualizada a cada chamada; no-op se delta = 0; aritmética com `wrapping_sub`/`wrapping_add`, porque a nova base pode ser menor). Ignora `ABSOLUTE` (tipo 0); outro tipo → `BadPe`. O diretório de relocação (data directory 5) é lido do cabeçalho do próprio arquivo; bloco com `block_size < 8` → `BadPe` (evita laço infinito).
    - `pub fn product_version(&self) -> Option<String>`: `goblin::pe::PE::parse_with_opts` **sobre a imagem em layout de RVA**, com `let mut o = ParseOptions::default(); o.resolve_rva = false; o.parse_imports = false; o.parse_attribute_certificates = false;` (`ParseOptions` é `#[non_exhaustive]`; com `resolve_rva = true` o parse falha em "cannot map tls callback", verificado no exe real) → `resource_data.version_info` → string `ProductVersion`.
  - `pub fn check_same_version(memory: &PeHeaders, disk: &PeHeaders) -> Result<()>`: compara `time_date_stamp` e `size_of_image`; diferente → `VersionMismatch` com as duas chaves.

- [x] **Passo 1: Testes que falham** (bytes sintéticos montados por um helper de teste `fake_pe(sections: &[FakeSection], image_base: u64, time_date_stamp: u32, size_of_image: u32, reloc_dir: Option<(u32, u32)>) -> Vec<u8>` que escreve um **arquivo** PE32+ mínimo: DOS header, `PE\0\0`, COFF, optional header com 16 data directories (o índice 5, base relocation, recebe `reloc_dir` = `(rva, tamanho)`), tabela de seções e o conteúdo bruto de cada seção. `struct FakeSection { name: &'static str, rva: u32, vsize: u32, characteristics: u32, data: Vec<u8> }`; `raw_size = data.len()`. Helpers de teste também a escrever: `sec(name, rva, vsize, characteristics, data_len) -> FakeSection` (dados zerados), `text_with_ptr() -> FakeSection` (`.text` em `0x1000`, `u64` `0x140002000` no offset `0x10`), `reloc_block() -> FakeSection` (`.reloc` em `0x2000` com o bloco descrito no teste) e `u64_at(&DiskImage, rva) -> u64`)

```rust
#[test] fn parses_headers_and_sections() {
    let h = PeHeaders::parse(&fake_pe(&[sec(".text", 0x1000, 0x2000, 0x6000_0020, 0x200), sec(".data", 0x3000, 0x6bbecc, 0xC000_0040, 0x400)], 0x140000000, 0xc935e799, 0x0b27c000, None)).unwrap();
    assert_eq!(h.version_key(), "c935e799-0b27c000");
    assert_eq!(h.sections.len(), 2);
    assert!(h.section(".text").unwrap().is_executable());
    assert_eq!(h.section(".data").unwrap().virtual_size, 0x6bbecc);
    assert_eq!(h.section(".data").unwrap().raw_size, 0x400);
    assert_eq!(h.section(".data").unwrap().perms_string(), "rw-");
}
#[test] fn rejects_non_pe() {
    assert!(matches!(PeHeaders::parse(b"\x7fELF\0\0\0\0"), Err(Error::BadPe(_))));
}
#[test] fn version_mismatch_is_reported() {
    let a = PeHeaders::parse(&fake_pe(&[], 0x140000000, 1, 0x1000, None)).unwrap();
    let b = PeHeaders::parse(&fake_pe(&[], 0x140000000, 2, 0x1000, None)).unwrap();
    assert!(matches!(check_same_version(&a, &b), Err(Error::VersionMismatch { .. })));
}
#[test] fn relocation_dir64_applies_delta() {
    // .text em rva 0x1000 com u64 0x140002000 no offset 0x10; .reloc em rva 0x2000 com 1 bloco:
    // page_rva=0x1000, block_size=12, entradas [(10<<12)|0x10, 0] (a segunda é ABSOLUTE, ignorada)
    let mut img = DiskImage::from_file_bytes(&fake_pe(&[text_with_ptr(), reloc_block()], 0x140000000, 1, 0x3000, Some((0x2000, 12)))).unwrap();
    img.relocate(0x140000000).unwrap();
    assert_eq!(u64_at(&img, 0x1010), 0x140002000);
    img.relocate(0x150000000).unwrap();
    assert_eq!(u64_at(&img, 0x1010), 0x150002000);
}
#[test] fn bytes_outside_image_is_none() { /* from_file_bytes(fake_pe(.., size_of_image 0x3000 ..)).bytes(0x2ff0, 0x20) == None */ }

#[test] #[ignore = "exige o executável do jogo em disco"]
fn real_exe_on_disk() {
    let path = dirs_home().join(".local/share/Steam/steamapps/common/Grounded2/Augusta/Binaries/Win64/Grounded2Steam-Win64-Shipping.exe");
    let img = DiskImage::load(&path).unwrap();
    if img.headers.version_key() != "c935e799-0b27c000" { eprintln!("pulado: build diferente"); return; }
    assert_eq!(img.product_version().as_deref(), Some("Augusta-CL-3207493"));
    assert_eq!(img.headers.sections.len(), 8);
}
#[test] fn product_version_from_fake_is_none() { /* fake_pe sem .rsrc → product_version() == None, sem pânico */ }
```
(`dirs_home()` = `PathBuf::from(std::env::var_os("HOME").unwrap())` no próprio teste.)

- [x] **Passo 2:** `cargo test -p gt-core pe` → falha.
- [x] **Passo 3: Implementar** com as assinaturas acima. `DiskImage::load` lê o arquivo inteiro (≈180 MB) uma vez; alocação aceitável para o `diff`.
- [x] **Passo 4:** `cargo test -p gt-core pe` → ok; `cargo test -p gt-core pe -- --ignored` → `real_exe_on_disk` ok (não precisa do jogo aberto). Se o build mudou, registre a nova chave em `RESEARCH.md` e ajuste o teste.
- [x] **Passo 5: Commit** — `feat(core): cabeçalho PE, imagem em disco, realocações e versão`

---

### Tarefa 5: Leitura e escrita de memória e o processo-alvo de teste

**Arquivos:**
- Criar: `crates/gt-core/src/mem/mod.rs`, `src/mem/writer.rs`, `src/mem/test_support.rs`; `apps/gt-cli/src/bin/gt-testtarget.rs`; `apps/gt-cli/tests/support/mod.rs`; `apps/gt-cli/tests/core_live.rs`
- Modificar: `crates/gt-core/Cargo.toml` (`[features] write = []`, `test-support = ["dep:libc"]`; deps `nix`, `libc` opcional); `apps/gt-cli/Cargo.toml` (`default-run = "gt"` no `[package]`, para o `cargo run -p gt-cli -- inspect` do `CLAUDE.md` continuar funcionando; `[[bin]] name = "gt" path = "src/main.rs"`, `[[bin]] name = "gt-testtarget" path = "src/bin/gt-testtarget.rs"`, dep `gt-core = { workspace = true, features = ["test-support"] }`, dev-dep `gt-core = { workspace = true, features = ["write", "test-support"] }`, `serde_json`, `tempfile`)

**Interfaces:**
- Produz:
  - `pub struct MemReader { pid: i32 }`:
    - `pub fn new(pid: i32) -> MemReader`
    - `pub fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<()>`: `process_vm_readv` em blocos de até 1 MiB. Leitura curta ou `EFAULT` → `Unmapped`; `ESRCH` → `ProcessGone`; `EPERM` → `PermissionDenied { ptrace_scope: crate::process::ptrace_scope() }`.
    - `pub fn read_lossy(&self, addr: u64, len: usize) -> Result<LossyRead>`: tenta o bloco inteiro e, se falhar, página a página (4096, alinhado). Só `EFAULT` ou leitura curta contam como página ilegível (vira zero); `ESRCH` e `EPERM` são propagados como `ProcessGone`/`PermissionDenied`. Nenhuma página legível → `Unmapped`.
  - `pub struct LossyRead { pub bytes: Vec<u8>, pub holes: Vec<std::ops::Range<u64>> }` (intervalos absolutos ilegíveis, já mesclados).
  - `#[cfg(feature = "write")] pub struct MemWriter` (em `mem/writer.rs`, reexportado como `gt_core::mem::MemWriter`) com `pub fn open(pid: i32) -> Result<MemWriter>` (`/proc/<pid>/mem` com `O_WRONLY`, `EACCES` → `PermissionDenied`) e `pub fn write(&self, addr: u64, data: &[u8]) -> Result<()>` (`FileExt::write_all_at`). Doc comment: "escrita crua; a Fase 2 adiciona bytes esperados, pausa e restauração".
  - `#[cfg(feature = "test-support")] pub mod test_support`: `pub fn allow_any_tracer() -> Result<()>` (`prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY)`), `pub fn exec_page(fill: u8) -> Result<u64>` (`mmap` anônimo `PROT_READ|PROT_EXEC` de 4096 bytes, preenchido via `PROT_WRITE` temporário e depois `mprotect`) e `pub fn guarded_pages() -> Result<u64>` (3 páginas anônimas `rw`, com a do meio trocada para `PROT_NONE` via `mprotect`; devolve o início das 3. A página `PROT_NONE` é ilegível para `process_vm_readv` (`EFAULT`) e, ao contrário de um `munmap`, não é reaproveitada por `mmap`s posteriores — verificado em 2026-10-24). Tudo com `// SAFETY:`. As páginas vivem até o processo terminar (sem `munmap` final). Mantenha o `Box` do marcador vivo até o fim do `main`.
  - Alvo `gt-testtarget`: chama `allow_any_tracer()`, monta em tempo de execução um marcador de 32 bytes num `Box<[u8]>` (bytes `0xA5 ^ i` para i em 0..32, para o literal não existir em `.rodata`), cria `exec_page(0xCC)` e `guarded_pages()`, passa o marcador por `std::hint::black_box`, imprime com `writeln!` **uma** linha JSON em stdout (`{"pid":..,"marker":"0x..","marker_len":32,"exec_page":"0x..","region_start":"0x.."}`), dá flush e espera até o stdin fechar.
  - `tests/support/mod.rs`: `pub struct TestTarget { pub child: Child, pub pid: i32, pub marker: u64, pub exec_page: u64, pub region_start: u64 }` (a página ilegível é `region_start + 4096`) com `pub fn spawn() -> TestTarget` (usa `env!("CARGO_BIN_EXE_gt-testtarget")` com `stdin` e `stdout` em `Stdio::piped()`, lê a linha JSON e guarda o `stdin` aberto) e `Drop` que mata e aguarda o filho; `pub fn marker_bytes() -> [u8; 32]`; `pub fn hex_with_spaces(bytes: &[u8]) -> String` (ex. `"A5 A4 ..."`). O nome evita confusão com `context::Target` do binário. O arquivo começa com `#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]`, porque cada teste compila o `support` separado e nem todos usam tudo.

- [x] **Passo 1: Testes que falham** (`apps/gt-cli/tests/core_live.rs`)

```rust
mod support;
use gt_core::mem::{MemReader, MemWriter};

#[test] fn reads_marker() {
    let t = support::TestTarget::spawn();
    let mut buf = [0u8; 32];
    MemReader::new(t.pid).read_exact(t.marker, &mut buf).unwrap();
    assert_eq!(buf, support::marker_bytes());
}
#[test] fn scan_finds_marker_once_in_its_region() {
    let t = support::TestTarget::spawn();
    let maps = gt_core::maps::read_maps(t.pid).unwrap();
    let region = maps.iter().find(|r| r.start <= t.marker && t.marker < r.end).unwrap();
    let pat = gt_core::scan::Pattern::parse(&support::hex_with_spaces(&support::marker_bytes())).unwrap();
    let reader = MemReader::new(t.pid);
    let hits = gt_core::scan::scan_chunked(region.end - region.start, 1 << 20, &pat, |off, buf| reader.read_exact(region.start + off, buf)).unwrap();
    assert_eq!(hits, vec![t.marker - region.start]);
}
#[test] fn raw_writer_changes_marker() {
    let t = support::TestTarget::spawn();
    MemWriter::open(t.pid).unwrap().write(t.marker, &[0x11; 4]).unwrap();
    let mut buf = [0u8; 4];
    MemReader::new(t.pid).read_exact(t.marker, &mut buf).unwrap();
    assert_eq!(buf, [0x11; 4]);
}
#[test] fn lossy_read_over_hole_marks_only_the_hole() {
    let t = support::TestTarget::spawn();
    let r = MemReader::new(t.pid).read_lossy(t.region_start, 3 * 4096).unwrap();
    assert_eq!(r.holes, vec![t.region_start + 4096..t.region_start + 8192]);
    assert_eq!(r.bytes.len(), 3 * 4096);
}
#[test] fn fully_unmapped_is_error() {
    let t = support::TestTarget::spawn();
    assert!(matches!(MemReader::new(t.pid).read_lossy(t.region_start + 4096, 4096), Err(gt_core::Error::Unmapped { .. })));
}
#[test] fn read_past_u64_max_is_unmapped() {
    let t = support::TestTarget::spawn();
    assert!(matches!(MemReader::new(t.pid).read_lossy(u64::MAX - 15, 32), Err(gt_core::Error::Unmapped { .. })));
}
#[test] fn dead_process_is_gone_for_lossy_too() {
    let mut t = support::TestTarget::spawn();
    let pid = t.pid; t.child.kill().unwrap(); t.child.wait().unwrap();
    assert!(matches!(MemReader::new(pid).read_lossy(t.marker, 32), Err(gt_core::Error::ProcessGone { .. })));
}
#[test] fn dead_process_is_gone() {
    let mut t = support::TestTarget::spawn();
    let pid = t.pid; t.child.kill().unwrap(); t.child.wait().unwrap();
    let mut buf = [0u8; 1];
    assert!(matches!(MemReader::new(pid).read_exact(0x1000, &mut buf), Err(gt_core::Error::ProcessGone { .. })));
}
```

- [x] **Passo 2:** `cargo test -p gt-cli --test core_live` → falha de compilação.
- [x] **Passo 3: Implementar** `mem/`, `gt-testtarget` e `tests/support`. Crie `crates/gt-core/src/process.rs` com o stub `pub fn ptrace_scope() -> Option<u8> { None }` (a Tarefa 6 o completa). Deixe `main.rs` do `gt` ainda vazio (`fn main() {}`).
- [x] **Passo 4:** `cargo test -p gt-cli --test core_live` → 6 ok. `cargo build -p gt-cli` isolado → compila. Validação global.
- [x] **Passo 5: Commit** — `feat(core): leitura de memória, escrita crua e processo-alvo de teste`

---

### Tarefa 6: Processos e regra de casamento do jogo

**Arquivos:** Criar `crates/gt-core/src/process.rs` (substitui o stub), `crates/gt-grounded2/src/lib.rs`, `crates/gt-grounded2/src/target.rs`. Dependências do `gt-grounded2` nesta tarefa: `gt-core`, `thiserror`; dev: `tempfile` (usado nas Tarefas 7 e 9).

**Interfaces:**
- Produz:
  - `gt_core::process`: `pub fn parse_cmdline(raw: &[u8]) -> Vec<String>` (separa por NUL, descarta os vazios do fim, `from_utf8_lossy`), `pub fn list_processes() -> Result<Vec<(i32, Vec<String>)>>` (ignora processos que somem durante a varredura) e `pub fn ptrace_scope() -> Option<u8>` (lê `/proc/sys/kernel/yama/ptrace_scope`; `None` se não existir).
  - `gt_grounded2`: `pub const GAME_EXE: &str = "Grounded2Steam-Win64-Shipping.exe"`, `pub const APP_ID: u32 = 2661300`, `pub const FLING_GAME_DIR: &str = "grounded-2-trainer"`.
  - `gt_grounded2::target`:
    - `pub fn is_game(args: &[String]) -> bool`: regra da spec 6.1 aplicada a `args[0]`.
    - `pub fn mentions_game(args: &[String]) -> bool`: algum argumento contém `grounded2`, sem diferenciar maiúsculas.
    - `pub enum Pick { One(i32), None, Many(Vec<i32>) }`
    - `pub fn pick(procs: &[(i32, Vec<String>)]) -> (Pick, Vec<(i32, String)>)`: devolve a escolha (entre os que passam em `is_game`) e os descartados, que são **só** os processos com `mentions_game` e sem `is_game`, cada um com o `args[0]` como motivo.

- [x] **Passo 1: Testes que falham** (`target.rs`, com as fixtures de `crates/gt-core/tests/fixtures/cmdline/` via `include_bytes!("../../gt-core/tests/fixtures/cmdline/game.bin")`)

```rust
#[test] fn real_cmdlines() {
    assert!(is_game(&parse_cmdline(GAME)));
    for other in [LAUNCHER, STEAM_EXE, PROTON, REAPER] {
        let a = parse_cmdline(other);
        assert!(!is_game(&a)); assert!(mentions_game(&a));
    }
}
#[test] fn joined_with_space_and_case() {
    assert!(is_game(&[r"S:\steamapps\common\Grounded2\Augusta\Binaries\Win64\grounded2steam-win64-shipping.EXE Augusta".into()]));
    assert!(is_game(&["Grounded2Steam-Win64-Shipping.exe".into()]));
    assert!(!is_game(&[r"C:\x\NotGrounded2Steam-Win64-Shipping.exe".into()]));
    assert!(!is_game(&[r"C:\x\Grounded2Steam-Win64-Shipping.exe.bak".into()]));
}
#[test] fn pick_one_none_many() {
    let g = |pid| (pid, vec![r"S:\a\Grounded2Steam-Win64-Shipping.exe".to_string()]);
    assert!(matches!(pick(&[g(10)]).0, Pick::One(10)));
    assert!(matches!(pick(&[]).0, Pick::None));
    assert!(matches!(pick(&[g(1), g(2)]).0, Pick::Many(ref v) if v == &vec![1, 2]));
}
```
E em `gt_core::process`: `parse_cmdline(b"a\0b\0\0\0") == ["a", "b"]`.

- [x] **Passo 2:** `cargo test -p gt-core -p gt-grounded2` → falha de compilação.
- [x] **Passo 3: Implementar.**
- [x] **Passo 4:** testes ok; validação global.
- [x] **Passo 5: Commit** — `feat: localizar o processo do jogo pelo cmdline`

---

### Tarefa 7: Módulos, política de confiança e guarda

**Arquivos:** Criar `crates/gt-core/src/modules.rs`, `crates/gt-core/src/guard.rs`, `crates/gt-grounded2/src/trust.rs`

**Interfaces:**
- Produz:
  - `gt_core::modules`:
    - `pub struct TrustPolicy { dirs: Vec<PathBuf> }` com `pub fn new(dirs: Vec<PathBuf>) -> TrustPolicy` e `pub fn is_trusted(&self, path: &Path) -> bool`. A comparação é por componentes (`Path::starts_with` sobre os caminhos normalizados sem tocar o disco), nunca por substring.
    - `pub struct Module { pub name: String, pub path: PathBuf, pub base: u64, pub size: u64, pub trusted: bool }` com `pub fn contains(&self, addr: u64) -> bool` e `pub fn end(&self) -> u64`.
    - `pub fn candidate_bases(maps: &[MapRegion]) -> Vec<(u64, PathBuf)>`: regiões `File` (não `deleted`) com offset 0, legíveis (`r`) e fora de `/dev/`, uma por caminho (a de menor endereço). A fixture tem 249 regiões de `/dev/` com offset 0 que precisam sair aqui.
    - `pub fn discover(maps: &[MapRegion], reader: &MemReader, policy: &TrustPolicy) -> Result<Vec<Module>>`: lê `0x1000` bytes em cada base candidata; só entram os que passam em `PeHeaders::parse`; `size = size_of_image`. `Unmapped` numa candidata a pula; `PermissionDenied` e `ProcessGone` são propagados.
    - `pub fn require_trusted(module: &Module) -> Result<()>`: `ThirdPartyCode { addr: module.base }` se `!module.trusted`. Usada por `scan`, `diff` e `dump` de seção executável.
    - `pub fn find<'a>(modules: &'a [Module], name: &str) -> Option<&'a Module>`: compara o nome do arquivo sem diferenciar maiúsculas.
    - `pub fn describe(modules: &[Module], addr: u64) -> Option<(String, u64)>`: devolve `(nome, offset)` do módulo que contém o endereço.
  - `gt_core::guard`: `pub fn check(maps: &[MapRegion], modules: &[Module], addr: u64, len: u64) -> Result<()>`. Para cada byte do intervalo em região `exec`: se cair num módulo com `trusted == false`, ou se a região for anônima e o byte não cair em nenhum módulo `trusted`, devolve `ThirdPartyCode { addr: <primeiro byte bloqueado> }`. Região não executável, `[vdso]`/`[vsyscall]` e arquivo não PE passam. O cálculo é por intervalos (sem iterar byte a byte). Um intervalo que toca região não mapeada não é problema da guarda (a leitura trata).
  - `gt_grounded2::trust::policy(game_exe: &Path, home: &Path) -> TrustPolicy`: lista da spec 6.1. A biblioteca do jogo é o prefixo de `game_exe` até o componente `steamapps` inclusive, mais `home/.local/share/Steam/steamapps`. Em cada biblioteca, `common/Grounded2`, e cada entrada de `common/` que comece com `Proton` ou `SteamLinuxRuntime`, via `read_dir`, ignorando erros. Também `home/.local/share/Steam/compatibilitytools.d`. No prefixo `<biblioteca do jogo>/compatdata/2661300/pfx/drive_c/`: `windows`, `Program Files (x86)/Steam` e `Program Files/Steam`.

- [x] **Passo 1: Testes que falham** (`guard.rs`, com mapas e módulos sintéticos montados à mão)

```rust
// módulo confiável M: [0x140000000, 0x14b27c000); módulo de terceiros T: [0x180000000, 0x180100000)
#[test] fn anon_exec_outside_modules_blocked()      { /* região 0x7700000-0x7710000 rwxp anônima → Err(ThirdPartyCode{addr:0x7700000}) */ }
#[test] fn anon_text_inside_trusted_module_allowed() { /* 0x140001000-0x147a90000 r-xp anônima, ler 0x1456fc11c..+32 → Ok */ }
#[test] fn third_party_pe_exec_blocked()            { /* região r-xp com arquivo dentro de T → Err */ }
#[test] fn third_party_pe_data_allowed()            { /* região r--p dentro de T → Ok */ }
#[test] fn elf_file_exec_allowed()                  { /* r-xp com arquivo /usr/lib64/libc.so.6 fora de módulos → Ok */ }
#[test] fn anon_data_allowed()                      { /* rw-p anônima → Ok */ }
#[test] fn merged_region_crossing_module_end()      { /* r-xp anônima 0x14b270000-0x14b290000: ler [0x14b270000,+0x1000) Ok; ler [0x14b27b000,+0x2000) → Err(addr: 0x14b27c000) */ }
#[test] fn vdso_allowed()                           { /* [vdso] r-xp → Ok */ }
#[test] fn range_past_u64_max_is_rejected()         { /* check(.., u64::MAX - 15, 32) → Err(Unmapped), sem pânico */ }
```
E em `modules.rs` / `trust.rs`:
```rust
#[test] fn policy_matches_by_components() {
    let p = TrustPolicy::new(vec!["/home/user/.local/share/Steam/steamapps/common/Proton 11.0".into()]);
    assert!(p.is_trusted(Path::new("/home/user/.local/share/Steam/steamapps/common/Proton 11.0/files/lib/wine/x86_64-windows/ntdll.dll")));
    assert!(!p.is_trusted(Path::new("/tmp/steamapps/common/Proton x/evil.dll")));
    assert!(!p.is_trusted(Path::new("/home/user/.local/share/Steam/steamapps/common/Proton 11.0-evil/a.dll")));
}
#[test] fn fling_dll_is_third_party() { /* policy(game_exe, home) com diretórios criados em tempfile; .../drive_c/users/steamuser/AppData/Local/FLiNGTrainer/TrSpeedHack_x64.dll → !trusted; .../drive_c/windows/system32/kernel32.dll → trusted */ }
#[test] fn candidate_bases_from_real_maps() { /* fixture: contém 0x140000000 com nome do exe; nenhum caminho /dev/ */ }
#[test] fn require_trusted_rejects_third_party() { /* Module{trusted:false} → Err(ThirdPartyCode{addr: base}); trusted → Ok */ }
```

- [x] **Passo 2:** `cargo test -p gt-core -p gt-grounded2` → falha de compilação.
- [x] **Passo 3: Implementar.**
- [x] **Passo 4:** testes ok; validação global.
- [x] **Passo 5: Commit** — `feat: módulos, política de confiança e guarda contra código de terceiros`

---

### Tarefa 8: Desmontagem, destino de desvios e agrupamento de diferenças

**Arquivos:** Criar `crates/gt-core/src/asm.rs`, `crates/gt-core/src/diff.rs`; `iced-x86` no `gt-core` (features padrão).

**Interfaces:**
- Produz:
  - `gt_core::asm`:
    - `pub struct Insn { pub addr: u64, pub bytes: Vec<u8>, pub text: String, pub target: Option<u64> }`. `text` em sintaxe Intel (`IntelFormatter` configurado com `hex_prefix = "0x"`, `hex_suffix = ""`, `uppercase_hex = false`, `leading_zeros = false`, `branch_leading_zeros = false`, `space_after_operand_separator = true`, para sair como em `RESEARCH.md`: `call 0x1439e6d10`, `cmp qword ptr [rsi+0xb0], 0`; um operando RIP-relativo sai como `[0x1016]`, já resolvido); `target` é o destino de `call`/`jmp`/`jcc` near (`near_branch_target()`, só quando a instrução é desvio near) ou o endereço efetivo de um operando relativo ao RIP (`ip_rel_memory_address()`, **só** se `is_ip_rel_memory_operand()`; sem esse filtro, `[rcx+0x974]` daria `0x974`).
    - `pub fn disassemble(bytes: &[u8], ip: u64, max: usize) -> Vec<Insn>`: para em `max` instruções ou no fim dos bytes; instrução inválida vira `Insn` com `text = "(inválida)"` e **1 byte**, e o decoder recomeça em `+1` (`Decoder::set_position` + `set_ip`), porque o iced devolve a inválida com tamanho maior.
    - `pub enum Branch { Direct(u64), RipIndirect { slot: u64 } }`
    - `pub fn branch_at(bytes: &[u8], ip: u64) -> Option<Branch>`: só a primeira instrução; `jmp rel32`/`call rel32` → `Direct`, `jmp qword ptr [rip+x]` → `RipIndirect`.
    - `pub fn disassemble_around(bytes: &[u8], base: u64, start: u64, end: u64, context: usize) -> Vec<Insn>`: tenta começar em `start - 64 + k` (limitado ao início de `bytes`), com `k` de 0 a 15, e usa o primeiro `k` cuja decodificação chegue **exatamente** em `start` sem instrução inválida (se nenhum servir, começa em `start`) e devolve até `context` instruções antes, o trecho e `context` depois.
  - `gt_core::diff::group(a: &[u8], b: &[u8], gap: usize) -> Vec<std::ops::Range<usize>>`: trechos onde `a != b`; duas diferenças nos índices `i < j` ficam no mesmo trecho se `j - i < gap` (spec: "a menos de 16 bytes"); `a.len() == b.len()` é pré-condição (assert documentado).

- [x] **Passo 1: Testes que falham**

```rust
// bytes de RESEARCH.md, +57BD6D0 (FLiNG): e9 4b 29 ac 86 0f 1f 00 em ip 0x1457BD6D0
#[test] fn fling_jump_destination() {
    let b = [0xe9, 0x4b, 0x29, 0xac, 0x86, 0x0f, 0x1f, 0x00];
    assert!(matches!(branch_at(&b, 0x1457BD6D0), Some(Branch::Direct(0xCC280020))));
}
#[test] fn rip_indirect_slot() {
    let b = [0xff, 0x25, 0x10, 0x00, 0x00, 0x00]; // jmp [rip+0x10]
    assert!(matches!(branch_at(&b, 0x1000), Some(Branch::RipIndirect { slot: 0x1016 })));
}
#[test] fn call_target_resolved() {
    let b = [0xe8, 0xef, 0xab, 0x2e, 0xfe]; // RESEARCH: +56FC11C → call 0x1439E6D10
    let i = &disassemble(&b, 0x1456FC11C, 1)[0];
    assert_eq!(i.target, Some(0x1439E6D10));
    assert_eq!(i.text, "call 0x1439e6d10");
}
#[test] fn non_rip_memory_has_no_target() {
    let b = [0xf3, 0x0f, 0x10, 0x89, 0x74, 0x09, 0x00, 0x00]; // movss xmm1, [rcx+0x974]
    assert_eq!(disassemble(&b, 0x1457BD6D0, 1)[0].target, None);
}
#[test] fn invalid_byte_does_not_stop() {
    assert_eq!(disassemble(&[0x06, 0x90], 0, 5).len(), 2);
}
#[test] fn groups_with_gap() {
    let a = [0u8; 64]; let mut b = a;
    b[2] = 1; b[10] = 1; b[40] = 1;
    assert_eq!(group(&a, &b, 16), vec![2..11, 40..41]);
    assert!(group(&a, &a, 16).is_empty());
}
#[test] fn around_aligns_to_start() { /* sequência de 10 `nop` de 1 byte + alvo: disassemble_around(..., context=3) começa 3 instruções antes de start */ }
```

- [x] **Passo 2:** `cargo test -p gt-core` → falha de compilação.
- [x] **Passo 3: Implementar.**
- [x] **Passo 4:** testes ok; validação global.
- [x] **Passo 5: Commit** — `feat(core): desmontagem, destino de desvios e agrupamento de diferenças`

---

### Tarefa 9: `symbols.ini` do FLiNG

**Arquivos:** Criar `crates/gt-grounded2/src/fling.rs`

**Interfaces:**
- Produz:
  - `pub struct FlingSymbol { pub name: String, pub module: String, pub offset: u64 }`
  - `pub fn parse_symbols_ini(raw: &[u8]) -> Result<Vec<FlingSymbol>, FlingError>`: exige BOM `FF FE`, decodifica UTF-16LE, aceita CRLF/LF, ignora a linha de cabeçalho `[default]` (os símbolos estão dentro dela), linhas vazias e comentários `;`; o valor é `módulo+offset` (offset em hex, via `gt_core::addr::parse_addr`).
  - `pub fn version_dir(library: &Path) -> PathBuf`: `<library>/compatdata/2661300/pfx/drive_c/users/steamuser/AppData/Local/FLiNGTrainer/trainer-logs/grounded-2-trainer/version`.
  - `pub fn locate_ini(library: &Path, time_date_stamp: u32) -> Result<PathBuf, FlingError>`: `<version_dir>/<tds em decimal>/symbols.ini`; se não existir, `FlingError::NoMatchingVersion { wanted: u32, found: Vec<String> }`.
  - `pub fn ini_version_mismatch(ini: &Path, time_date_stamp: u32) -> bool`: nome da pasta-mãe ≠ tds em decimal (para o aviso do `--ini`).
  - `pub const CHANGE_WINDOW: usize = 16;` e `pub fn is_changed(memory: &[u8], disk: &[u8]) -> bool` (compara os primeiros `CHANGE_WINDOW` bytes).
  - `pub enum FlingError` (thiserror, pt-BR): `NotUtf16`, `BadLine { line: usize, text: String }`, `NoMatchingVersion { .. }`, `Io(std::io::Error)`.

- [x] **Passo 1: Testes que falham**

```rust
const REAL: &[u8] = include_bytes!("../tests/fixtures/symbols.ini");
#[test] fn parses_real_file() {
    let s = parse_symbols_ini(REAL).unwrap();
    assert_eq!(s.len(), 11);
    let god = s.iter().find(|x| x.name == "aobgodmode").unwrap();
    assert_eq!((god.module.as_str(), god.offset), ("Grounded2Steam-Win64-Shipping.exe", 0x56FC11C));
    assert!(s.iter().any(|x| x.name == "aobcharges" && x.offset == 0x57BD6D0));
}
#[test] fn rejects_without_bom() { assert!(matches!(parse_symbols_ini(b"[default]\r\n"), Err(FlingError::NotUtf16))); }
#[test] fn decimal_folder_name() {
    let dir = tempfile::tempdir().unwrap();
    let v = version_dir(dir.path()).join("3375753113"); std::fs::create_dir_all(&v).unwrap();
    std::fs::write(v.join("symbols.ini"), REAL).unwrap();
    assert!(locate_ini(dir.path(), 0xc935e799).is_ok());
    assert!(matches!(locate_ini(dir.path(), 1), Err(FlingError::NoMatchingVersion { ref found, .. }) if found == &vec!["3375753113".to_string()]));
}
#[test] fn change_window() {
    let disk = [0u8; 32]; let mut mem = disk;
    mem[15] = 1; assert!(is_changed(&mem, &disk));
    mem[15] = 0; mem[16] = 1; assert!(!is_changed(&mem, &disk));
}
```
(`tempfile` como dev-dependency do `gt-grounded2`.)

- [x] **Passo 2:** `cargo test -p gt-grounded2 fling` → falha.
- [x] **Passo 3: Implementar.** Registrar o formato em `RESEARCH.md`, se ainda faltar algum detalhe (já há uma entrada de 2026-10-24).
- [x] **Passo 4:** testes ok; validação global.
- [x] **Passo 5: Commit** — `feat(grounded2): leitura do symbols.ini do FLiNG`

---

### Tarefa 10: Esqueleto do `gt`, contexto do alvo, `inspect` e `read`

**Arquivos:** Criar `apps/gt-cli/src/{main.rs,context.rs,output.rs}`, `apps/gt-cli/src/cmd/{mod.rs,inspect.rs,read.rs}`, `apps/gt-cli/tests/cli.rs`. Deps do `gt-cli`: `gt-grounded2`, `clap`, `serde`, `serde_json`, `anyhow`, `tracing`, `tracing-subscriber` (com `env-filter`).

**Interfaces:**
- Consome: Tarefas 1–9.
- Produz:
  - CLI (`clap` derive): opções globais (`global = true`) `--pid <PID>`, `--module <NOME>` (padrão `gt_grounded2::GAME_EXE`) e `--json`; subcomandos `inspect`, `read <ENDERECO> [TAMANHO]`, e (nas próximas tarefas) `scan`, `disasm`, `diff`, `dump`, `fling`.
  - `context.rs`: `pub struct Target { pub pid: i32, pub discarded: Vec<(i32, String)>, pub maps: Vec<MapRegion>, pub reader: MemReader, pub modules: Vec<Module> }` com:
    - `pub fn open(pid: Option<i32>) -> anyhow::Result<Target>`: sem `--pid`, usa `gt_grounded2::target::pick` sobre `list_processes()` **sem o PID do próprio `gt`** (`std::process::id()`) e sem o processo pai (o `cargo run`), cujos `cmdline` podem conter `Grounded2`. A política vem de `gt_grounded2::trust::policy` usando o caminho do exe do jogo encontrado no `maps` (ou uma política vazia, se não houver). `home` vem de `$HOME`.
    - `pub fn module(&self, name: &str) -> anyhow::Result<(&Module, PeHeaders)>`: `ModuleNotFound` se não estiver em `maps`; `NotPe` se estiver mas não for PE.
    - `pub fn resolve(&self, spec: &AddrSpec, default_module: &str) -> anyhow::Result<u64>`.
    - `pub fn label(&self, addr: u64) -> String`: `0x1456fc11c (Grounded2Steam-Win64-Shipping.exe+56fc11c)` ou só o absoluto (uso no texto).
    - `pub fn module_label(&self, addr: u64) -> Option<String>`: só `Grounded2Steam-Win64-Shipping.exe+56fc11c` (offset `{:x}` minúsculo). **Convenção do JSON em todos os comandos:** todo endereço (`address`, `base`, `start`, `end`, `target`, `image_base`) é string `0x…` em hex minúsculo; tamanhos (`size`, `len`, `virtual_size`...) são números; `*_hex` é hex minúsculo sem espaços (`"e94b29"`); intervalos são `{"start": "0x…", "end": "0x…"}`; todo campo `*label` (`label`, `target_label`) vem de `module_label` (ou `null`).
    - `pub fn open_disk(&self, module: &Module, mem: &PeHeaders) -> anyhow::Result<DiskImage>`: abre `/proc/<pid>/root` + `<path>` **sem a barra inicial** (`Path::join` com caminho absoluto descartaria o prefixo); se falhar, tenta `<path>` direto e registra o primeiro erro com `tracing::debug!`; depois `check_same_version`, e `relocate(module.base)` se `base != image_base`. Usada por `inspect` (ProductVersion), `scan --disk`, `diff` e `fling`.
  - `output.rs`: `pub fn emit(json: bool, text: impl FnOnce(&mut dyn Write) -> io::Result<()>, value: &impl Serialize) -> anyhow::Result<()>` e `pub fn hexdump(start: u64, bytes: &[u8], holes: &[Range<u64>]) -> String` (16 bytes por linha, `??` nas lacunas, coluna ASCII com `.` para não imprimíveis).
  - `main`: erro → stderr (com `writeln!` em `std::io::stderr()`, por causa do lint `print_stderr`) com causa e sugestão, saída 1. `tests/cli.rs` começa com `#![allow(clippy::unwrap_used, clippy::expect_used)]` (o `clippy.toml` só cobre funções `#[test]`). Mapeamentos obrigatórios (spec 6.10): `PermissionDenied` → explica `ptrace_scope` com o valor atual, sem sugerir `sudo` primeiro; `ProcessGone` → "o processo terminou"; `Unmapped` → "endereço fora de qualquer região mapeada"; `ThirdPartyCode` → mensagem exata da spec 6.12.
  - `inspect`: campos da spec 6.3. JSON: `{pid, discarded:[{pid,reason}], module:{name,path,base,image_base,size_of_image}, version_key, product_version, sections:[{name,address,virtual_size,raw_size,perms}], ptrace_scope, memory_readable}`. O `product_version` vem de `ctx.open_disk(..)?.product_version()`; se qualquer passo falhar, `null`/"indisponível" (o `inspect` não falha por isso).
  - `read`: limite de 1 MiB; `guard::check` antes de ler; `read_lossy`. JSON: `{address, label, size, bytes_hex, holes:[{start,end}]}`.

- [x] **Passo 1: Testes que falham** (`tests/cli.rs`, rodando `env!("CARGO_BIN_EXE_gt")` com `--pid <alvo> --module gt-testtarget`)

```rust
#[test] fn read_shows_marker()                    { /* gt --json read <marker> 32 → bytes_hex == marker */ }
#[test] fn read_text_is_hexdump()                 { /* sem --json: primeira linha começa com o endereço em hex e tem 16 bytes */ }
#[test] fn inspect_on_elf_says_not_pe()           { /* saída != 0, stderr contém "não é uma imagem PE" */ }
#[test] fn unknown_module_with_shortcut_errors()  { /* --module nada.exe read +10 → stderr "não encontrado" */ }
#[test] fn absolute_read_ignores_missing_module() { /* --module nada.exe read <marker> 4 → ok */ }
#[test] fn read_on_exec_page_is_blocked()         { /* read <exec_page> 16 → != 0, stderr contém "bloqueado pela regra do projeto" */ }
#[test] fn read_hole_shows_question_marks()       { /* read <region_start> 12288 → "??" presente, saída 0 */ }
#[test] fn read_rejects_size_over_limit()         { /* read <marker> 0xFFFFFFFF → != 0, stderr menciona "1 MiB" */ }
#[test] fn read_rejects_address_overflow()        { /* read FFFFFFFFFFFFFFF0 32 → != 0, stderr sem "panicked" */ }
#[test] fn read_after_target_exits_reports_gone()  { /* child.kill() + child.wait(); gt --pid <pid> read <marker> → != 0, stderr contém "terminou" */ }
#[test] fn no_game_running_message()              { /* sem --pid e sem o jogo aberto: stderr contém "abra o Grounded 2"; pule com aviso se o jogo estiver aberto */ }
```
E unitários das mensagens (funções puras de formatação em `main.rs`/`output.rs`): `permission_hint(Some(1))` menciona `ptrace_scope` e o valor `1` e **não** começa sugerindo `sudo`; `many_candidates_message(&[10, 20])` lista `10` e `20` e menciona `--pid`.

E unitários em `output.rs`: `hexdump(0x1000, &[0x41; 20], &[])` tem 2 linhas, a primeira `0x0000000000001000  41 41 ... |AAAAAAAAAAAAAAAA|`, e as lacunas aparecem como `??`.

- [x] **Passo 2:** `cargo test -p gt-cli --test cli` → falha.
- [x] **Passo 3: Implementar.** Logs: `tracing_subscriber` em stderr com `EnvFilter` de `RUST_LOG` (padrão `warn`).
- [x] **Passo 4:** `cargo test -p gt-cli` (inclui os unitários do binário) ok; `cargo build -p gt-cli` isolado; validação global.
- [x] **Passo 5: Commit** — `feat(cli): gt inspect e gt read`

---

### Tarefa 11: `scan`, `disasm` e `dump`

**Arquivos:** Criar `apps/gt-cli/src/cmd/{scan.rs,disasm.rs,dump.rs}`; ampliar `tests/cli.rs`.

**Interfaces:**
- Consome: `Target`, `emit`, `Pattern`, `scan_chunked`, `Uniqueness`, `disassemble`, `guard::check`, `DiskImage`, `check_same_version`.
- Produz:
  - `scan "<PADRAO>" [--section NOME | --all] [--disk] [--limit N=50]`: área padrão = seções executáveis do módulo; `require_trusted` antes. Na memória, lê seção a seção (`base + rva`, `virtual_size`) com `scan_chunked` (blocos de 1 MiB); um bloco ilegível é lido com `read_lossy` (zeros nas lacunas) e a saída avisa quantos bytes ficaram ilegíveis. Com `--disk`, lê de `DiskImage` (após `check_same_version`). Texto: até `limit` linhas com `ctx.label`, depois o total e a palavra "único", "nenhum" ou "N ocorrências", e "(origem: memória|disco)". JSON: `{source, total, uniqueness, hits:[{address,label}]}`.
  - `disasm <ENDERECO> [-n N=20]`: `N` até 10000 (acima → erro "contagem acima do limite de 10000"); lê `N*15` bytes (com `checked_mul`) com `read_lossy` (após `guard::check` do intervalo) e desmonta; cada linha `label  bytes  texto`, mais `→ label(target)` quando `target` cair num módulo conhecido. JSON: `{instructions:[{address,label,bytes_hex,text,target,target_label}]}`.
  - `dump (<SECAO> | <ENDERECO> <TAMANHO>) [-o ARQUIVO] [--force]`: com dois argumentos posicionais é intervalo; com um, é nome de seção. Os diretórios de destino são criados (`create_dir_all`). Seção → `[base+rva, +virtual_size)`; intervalo limitado a 1 GiB. `guard::check` antes; lê com `read_lossy`. Destino padrão `scratch/dumps/<version_key | pid<PID>>/<nome>.bin` (relativo ao diretório atual; `<nome>` = seção sem ponto, ou o endereço inicial como `{:x}`, minúsculo, sem `0x` e sem zeros à esquerda), com o `.json` ao lado (`{base, start, size, version_key, holes}`). Com `-o X.bin`, o `.json` é `X.json` (troca a extensão). Arquivo existente (`.bin` ou `.json`) sem `--force` → erro. A seção executável de módulo não confiável é recusada por `require_trusted`.

- [x] **Passo 1: Testes que falham** (`tests/cli.rs`)

```rust
#[test] fn disasm_on_exec_page_is_blocked() { /* != 0, "bloqueado pela regra do projeto" */ }
#[test] fn dump_on_exec_page_is_blocked()   { /* dump <exec_page> 4096 -o <tmp>/x.bin → != 0, arquivo não criado */ }
#[test] fn dump_range_writes_bin_and_json() { /* dump <marker> 32 -o <tmp>/m.bin → m.bin == marker, m.json tem "size": 32 */ }
#[test] fn dump_refuses_overwrite()         { /* segundo dump igual → != 0; com --force → 0 */ }
#[test] fn dump_default_path_uses_pid_dir() { /* current_dir = tempdir; dump <marker> 32 → scratch/dumps/pid<PID>/<hex>.bin existe */ }
#[test] fn scan_on_elf_says_not_pe()        { /* != 0, "não é uma imagem PE" */ }
#[test] fn disasm_on_data_is_allowed()      { /* disasm <marker> -n 3 → saída 0, 3 linhas */ }
#[test] fn disasm_rejects_huge_count()      { /* disasm <marker> -n 4294967295 → != 0, stderr menciona "10000" */ }
#[test] fn dump_hole_writes_zeros_and_lists_hole() { /* dump <region_start> 12288 -o <tmp>/h.bin → bytes 4096..8192 zerados; h.json "holes" == [{"start": "0x<region_start+4096>", "end": "0x<region_start+8192>"}] */ }
```

- [x] **Passo 2:** `cargo test -p gt-cli --test cli` → os novos falham.
- [x] **Passo 3: Implementar.**
- [x] **Passo 4:** testes ok; validação global.
- [x] **Passo 5: Commit** — `feat(cli): gt scan, gt disasm e gt dump`

---

### Tarefa 12: `diff` e `fling`

**Arquivos:** Criar `apps/gt-cli/src/cmd/{diff.rs,fling.rs}`, com um módulo comum `apps/gt-cli/src/cmd/compare.rs`.

**Interfaces:**
- Consome: `Target::open_disk` (Tarefa 10), `diff::group`, `asm::{branch_at, disassemble_around}`, `guard::check`, `fling::*`.
- Produz:
  - `compare.rs`:
    - `pub struct Chunk { pub addr: u64, pub original: Vec<u8>, pub current: Vec<u8>, pub unreadable: bool, pub branch: Option<Branch> }` e a função pura `pub fn build_chunks(mem: &[u8], holes: &[Range<u64>], disk: &[u8], start: u64) -> Vec<Chunk>`: agrupa com `diff::group(.., 16)`, toda lacuna ilegível vira trecho próprio com `unreadable = true` (mesmo que o disco também seja zero ali), e um trecho de diferença que cruza lacuna também é marcado e preenche `branch` com `branch_at` no início do trecho atual. Toda a lógica do `diff` sem processo vivo fica aqui.
    - `pub enum Destination { Trusted(String), ThirdPartyModule(String), Outside, Unresolved }`, a função pura `pub fn destination_for(modules: &[Module], target: u64) -> Destination` e `pub fn classify_branch(ctx: &Target, branch: Branch) -> (Option<u64>, Destination)`: `RipIndirect` lê o slot (8 bytes) só se o slot estiver num módulo `trusted`; senão `Unresolved`. Nunca lê bytes no destino.
  - `diff [--section NOME]`: padrão = seções executáveis do módulo (`--module`); `require_trusted` antes. Fora das seções executáveis, imprime primeiro o aviso da spec 6.7. Lê a seção da memória (`read_lossy`; lacunas entram como diferença marcada "ilegível") e compara com `DiskImage::bytes`, usando `group(.., 16)`. Para cada trecho: `label`, tamanho, bytes originais × atuais, `disassemble_around` dos dois lados (`context = 3`) e, se `branch_at` no início do trecho atual, o destino classificado. No fim, o total, ou "nenhuma alteração". JSON: `{section, chunks:[{address,label,len,original_hex,current_hex,original_asm:[..],current_asm:[..],branch:{target,destination}}], total}`.
  - `fling [--steam-library DIR] [--ini ARQUIVO]`: o padrão da biblioteca é `$HOME/.local/share/Steam/steamapps`. `locate_ini` pelo `time_date_stamp` da memória, ou `--ini` com aviso se `ini_version_mismatch`. Para cada símbolo: resolve `módulo+offset` pelo nome de módulo que vem do próprio `.ini` (não pelo `--module`), compara `CHANGE_WINDOW` bytes memória × disco (`is_changed`), desmonta 3 instruções do código atual (se o endereço passar no `guard::check`) e, se alterado, mostra o destino via `classify_branch`. O texto termina com a nota: "original = sem alteração nos 16 bytes a partir do endereço". JSON: `{ini, symbols:[{name,address,label,state,asm:[..],branch}]}`.

- [x] **Passo 1: Testes que falham.** De CLI, sem o jogo:

```rust
#[test] fn diff_on_elf_says_not_pe()  { /* != 0, "não é uma imagem PE" */ }
#[test] fn fling_on_elf_says_not_pe() { /* != 0, "não é uma imagem PE" */ }
```
Unitários das funções puras de `compare.rs`:
```rust
#[test] fn fling_charges_patch_is_one_chunk_outside() {
    // RESEARCH.md, +57BD6D0: disco f3 0f 10 89 74 09 00 00 × memória e9 4b 29 ac 86 0f 1f 00
    let (disk, mem) = (hex("f3 0f 10 89 74 09 00 00"), hex("e9 4b 29 ac 86 0f 1f 00"));
    let chunks = build_chunks(&mem, &[], &disk, 0x1457BD6D0);
    assert_eq!(chunks.len(), 1);
    // 7, não 8: o último byte do `nop` do FLiNG (00) coincide com o original
    assert_eq!((chunks[0].addr, chunks[0].current.len()), (0x1457BD6D0, 7));
    assert!(matches!(chunks[0].branch, Some(Branch::Direct(0xCC280020))));
    assert!(matches!(destination_for(&[game_module()], 0xCC280020), Destination::Outside));
}
#[test] fn two_far_patches_are_two_chunks() { /* diferenças a 0x40 bytes uma da outra → 2 trechos */ }
#[test] fn identical_is_empty() { /* build_chunks(a, [], a, ..) → vazio */ }
#[test] fn hole_marks_unreadable() { /* lacuna sobre o trecho → unreadable = true */ }
#[test] fn destination_kinds() { /* endereço no módulo confiável → Trusted; em módulo T → ThirdPartyModule */ }
```
(`Module` deriva `Default`; `game_module()` = `Module { name: "Grounded2Steam-Win64-Shipping.exe".into(), base: 0x140000000, size: 0xb27c000, trusted: true, ..Default::default() }`; `hex()` helper local.)

- [x] **Passo 2:** os testes falham.
- [x] **Passo 3: Implementar.**
- [x] **Passo 4:** testes ok; validação global.
- [x] **Passo 5: Commit** — `feat(cli): gt diff e gt fling`

---

### Tarefa 13: Testes com o jogo, critério de pronto e registro

**Arquivos:** Criar `apps/gt-cli/tests/game.rs`; modificar `docs/RESEARCH.md`, `docs/ROADMAP.md` (marcar a Fase 1) e `docs/plans/001-nucleo-memoria-gt.md` (checkboxes).

- [x] **Passo 1: Escrever os testes `#[ignore = "exige o jogo aberto"]`** (arquivo começa com `#![allow(clippy::unwrap_used, clippy::expect_used)]`)

```rust
#[test] #[ignore = "exige o jogo aberto"] fn inspect_finds_game()   { /* gt --json inspect → module.base presente, sections ≥ 6, version_key casa ^[0-9a-f]{8}-[0-9a-f]{8}$ */ }
#[test] #[ignore = "exige o jogo aberto"] fn known_build_bytes()     { /* se version_key == "c935e799-0b27c000": gt --json disasm +56FC11C -n 1 → text começa com "call", target_label == "Grounded2Steam-Win64-Shipping.exe+39e6d10"; senão: eprintln!("pulado: build diferente") e return */ }
#[test] #[ignore = "exige o jogo aberto"] fn diff_runs_on_text()     { /* gt --json diff → saída 0 */ }
#[test] #[ignore = "exige o jogo aberto"] fn fling_lists_symbols()   { /* gt --json fling → 11 símbolos (build c935e799) */ }
```

- [x] **Passo 2: Pedir ao dono:** "abra o Grounded 2 num save de teste (faça backup do save antes) e me avise". Rodar `cargo test --workspace -- --ignored` → todos ok.

- [x] **Passo 3: Critério de pronto (spec 8).** Pedir ao dono: "abra o FLiNG e ligue **usos/cargas infinitas** e **quantidade ao arrastar**". Rodar e guardar as saídas em texto:
  1. `cargo run --release -p gt-cli --bin gt -- inspect` → base, seções, chave de versão.
  2. `cargo run --release -p gt-cli --bin gt -- disasm Grounded2Steam-Win64-Shipping.exe+56FC11C` → bate com `RESEARCH.md` (`call 0x1439e6d10`, `cmp qword ptr [rsi+0xb0], 0`...).
  3. `cargo run --release -p gt-cli --bin gt -- diff` → 2 trechos, em `+57BD6D0` e `+5BBFE8C`, com o destino "fora dos módulos".
  4. `cargo run --release -p gt-cli --bin gt -- fling` → `aobcharges` e `aobdragitemamount` como "alterado".

  Se o build mudou, os offsets serão outros: o critério é achar os mesmos dois patches onde estiverem.

- [x] **Passo 4: Mostrar ao dono** as saídas lado a lado com `RESEARCH.md` e pedir a confirmação.

- [x] **Passo 5: Registrar** em `RESEARCH.md` (data, build, o que o `gt` confirmou, hipóteses descartadas da sessão) e marcar a Fase 1 como concluída no `ROADMAP.md`.

- [x] **Passo 6: Commit** — `test: testes com o jogo e fechamento da fase 1` (inclui o próprio plano com as caixas marcadas).
