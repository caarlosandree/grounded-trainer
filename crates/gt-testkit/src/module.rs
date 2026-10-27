//! Módulo sintético do `gt-testtarget --module`: uma imagem PE mínima com a mesma forma do jogo.
//!
//! - o arquivo `gt-synthetic.exe` (cabeçalho PE, `.text` e `.data`) é o "disco" do `gt diff`;
//! - na memória, a cabeça vem do arquivo (`r--` no cabeçalho, `r-x` na `.text`, `rw-` no começo da
//!   `.data`) e a cauda da `.data` é uma VMA **anônima** `rw-p` até o fim da página, com o
//!   `VirtualSize` terminando no meio dela (folga de [`SLACK_LEN`] bytes, como no jogo);
//! - a `.text` traz duas sequências de `CC` (300 e 70 bytes) e funções-alvo montadas com
//!   `iced-x86`, que as caves das próximas tarefas desviam. Todas seguem a convenção x64 do
//!   Windows (`rcx`, `rdx`, `r8`, `r9`; resultado em `rax`), como o jogo, e usam só endereçamento
//!   relativo ao `rip`: a imagem não precisa de realocação.
//!
//! O que é puro ([`build`]) não precisa de processo; [`install`] grava e mapeia o módulo no
//! processo atual (é o que o `gt-testtarget` faz ao iniciar).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use gt_core::mem::test_support::{ImageSegment, PROT_R, PROT_RW, PROT_RX, map_image, write_local};
use iced_x86::code_asm::{
    CodeAssembler, dword_ptr, eax, edx, esi, ptr, qword_ptr, r11, r14, rax, rbp, rbx, rcx, rdx,
    rsi, rsp, xmm1,
};
use iced_x86::{Code, Instruction, MemoryOperand, Register};

/// Nome do arquivo (e do módulo).
pub const MODULE_NAME: &str = "gt-synthetic.exe";

/// Base preferida da imagem (a do jogo); se estiver ocupada, o kernel escolhe outra.
pub const PREFERRED_BASE: u64 = 0x1_4000_0000;
/// Tamanho da página (e alinhamento das seções e do arquivo).
pub const PAGE: u32 = 0x1000;
/// RVA da `.text`.
pub const TEXT_RVA: u32 = 0x1000;
/// RVA da `.data`.
pub const DATA_RVA: u32 = 0x2000;
/// Parte da `.data` mapeada do arquivo (2 páginas).
pub const DATA_HEAD_LEN: u32 = 0x2000;
/// Folga entre o fim do `VirtualSize` da `.data` e o fim da página: a do jogo (`0x130`).
pub const SLACK_LEN: u32 = 0x130;
/// `VirtualSize` da `.data`: a parte do arquivo, uma página anônima inteira menos a folga.
pub const DATA_VSIZE: u32 = DATA_HEAD_LEN + PAGE - SLACK_LEN;
/// `SizeOfImage`.
pub const IMAGE_SIZE: u32 = DATA_RVA + DATA_HEAD_LEN + PAGE;
/// Marca de data do cabeçalho (fixa, para o arquivo ser reproduzível).
const TIME_DATE_STAMP: u32 = 0x5A5A_5A5A;

/// Tamanho das duas sequências de `CC` da `.text`.
pub const CC_RUN_LONG: usize = 300;
/// Idem (a curta).
pub const CC_RUN_SHORT: usize = 70;

// Offsets dos objetos sintéticos (valores só deste módulo; não são os do jogo).
/// `ResearchPoints` do objeto de pesquisa (`rcx` de `level_gain`).
pub const RESEARCH_POINTS: i32 = 0x514;
/// Contador de níveis do objeto de pesquisa (`inc` quando um nível é cruzado).
pub const RESEARCH_LEVEL: i32 = 0x518;
/// Início da tabela de níveis no objeto da tabela (ponteiro).
pub const TABLE_START: i32 = 0x118;
/// Contagem de entradas (`i32`) no objeto da tabela.
pub const TABLE_COUNT: i32 = 0x120;
/// Bytes por entrada da tabela.
pub const TABLE_STRIDE: i32 = 0x30;
/// Limite (`i32`) dentro da entrada.
pub const TABLE_LIMIT: i32 = 0x10;
/// Cargas (`float`) do analisador (`rcx` de `getter`).
pub const ANALYZER_CHARGES: i32 = 0x974;
/// `StackSize` (`i32`) do item.
pub const ITEM_STACK: i32 = 0x2B8;
/// Total (`i32`) do inventário e contagem de itens somados.
pub const INVENTORY_TOTAL: i32 = 0x40;
/// Contagem (`i32`) de chamadas de `add_item` no inventário.
pub const INVENTORY_COUNT: i32 = 0x44;

// Posições dos objetos dentro da `.data` (deslocamentos a partir do RVA da `.data`).
const OBJ_RESEARCH: u32 = 0x0000;
const OBJ_TABLE: u32 = 0x0600;
const OBJ_ENTRIES: u32 = 0x0800;
const OBJ_INVENTORY_A: u32 = 0x0C00;
const OBJ_INVENTORY_B: u32 = 0x0D00;
const OBJ_ANALYZER: u32 = 0x1000;
const OBJ_ITEM_A: u32 = 0x1A00;
const OBJ_ITEM_B: u32 = 0x1D00;
/// Limites iniciais da tabela de níveis (em ordem; os testes podem reescrevê-la).
pub const LEVEL_LIMITS: [i32; 6] = [100, 300, 700, 1500, 3000, 6000];

/// Símbolos como `(nome, RVA)`.
type Symbols = Vec<(&'static str, u32)>;

/// Cabeçalho de uma seção do arquivo.
struct SectionHeader {
    name: &'static [u8],
    virtual_size: u32,
    rva: u32,
    raw_size: u32,
    raw_ptr: u32,
    characteristics: u32,
}

/// Imagem montada: o arquivo e os símbolos (RVAs).
pub struct Image {
    /// Conteúdo do arquivo `gt-synthetic.exe`.
    pub file: Vec<u8>,
    /// `(nome, RVA)` de cada símbolo, em ordem estável.
    pub symbols: Symbols,
    /// Bytes da `.text` (do começo da seção, sem o preenchimento até a página).
    pub text: Vec<u8>,
}

impl Image {
    /// RVA de um símbolo.
    pub fn rva(&self, name: &str) -> Option<u32> {
        self.symbols
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, r)| *r)
    }
}

fn asm_err(err: iced_x86::IcedError) -> anyhow::Error {
    anyhow!("montagem da .text sintética: {err}")
}

/// Monta o arquivo e os símbolos. Pura: não toca processo nem disco.
pub fn build() -> Result<Image> {
    let (text, mut symbols) = assemble_text()?;
    // Objetos da `.data`: os endereços são RVAs.
    for (name, off) in [
        ("research_obj", OBJ_RESEARCH),
        ("level_table_obj", OBJ_TABLE),
        ("level_entries", OBJ_ENTRIES),
        ("inventory_a", OBJ_INVENTORY_A),
        ("inventory_b", OBJ_INVENTORY_B),
        ("analyzer_obj", OBJ_ANALYZER),
        ("item_a", OBJ_ITEM_A),
        ("item_b", OBJ_ITEM_B),
    ] {
        symbols.push((name, DATA_RVA + off));
    }
    // Começo da folga das variáveis: fim do `VirtualSize` alinhado a 8.
    symbols.push(("slack", (DATA_RVA + DATA_VSIZE).next_multiple_of(8)));
    symbols.push(("slack_end", (DATA_RVA + DATA_VSIZE).next_multiple_of(PAGE)));

    let text_len = text.len() as u32;
    let mut file = vec![0u8; (DATA_RVA + DATA_HEAD_LEN) as usize];
    write_headers(&mut file, text_len);
    file[TEXT_RVA as usize..TEXT_RVA as usize + text.len()].copy_from_slice(&text);
    Ok(Image {
        file,
        symbols,
        text,
    })
}

fn put_u16(f: &mut [u8], at: usize, v: u16) {
    f[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(f: &mut [u8], at: usize, v: u32) {
    f[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Cabeçalhos PE32+ mínimos: DOS, `PE\0\0`, COFF, optional header (16 diretórios vazios) e as
/// duas seções. Alinhamento de arquivo e de seção iguais à página.
fn write_headers(f: &mut [u8], text_len: u32) {
    const OPT_SIZE: usize = 112 + 16 * 8;
    f[0..2].copy_from_slice(b"MZ");
    put_u32(f, 0x3c, 0x40);
    f[0x40..0x44].copy_from_slice(b"PE\0\0");
    put_u16(f, 0x44, 0x8664);
    put_u16(f, 0x46, 2);
    put_u32(f, 0x48, TIME_DATE_STAMP);
    put_u16(f, 0x54, OPT_SIZE as u16);
    put_u16(f, 0x56, 0x0022);
    let o = 0x58;
    put_u16(f, o, 0x20b);
    put_u32(f, o + 4, PAGE); // SizeOfCode
    put_u32(f, o + 8, DATA_HEAD_LEN); // SizeOfInitializedData
    put_u32(f, o + 20, TEXT_RVA); // BaseOfCode
    f[o + 24..o + 32].copy_from_slice(&PREFERRED_BASE.to_le_bytes());
    put_u32(f, o + 32, PAGE); // SectionAlignment
    put_u32(f, o + 36, PAGE); // FileAlignment
    put_u16(f, o + 40, 6); // MajorOperatingSystemVersion
    put_u16(f, o + 48, 6); // MajorSubsystemVersion
    put_u32(f, o + 56, IMAGE_SIZE);
    put_u32(f, o + 60, PAGE); // SizeOfHeaders
    put_u16(f, o + 68, 2); // Subsystem: GUI
    put_u32(f, o + 108, 16); // NumberOfRvaAndSizes
    let sections = [
        SectionHeader {
            name: b".text",
            virtual_size: text_len,
            rva: TEXT_RVA,
            raw_size: PAGE,
            raw_ptr: TEXT_RVA,
            characteristics: 0x6000_0020,
        },
        SectionHeader {
            name: b".data",
            virtual_size: DATA_VSIZE,
            rva: DATA_RVA,
            raw_size: DATA_HEAD_LEN,
            raw_ptr: DATA_RVA,
            characteristics: 0xC000_0040,
        },
    ];
    for (i, sec) in sections.iter().enumerate() {
        let h = o + OPT_SIZE + 40 * i;
        f[h..h + sec.name.len()].copy_from_slice(sec.name);
        put_u32(f, h + 8, sec.virtual_size);
        put_u32(f, h + 12, sec.rva);
        put_u32(f, h + 16, sec.raw_size);
        put_u32(f, h + 20, sec.raw_ptr);
        put_u32(f, h + 36, sec.characteristics);
    }
}

/// Monta a `.text` e devolve os bytes e os símbolos (RVAs).
fn assemble_text() -> Result<(Vec<u8>, Symbols)> {
    let table_obj = u64::from(DATA_RVA + OBJ_TABLE);
    let mut a = CodeAssembler::new(64).map_err(asm_err)?;

    let mut sum = a.create_label();
    let mut level_gain = a.create_label();
    let mut level_pattern = a.create_label();
    let mut level_point = a.create_label();
    let mut mark_dirty = a.create_label();
    let mut get_table = a.create_label();
    let mut getter = a.create_label();
    let mut half = a.create_label();
    let mut drag = a.create_label();
    let mut drag_null = a.create_label();
    let mut drag_out = a.create_label();
    let mut add_item = a.create_label();
    let mut spin = a.create_label();
    let mut cc_long = a.create_label();
    let mut cc_short = a.create_label();
    let mut loop_top = a.create_label();
    let mut next = a.create_label();
    let mut found = a.create_label();
    let mut done = a.create_label();

    // sum(a, b) -> i32
    a.set_label(&mut sum).map_err(asm_err)?;
    a.mov(qword_ptr(rsp + 8), rbx).map_err(asm_err)?;
    a.lea(eax, ptr(rcx + rdx)).map_err(asm_err)?;
    a.mov(rbx, qword_ptr(rsp + 8)).map_err(asm_err)?;
    a.ret().map_err(asm_err)?;
    a.int3().map_err(asm_err)?;

    // Auxiliares de `level_gain`: marcam o objeto como sujo e devolvem a tabela de níveis.
    a.set_label(&mut mark_dirty).map_err(asm_err)?;
    a.ret().map_err(asm_err)?;
    a.int3().map_err(asm_err)?;
    a.set_label(&mut get_table).map_err(asm_err)?;
    // `lea rax,[rip+tabela]`: o destino é o endereço (RVA) do objeto e o encoder calcula o `disp32`.
    a.add_instruction(
        Instruction::with2(
            Code::Lea_r64_m,
            Register::RAX,
            MemoryOperand::with_base_displ(Register::RIP, table_obj as i64),
        )
        .map_err(asm_err)?,
    )
    .map_err(asm_err)?;
    a.ret().map_err(asm_err)?;
    a.int3().map_err(asm_err)?;

    // level_gain(obj, gain) -> i32 (nível depois do ganho): a forma da função do Brain Power.
    // `rbx` = objeto, `esi` = pontos antigos, `rax` = tabela no ponto do desvio.
    a.set_label(&mut level_gain).map_err(asm_err)?;
    a.mov(r11, rsp).map_err(asm_err)?;
    a.sub(rsp, 0x48).map_err(asm_err)?;
    a.mov(qword_ptr(r11 - 0x10), rbx).map_err(asm_err)?;
    a.mov(qword_ptr(r11 - 0x18), rbp).map_err(asm_err)?;
    a.mov(qword_ptr(r11 - 0x20), rsi).map_err(asm_err)?;
    a.lea(rbp, ptr(rsp + 0x10)).map_err(asm_err)?;
    a.mov(rbx, rcx).map_err(asm_err)?;
    // Padrão estendido (o do jogo, com curingas nos deslocamentos e destinos de chamada).
    a.set_label(&mut level_pattern).map_err(asm_err)?;
    a.mov(esi, dword_ptr(rcx + RESEARCH_POINTS))
        .map_err(asm_err)?;
    a.mov(qword_ptr(r11 - 0x28), r14).map_err(asm_err)?;
    a.mov(dword_ptr(rbp - 4), esi).map_err(asm_err)?;
    a.lea(eax, ptr(rsi + rdx)).map_err(asm_err)?;
    a.mov(rdx, qword_ptr(rcx + 0x10)).map_err(asm_err)?;
    a.mov(dword_ptr(rcx + RESEARCH_POINTS), eax)
        .map_err(asm_err)?;
    a.call(mark_dirty).map_err(asm_err)?;
    a.call(get_table).map_err(asm_err)?;
    a.set_label(&mut level_point).map_err(asm_err)?;
    a.mov(r14, qword_ptr(rax + TABLE_START)).map_err(asm_err)?;
    a.movsxd(rax, dword_ptr(rax + TABLE_COUNT))
        .map_err(asm_err)?;
    a.lea(rcx, ptr(rax + rax * 2)).map_err(asm_err)?;
    a.shl(rcx, 4).map_err(asm_err)?;
    a.add_instruction(
        Instruction::with2(Code::Add_r64_rm64, Register::RCX, Register::R14).map_err(asm_err)?,
    )
    .map_err(asm_err)?;
    a.add_instruction(
        Instruction::with2(Code::Cmp_r64_rm64, Register::R14, Register::RCX).map_err(asm_err)?,
    )
    .map_err(asm_err)?;
    // Desvios do padrão: `je` longo, `jge` curto.
    a.set_prefer_short_branch(false);
    a.je(done).map_err(asm_err)?;
    a.set_prefer_short_branch(true);
    a.db(&[0x0f, 0x1f, 0x80, 0, 0, 0, 0]).map_err(asm_err)?;
    a.set_label(&mut loop_top).map_err(asm_err)?;
    a.mov(eax, dword_ptr(r14 + TABLE_LIMIT)).map_err(asm_err)?;
    a.add_instruction(
        Instruction::with2(Code::Cmp_r32_rm32, Register::ESI, Register::EAX).map_err(asm_err)?,
    )
    .map_err(asm_err)?;
    a.jge(next).map_err(asm_err)?;
    a.cmp(eax, dword_ptr(rbx + RESEARCH_POINTS))
        .map_err(asm_err)?;
    a.jle(found).map_err(asm_err)?;
    // Fim do padrão. Daqui em diante o código é livre: sobe no máximo um nível.
    a.set_label(&mut next).map_err(asm_err)?;
    a.add(r14, TABLE_STRIDE).map_err(asm_err)?;
    a.cmp(r14, rcx).map_err(asm_err)?;
    a.jne(loop_top).map_err(asm_err)?;
    a.jmp(done).map_err(asm_err)?;
    a.set_label(&mut found).map_err(asm_err)?;
    a.inc(dword_ptr(rbx + RESEARCH_LEVEL)).map_err(asm_err)?;
    a.set_label(&mut done).map_err(asm_err)?;
    a.mov(eax, dword_ptr(rbx + RESEARCH_LEVEL))
        .map_err(asm_err)?;
    a.mov(r14, qword_ptr(rsp + 0x20)).map_err(asm_err)?;
    a.mov(rsi, qword_ptr(rsp + 0x28)).map_err(asm_err)?;
    a.mov(rbp, qword_ptr(rsp + 0x30)).map_err(asm_err)?;
    a.mov(rbx, qword_ptr(rsp + 0x38)).map_err(asm_err)?;
    a.add(rsp, 0x48).map_err(asm_err)?;
    a.ret().map_err(asm_err)?;
    a.int3().map_err(asm_err)?;

    // getter(obj) -> i32: a forma do getter das cargas do analisador (`movss` de 8 bytes primeiro).
    a.set_label(&mut getter).map_err(asm_err)?;
    a.movss(xmm1, dword_ptr(rcx + ANALYZER_CHARGES))
        .map_err(asm_err)?;
    a.addss(xmm1, xmm1).map_err(asm_err)?;
    a.subss(xmm1, dword_ptr(half)).map_err(asm_err)?;
    a.cvtss2si(eax, xmm1).map_err(asm_err)?;
    a.sar(eax, 1).map_err(asm_err)?;
    a.ret().map_err(asm_err)?;
    a.int3().map_err(asm_err)?;

    // drag(item, n) -> i32: `StackSize + n` do item (ou -1 com `item` nulo).
    a.set_label(&mut drag).map_err(asm_err)?;
    a.mov(qword_ptr(rsp + 8), rbx).map_err(asm_err)?;
    a.test(rcx, rcx).map_err(asm_err)?;
    a.jz(drag_null).map_err(asm_err)?;
    a.mov(rbx, rcx).map_err(asm_err)?;
    a.mov(eax, dword_ptr(rbx + ITEM_STACK)).map_err(asm_err)?;
    a.add(eax, edx).map_err(asm_err)?;
    a.jmp(drag_out).map_err(asm_err)?;
    a.set_label(&mut drag_null).map_err(asm_err)?;
    a.mov(eax, -1).map_err(asm_err)?;
    a.set_label(&mut drag_out).map_err(asm_err)?;
    a.mov(rbx, qword_ptr(rsp + 8)).map_err(asm_err)?;
    a.ret().map_err(asm_err)?;
    a.int3().map_err(asm_err)?;

    // add_item(inv, item) -> i32: soma o `StackSize` do item ao total do inventário e devolve o
    // total novo.
    a.set_label(&mut add_item).map_err(asm_err)?;
    a.mov(qword_ptr(rsp + 8), rbx).map_err(asm_err)?;
    a.mov(rbx, rcx).map_err(asm_err)?;
    a.mov(eax, dword_ptr(rdx + ITEM_STACK)).map_err(asm_err)?;
    a.add(dword_ptr(rbx + INVENTORY_TOTAL), eax)
        .map_err(asm_err)?;
    a.inc(dword_ptr(rbx + INVENTORY_COUNT)).map_err(asm_err)?;
    a.mov(eax, dword_ptr(rbx + INVENTORY_TOTAL))
        .map_err(asm_err)?;
    a.mov(rbx, qword_ptr(rsp + 8)).map_err(asm_err)?;
    a.ret().map_err(asm_err)?;
    a.int3().map_err(asm_err)?;

    // spin(): laço de 2 bytes (`jmp $`), para uma thread ficar parada num endereço conhecido.
    a.set_label(&mut spin).map_err(asm_err)?;
    a.jmp(spin).map_err(asm_err)?;
    a.int3().map_err(asm_err)?;

    // Constante do getter e as duas sequências de `CC`, separadas por bytes zero.
    a.db(&[0; 4]).map_err(asm_err)?;
    a.set_label(&mut half).map_err(asm_err)?;
    a.db(&0.25f32.to_le_bytes()).map_err(asm_err)?;
    a.db(&[0; 4]).map_err(asm_err)?;
    a.set_label(&mut cc_long).map_err(asm_err)?;
    a.db(&[0xCC; CC_RUN_LONG]).map_err(asm_err)?;
    a.db(&[0; 8]).map_err(asm_err)?;
    a.set_label(&mut cc_short).map_err(asm_err)?;
    a.db(&[0xCC; CC_RUN_SHORT]).map_err(asm_err)?;
    a.db(&[0; 4]).map_err(asm_err)?;

    let result = a
        .assemble_options(
            u64::from(TEXT_RVA),
            // Sem refazer os desvios: o `je` do padrão fica com 32 bits, como no jogo.
            iced_x86::BlockEncoderOptions::DONT_FIX_BRANCHES
                | iced_x86::BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .map_err(asm_err)?;
    let rva = |label: &iced_x86::code_asm::CodeLabel| -> Result<u32> {
        let ip = result.label_ip(label).map_err(asm_err)?;
        u32::try_from(ip).context("RVA fora do u32")
    };
    let symbols = vec![
        ("sum", rva(&sum)?),
        ("level_gain", rva(&level_gain)?),
        ("level_pattern", rva(&level_pattern)?),
        ("level_point", rva(&level_point)?),
        ("getter", rva(&getter)?),
        ("drag", rva(&drag)?),
        ("add_item", rva(&add_item)?),
        ("spin", rva(&spin)?),
        ("cc300", rva(&cc_long)?),
        ("cc70", rva(&cc_short)?),
    ];
    let text = result.inner.code_buffer;
    if text.len() > PAGE as usize {
        return Err(anyhow!("a .text sintética passou de uma página"));
    }
    Ok((text, symbols))
}

/// Escritas iniciais da `.data` (endereços absolutos), que dependem da base: a tabela de níveis
/// e os objetos de teste.
pub fn initial_data(base: u64) -> Vec<(u64, Vec<u8>)> {
    let at = |off: u32| base + u64::from(DATA_RVA + off);
    let mut writes = Vec::new();
    // Objeto da tabela: ponteiro para as entradas e a contagem.
    writes.push((
        at(OBJ_TABLE) + TABLE_START as u64,
        at(OBJ_ENTRIES).to_le_bytes().to_vec(),
    ));
    writes.push((
        at(OBJ_TABLE) + TABLE_COUNT as u64,
        (LEVEL_LIMITS.len() as i32).to_le_bytes().to_vec(),
    ));
    for (i, limit) in LEVEL_LIMITS.iter().enumerate() {
        writes.push((
            at(OBJ_ENTRIES) + (i as u64) * TABLE_STRIDE as u64 + TABLE_LIMIT as u64,
            limit.to_le_bytes().to_vec(),
        ));
    }
    // Itens: pilhas de 1 e de 5.
    writes.push((
        at(OBJ_ITEM_A) + ITEM_STACK as u64,
        1i32.to_le_bytes().to_vec(),
    ));
    writes.push((
        at(OBJ_ITEM_B) + ITEM_STACK as u64,
        5i32.to_le_bytes().to_vec(),
    ));
    writes
}

/// O módulo instalado no processo atual.
pub struct Installed {
    pub base: u64,
    pub path: PathBuf,
    /// `(nome, endereço absoluto)`.
    pub symbols: Vec<(String, u64)>,
}

/// Grava `gt-synthetic.exe` em `dir`, mapeia a imagem no processo atual e inicializa a `.data`.
pub fn install(dir: &Path) -> Result<Installed> {
    let image = build()?;
    let path = dir.join(MODULE_NAME);
    std::fs::write(&path, &image.file).with_context(|| format!("gravar {}", path.display()))?;
    let page = PAGE as usize;
    let segments = [
        ImageSegment {
            rva: 0,
            len: page,
            file_offset: Some(0),
            prot: PROT_R,
        },
        ImageSegment {
            rva: TEXT_RVA as usize,
            len: page,
            file_offset: Some(u64::from(TEXT_RVA)),
            prot: PROT_RX,
        },
        ImageSegment {
            rva: DATA_RVA as usize,
            len: DATA_HEAD_LEN as usize,
            file_offset: Some(u64::from(DATA_RVA)),
            prot: PROT_RW,
        },
        ImageSegment {
            rva: (DATA_RVA + DATA_HEAD_LEN) as usize,
            len: page,
            file_offset: None,
            prot: PROT_RW,
        },
    ];
    let base = map_image(&path, IMAGE_SIZE as usize, PREFERRED_BASE, &segments)?;
    for (addr, bytes) in initial_data(base) {
        write_local(addr, &bytes)?;
    }
    let mut symbols: Vec<(String, u64)> = image
        .symbols
        .iter()
        .map(|(n, r)| ((*n).to_string(), base + u64::from(*r)))
        .collect();
    symbols.push(("base".to_string(), base));
    Ok(Installed {
        base,
        path,
        symbols,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use gt_core::scan::{Pattern, find_all};

    /// Padrão estendido do Brain Power (spec 008, 6.6.1), do jogo.
    const BRAIN_POWER_PATTERN: &str = "8b b1 ?? ?? 00 00 4d 89 73 ?? 89 75 ?? 8d 04 16 48 8b 51 ?? 89 81 ?? ?? 00 00 e8 ?? ?? ?? ?? e8 ?? ?? ?? ?? 4c 8b b0 ?? ?? 00 00 48 63 80 ?? ?? 00 00 48 8d 0c 40 48 c1 e1 04 49 03 ce 4c 3b f1 0f 84 ?? ?? ?? ?? 0f 1f 80 00 00 00 00 41 8b 46 ?? 3b f0 7d ?? 3b 83 ?? ?? 00 00";

    fn text_of(image: &Image) -> &[u8] {
        &image.text
    }

    #[test]
    fn level_gain_matches_the_game_pattern_once() {
        let image = build().unwrap();
        let pattern = Pattern::parse(BRAIN_POWER_PATTERN).unwrap();
        let hits = find_all(text_of(&image), &pattern);
        assert_eq!(hits.len(), 1, "o padrão do Brain Power casa uma vez");
        let start = image.rva("level_pattern").unwrap() - TEXT_RVA;
        assert_eq!(hits[0] as u32, start);
        // O ponto do desvio fica em `+0x24` do padrão, como no jogo.
        assert_eq!(image.rva("level_point").unwrap() - TEXT_RVA, start + 0x24);
        // `mov r14,[rax+T]` de 7 bytes no ponto.
        let point = (image.rva("level_point").unwrap() - TEXT_RVA) as usize;
        assert_eq!(&text_of(&image)[point..point + 3], &[0x4c, 0x8b, 0xb0]);
    }

    #[test]
    fn text_has_exactly_the_two_cc_runs_and_hookable_prologues() {
        let image = build().unwrap();
        let runs = gt_core::cave::cc_runs(text_of(&image), u64::from(TEXT_RVA), 8);
        let lens: Vec<u64> = runs.iter().map(|r| r.end - r.start).collect();
        assert_eq!(
            lens.iter()
                .filter(|l| **l >= 64)
                .copied()
                .collect::<Vec<_>>(),
            [300, 70]
        );
        // Cada função-alvo começa com uma instrução de 5+ bytes que pode ser desviada.
        for (name, first) in [
            ("sum", &[0x48u8, 0x89, 0x5c, 0x24, 0x08][..]),
            ("drag", &[0x48, 0x89, 0x5c, 0x24, 0x08][..]),
            ("add_item", &[0x48, 0x89, 0x5c, 0x24, 0x08][..]),
            ("getter", &[0xf3, 0x0f, 0x10, 0x89][..]),
        ] {
            let at = (image.rva(name).unwrap() - TEXT_RVA) as usize;
            assert_eq!(&text_of(&image)[at..at + first.len()], first, "{name}");
        }
    }

    #[test]
    fn headers_parse_with_text_and_data() {
        let image = build().unwrap();
        let pe = gt_core::pe::PeHeaders::parse(&image.file).unwrap();
        assert_eq!(pe.size_of_image, IMAGE_SIZE);
        assert_eq!(pe.section(".text").unwrap().perms_string(), "r-x");
        let data = pe.section(".data").unwrap();
        assert_eq!(data.perms_string(), "rw-");
        // A `.data` termina no meio de uma página e deixa a folga do jogo.
        let end = data.rva + data.virtual_size;
        assert_eq!(end % PAGE, PAGE - SLACK_LEN);
        assert_eq!(
            image.rva("slack_end").unwrap() - image.rva("slack").unwrap(),
            SLACK_LEN
        );
        assert!(data.raw_size < data.virtual_size);
    }
}
