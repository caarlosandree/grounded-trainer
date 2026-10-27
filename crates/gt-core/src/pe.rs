//! Cabeçalho PE em memória, imagem em disco, realocações e versão.
//!
//! O cabeçalho vem de uma leitura de memória do processo ou do arquivo `.exe`; a imagem em
//! disco é montada no layout de RVA para comparar bytes e interpretar recursos.

use std::fs::{self, File};
use std::io::Read;
use std::ops::Range;
use std::path::Path;

use goblin::pe::PE;
use goblin::pe::header::{Header, PE_MAGIC, SIZEOF_COFF_HEADER, SIZEOF_PE_MAGIC};
use goblin::pe::options::ParseOptions;

use crate::error::{Error, Result};

/// Quantos bytes do início da imagem bastam para os cabeçalhos e a tabela de seções.
const HEADER_WINDOW: usize = 0x1000;

const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

/// Tipo de realocação que soma o delta a um endereço de 64 bits.
const IMAGE_REL_BASED_ABSOLUTE: u16 = 0;
const IMAGE_REL_BASED_DIR64: u16 = 10;

/// Seção de uma imagem PE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub rva: u32,
    pub virtual_size: u32,
    pub raw_size: u32,
    pub raw_offset: u32,
    pub characteristics: u32,
}

impl Section {
    /// A seção contém código executável.
    pub fn is_executable(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_EXECUTE != 0
    }

    /// A seção é legível.
    pub fn is_readable(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_READ != 0
    }

    /// A seção é gravável.
    pub fn is_writable(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_WRITE != 0
    }

    /// Permissões no formato de `/proc/<pid>/maps`, ex. `r-x`.
    pub fn perms_string(&self) -> String {
        [
            if self.is_readable() { 'r' } else { '-' },
            if self.is_writable() { 'w' } else { '-' },
            if self.is_executable() { 'x' } else { '-' },
        ]
        .iter()
        .collect()
    }
}

/// Campos do cabeçalho PE que o trainer usa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeHeaders {
    pub image_base: u64,
    pub size_of_image: u32,
    pub time_date_stamp: u32,
    pub size_of_headers: u32,
    pub sections: Vec<Section>,
}

impl PeHeaders {
    /// Interpreta os cabeçalhos a partir do início da imagem (até 0x1000 bytes).
    ///
    /// Qualquer coisa que não seja um PE com optional header devolve `BadPe`.
    pub fn parse(header_bytes: &[u8]) -> Result<PeHeaders> {
        parse_headers(header_bytes).map(|parsed| parsed.headers)
    }

    /// Chave de versão do build, `{time_date_stamp:08x}-{size_of_image:08x}`.
    pub fn version_key(&self) -> String {
        format!("{:08x}-{:08x}", self.time_date_stamp, self.size_of_image)
    }

    /// Procura uma seção pelo nome exato (ex. `.text`).
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }
}

/// Lê só o início do arquivo (até `0x1000` bytes, ou menos se ele for menor) e interpreta os
/// cabeçalhos PE. Não lê nem aloca a imagem inteira, ao contrário de [`DiskImage::load`].
pub fn read_file_headers(path: &Path) -> Result<PeHeaders> {
    let mut header = Vec::with_capacity(HEADER_WINDOW);
    File::open(path)?
        .take(HEADER_WINDOW as u64)
        .read_to_end(&mut header)?;
    PeHeaders::parse(&header)
}

/// Cabeçalhos mais os diretórios de dados `(rva, tamanho)` que a imagem em disco usa.
struct ParsedHeaders {
    headers: PeHeaders,
    /// Diretório de realocação, quando existe.
    reloc_dir: Option<(u32, u32)>,
    /// Diretório de exceção (`.pdata`), quando existe.
    exception_dir: Option<(u32, u32)>,
}

fn bad_pe(err: impl std::fmt::Display) -> Error {
    Error::BadPe(err.to_string())
}

fn parse_headers(header_bytes: &[u8]) -> Result<ParsedHeaders> {
    let bytes = &header_bytes[..header_bytes.len().min(HEADER_WINDOW)];
    let header = Header::parse(bytes).map_err(bad_pe)?;
    if header.signature != PE_MAGIC {
        return Err(Error::BadPe("assinatura PE ausente".to_string()));
    }
    let optional = header
        .optional_header
        .ok_or_else(|| Error::BadPe("sem optional header".to_string()))?;

    let table_start = header.dos_header.pe_pointer as usize
        + SIZEOF_PE_MAGIC
        + SIZEOF_COFF_HEADER
        + header.coff_header.size_of_optional_header as usize;
    let mut offset = table_start;
    let raw_sections = header
        .coff_header
        .sections(bytes, &mut offset)
        .map_err(bad_pe)?;

    let mut sections = Vec::with_capacity(raw_sections.len());
    for s in &raw_sections {
        sections.push(Section {
            name: s.name().map_err(bad_pe)?.to_string(),
            rva: s.virtual_address,
            virtual_size: s.virtual_size,
            raw_size: s.size_of_raw_data,
            raw_offset: s.pointer_to_raw_data,
            characteristics: s.characteristics,
        });
    }

    let windows = optional.windows_fields;
    let reloc_dir = optional
        .data_directories
        .get_base_relocation_table()
        .map(|dd| (dd.virtual_address, dd.size));
    let exception_dir = optional
        .data_directories
        .get_exception_table()
        .map(|dd| (dd.virtual_address, dd.size));
    let headers = PeHeaders {
        image_base: windows.image_base,
        size_of_image: windows.size_of_image,
        time_date_stamp: header.coff_header.time_date_stamp,
        size_of_headers: windows.size_of_headers,
        sections,
    };
    Ok(ParsedHeaders {
        headers,
        reloc_dir,
        exception_dir,
    })
}

/// Flag de `UNWIND_INFO` que encadeia a entrada a outra (`UNW_FLAG_CHAININFO`).
const UNW_FLAG_CHAININFO: u8 = 0x4;

/// Uma entrada da `.pdata` (`RUNTIME_FUNCTION`): a faixa da função e se o `UNWIND_INFO` dela é
/// encadeado (um pedaço de outra função, não um começo de função de verdade).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeFunction {
    /// RVA do começo.
    pub start: u32,
    /// RVA do fim (exclusivo).
    pub end: u32,
    /// `UNW_FLAG_CHAININFO` ligado (ou unwind ilegível: sem prova de que não é).
    pub chained: bool,
}

/// Imagem do arquivo `.exe` montada no layout de RVA, como o loader do Windows faria.
///
/// Aloca `SizeOfImage` bytes (≈187 MB no Grounded 2).
#[derive(Debug)]
pub struct DiskImage {
    pub headers: PeHeaders,
    image: Vec<u8>,
    reloc_dir: Option<(u32, u32)>,
    exception_dir: Option<(u32, u32)>,
    /// Base para a qual as realocações já foram aplicadas; começa em `image_base`.
    current_base: u64,
}

impl DiskImage {
    /// Lê o arquivo inteiro e monta a imagem. Não altera o arquivo.
    pub fn load(path: &Path) -> Result<DiskImage> {
        let file = fs::read(path)?;
        DiskImage::from_file_bytes(&file)
    }

    /// Monta a imagem a partir dos bytes de um arquivo PE.
    ///
    /// Os `size_of_headers` primeiros bytes vão para o RVA 0 e cada seção copia
    /// `min(raw_size, virtual_size)` bytes de `raw_offset` para `rva`; o resto fica zerado.
    pub fn from_file_bytes(file: &[u8]) -> Result<DiskImage> {
        let ParsedHeaders {
            headers,
            reloc_dir,
            exception_dir,
        } = parse_headers(file)?;
        let mut image = vec![0u8; headers.size_of_image as usize];

        let headers_len = headers.size_of_headers as usize;
        let src = file
            .get(..headers_len)
            .ok_or_else(|| Error::BadPe("cabeçalhos além do fim do arquivo".to_string()))?;
        image
            .get_mut(..headers_len)
            .ok_or_else(|| Error::BadPe("cabeçalhos maiores que a imagem".to_string()))?
            .copy_from_slice(src);

        for s in &headers.sections {
            let len = s.raw_size.min(s.virtual_size) as usize;
            let from = s.raw_offset as usize;
            let to = s.rva as usize;
            let src = file
                .get(from..from + len)
                .ok_or_else(|| Error::BadPe(format!("seção {} além do fim do arquivo", s.name)))?;
            let dst = image
                .get_mut(to..to + len)
                .ok_or_else(|| Error::BadPe(format!("seção {} além do fim da imagem", s.name)))?;
            dst.copy_from_slice(src);
        }

        let current_base = headers.image_base;
        Ok(DiskImage {
            headers,
            image,
            reloc_dir,
            exception_dir,
            current_base,
        })
    }

    /// `len` bytes a partir do RVA, ou `None` se o intervalo sai da imagem.
    pub fn bytes(&self, rva: u32, len: usize) -> Option<&[u8]> {
        let start = rva as usize;
        let end = start.checked_add(len)?;
        self.image.get(start..end)
    }

    /// Aplica as realocações `DIR64` para que a imagem fique como se estivesse em `actual_base`.
    ///
    /// O delta é relativo à base atual da imagem (começa em `image_base`), então chamadas
    /// seguidas funcionam. Se algo falhar, nada é alterado.
    pub fn relocate(&mut self, actual_base: u64) -> Result<()> {
        let delta = actual_base.wrapping_sub(self.current_base);
        if delta == 0 {
            return Ok(());
        }
        let targets = self.dir64_targets()?;
        for rva in targets {
            let at = rva as usize;
            let slot = &mut self.image[at..at + 8];
            let mut raw = [0u8; 8];
            raw.copy_from_slice(slot);
            let value = u64::from_le_bytes(raw).wrapping_add(delta);
            slot.copy_from_slice(&value.to_le_bytes());
        }
        self.current_base = actual_base;
        Ok(())
    }

    /// RVAs de todos os `u64` a corrigir, já validados contra os limites da imagem.
    fn dir64_targets(&self) -> Result<Vec<u32>> {
        let Some((dir_rva, dir_size)) = self.reloc_dir else {
            return Ok(Vec::new());
        };
        let dir = self
            .bytes(dir_rva, dir_size as usize)
            .ok_or_else(|| Error::BadPe("diretório de realocação fora da imagem".to_string()))?;

        let mut targets = Vec::new();
        let mut pos = 0usize;
        // Sobra menor que um cabeçalho de bloco é preenchimento.
        while pos + 8 <= dir.len() {
            let page_rva = read_u32(&dir[pos..]);
            let block_size = read_u32(&dir[pos + 4..]) as usize;
            if block_size < 8 {
                return Err(Error::BadPe(format!(
                    "bloco de realocação com tamanho {block_size}"
                )));
            }
            let block = dir
                .get(pos + 8..pos + block_size)
                .ok_or_else(|| Error::BadPe("bloco de realocação além do diretório".to_string()))?;
            // Sobra de um byte (bloco de tamanho ímpar) é ignorada.
            let (entries, _) = block.as_chunks::<2>();
            for &entry in entries {
                let entry = u16::from_le_bytes(entry);
                match entry >> 12 {
                    IMAGE_REL_BASED_ABSOLUTE => {}
                    IMAGE_REL_BASED_DIR64 => {
                        let rva =
                            page_rva
                                .checked_add(u32::from(entry & 0x0fff))
                                .ok_or_else(|| {
                                    Error::BadPe("RVA de realocação estourou".to_string())
                                })?;
                        if self.bytes(rva, 8).is_none() {
                            return Err(Error::BadPe(format!(
                                "realocação em {rva:#x} fora da imagem"
                            )));
                        }
                        targets.push(rva);
                    }
                    other => {
                        return Err(Error::BadPe(format!(
                            "tipo de realocação {other} não suportado"
                        )));
                    }
                }
            }
            pos += block_size;
        }
        Ok(targets)
    }

    /// Limites das funções (RVA inicial e final) segundo o diretório de exceção (`.pdata`),
    /// em ordem crescente. Cada entrada `RUNTIME_FUNCTION` tem 12 bytes; entradas vazias ou
    /// com fim antes do início são ignoradas. Sem diretório de exceção, a lista é vazia.
    pub fn functions(&self) -> Result<Vec<Range<u32>>> {
        Ok(self
            .runtime_entries()?
            .into_iter()
            .map(|(range, _)| range)
            .collect())
    }

    /// Como [`DiskImage::functions`], mais o flag de unwind encadeado de cada entrada (lido do
    /// `UNWIND_INFO` na imagem). Entrada de unwind que não dá para ler (fora da imagem) ou indireta
    /// (bit 0 do `UnwindData` ligado) conta como encadeada: sem prova de que não é.
    pub fn runtime_functions(&self) -> Result<Vec<RuntimeFunction>> {
        Ok(self
            .runtime_entries()?
            .into_iter()
            .map(|(range, unwind)| RuntimeFunction {
                start: range.start,
                end: range.end,
                chained: self.unwind_is_chained(unwind),
            })
            .collect())
    }

    /// `(faixa, UnwindData)` de cada `RUNTIME_FUNCTION` válida, em ordem de início.
    fn runtime_entries(&self) -> Result<Vec<(Range<u32>, u32)>> {
        let Some((dir_rva, dir_size)) = self.exception_dir else {
            return Ok(Vec::new());
        };
        let dir = self
            .bytes(dir_rva, dir_size as usize)
            .ok_or_else(|| Error::BadPe("diretório de exceção fora da imagem".to_string()))?;
        // Sobra menor que uma entrada é preenchimento.
        let (entries, _) = dir.as_chunks::<12>();
        let mut functions: Vec<(Range<u32>, u32)> = entries
            .iter()
            .map(|entry| {
                (
                    read_u32(&entry[0..4])..read_u32(&entry[4..8]),
                    read_u32(&entry[8..12]),
                )
            })
            .filter(|(f, _)| f.start < f.end)
            .collect();
        functions.sort_by_key(|(f, _)| f.start);
        Ok(functions)
    }

    /// O `UNWIND_INFO` em `unwind` tem `UNW_FLAG_CHAININFO`? Sem como ler, diz que sim.
    fn unwind_is_chained(&self, unwind: u32) -> bool {
        if unwind & 1 != 0 {
            return true;
        }
        match self.bytes(unwind, 1) {
            // Versão nos 3 bits baixos, flags nos 5 altos.
            Some(&[first]) => (first >> 3) & UNW_FLAG_CHAININFO != 0,
            _ => true,
        }
    }

    /// `ProductVersion` do recurso de versão (ex. `Augusta-CL-3207493`), se existir.
    pub fn product_version(&self) -> Option<String> {
        // `ParseOptions` é `#[non_exhaustive]`: parte do padrão e muda os campos.
        // Com `resolve_rva = true` o parse do exe real falha em "cannot map tls callback".
        let mut opts = ParseOptions::default();
        opts.resolve_rva = false;
        opts.parse_imports = false;
        opts.parse_attribute_certificates = false;
        let pe = PE::parse_with_opts(&self.image, &opts).ok()?;
        pe.resource_data?
            .version_info?
            .string_info
            .product_version()
    }
}

/// Little endian; quem chama garante `bytes.len() >= 4`.
fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Confere se a versão na memória é a mesma do disco (`time_date_stamp` e `size_of_image`).
pub fn check_same_version(memory: &PeHeaders, disk: &PeHeaders) -> Result<()> {
    if memory.time_date_stamp == disk.time_date_stamp && memory.size_of_image == disk.size_of_image
    {
        Ok(())
    } else {
        Err(Error::VersionMismatch {
            memory: memory.version_key(),
            disk: disk.version_key(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Seção de um PE de teste: `raw_size` é o tamanho de `data`.
    pub(crate) struct FakeSection {
        name: &'static str,
        rva: u32,
        vsize: u32,
        characteristics: u32,
        data: Vec<u8>,
    }

    fn sec(
        name: &'static str,
        rva: u32,
        vsize: u32,
        characteristics: u32,
        data_len: usize,
    ) -> FakeSection {
        FakeSection {
            name,
            rva,
            vsize,
            characteristics,
            data: vec![0; data_len],
        }
    }

    /// `.text` em `0x1000` com o `u64` `0x140002000` no offset `0x10`.
    fn text_with_ptr() -> FakeSection {
        let mut s = sec(".text", 0x1000, 0x1000, 0x6000_0020, 0x200);
        s.data[0x10..0x18].copy_from_slice(&0x1_4000_2000u64.to_le_bytes());
        s
    }

    /// `.reloc` em `0x2000` com um bloco: página `0x1000`, tamanho 12 e as entradas
    /// `DIR64` no offset `0x10` e `ABSOLUTE` (preenchimento).
    fn reloc_block() -> FakeSection {
        let mut data = Vec::new();
        data.extend_from_slice(&0x1000u32.to_le_bytes());
        data.extend_from_slice(&12u32.to_le_bytes());
        data.extend_from_slice(&((10u16 << 12) | 0x10).to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        FakeSection {
            name: ".reloc",
            rva: 0x2000,
            vsize: 12,
            characteristics: 0x4200_0040,
            data,
        }
    }

    fn align(value: usize, to: usize) -> usize {
        value.div_ceil(to) * to
    }

    /// Monta um arquivo PE32+ mínimo: DOS header, `PE\0\0`, COFF, optional header com
    /// 16 data directories, tabela de seções e o conteúdo bruto de cada seção.
    pub(crate) fn fake_pe(
        sections: &[FakeSection],
        image_base: u64,
        time_date_stamp: u32,
        size_of_image: u32,
        reloc_dir: Option<(u32, u32)>,
    ) -> Vec<u8> {
        const FILE_ALIGN: usize = 0x200;
        const OPT_SIZE: usize = 112 + 16 * 8;
        let headers_end = 0x40 + 4 + 20 + OPT_SIZE + 40 * sections.len();
        let size_of_headers = align(headers_end, FILE_ALIGN);

        let mut f = vec![0u8; size_of_headers];
        f[0..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        f[0x40..0x44].copy_from_slice(b"PE\0\0");
        // COFF
        f[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
        f[0x46..0x48].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        f[0x48..0x4c].copy_from_slice(&time_date_stamp.to_le_bytes());
        f[0x54..0x56].copy_from_slice(&(OPT_SIZE as u16).to_le_bytes());
        f[0x56..0x58].copy_from_slice(&0x0022u16.to_le_bytes());
        // optional header (PE32+)
        let o = 0x58;
        f[o..o + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        f[o + 24..o + 32].copy_from_slice(&image_base.to_le_bytes());
        f[o + 32..o + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        f[o + 36..o + 40].copy_from_slice(&(FILE_ALIGN as u32).to_le_bytes());
        f[o + 56..o + 60].copy_from_slice(&size_of_image.to_le_bytes());
        f[o + 60..o + 64].copy_from_slice(&(size_of_headers as u32).to_le_bytes());
        f[o + 108..o + 112].copy_from_slice(&16u32.to_le_bytes());
        if let Some((rva, size)) = reloc_dir {
            let d = o + 112 + 5 * 8;
            f[d..d + 4].copy_from_slice(&rva.to_le_bytes());
            f[d + 4..d + 8].copy_from_slice(&size.to_le_bytes());
        }
        // tabela de seções e dados
        let mut raw_offset = size_of_headers;
        for (i, s) in sections.iter().enumerate() {
            let h = o + OPT_SIZE + 40 * i;
            f[h..h + s.name.len()].copy_from_slice(s.name.as_bytes());
            f[h + 8..h + 12].copy_from_slice(&s.vsize.to_le_bytes());
            f[h + 12..h + 16].copy_from_slice(&s.rva.to_le_bytes());
            f[h + 16..h + 20].copy_from_slice(&(s.data.len() as u32).to_le_bytes());
            f[h + 20..h + 24].copy_from_slice(&(raw_offset as u32).to_le_bytes());
            f[h + 36..h + 40].copy_from_slice(&s.characteristics.to_le_bytes());
            raw_offset += align(s.data.len(), FILE_ALIGN);
        }
        f.resize(raw_offset, 0);
        let mut at = size_of_headers;
        for s in sections {
            f[at..at + s.data.len()].copy_from_slice(&s.data);
            at += align(s.data.len(), FILE_ALIGN);
        }
        f
    }

    fn u64_at(img: &DiskImage, rva: u32) -> u64 {
        let bytes = img.bytes(rva, 8).unwrap();
        u64::from_le_bytes(bytes.try_into().unwrap())
    }

    #[test]
    fn parses_headers_and_sections() {
        let h = PeHeaders::parse(&fake_pe(
            &[
                sec(".text", 0x1000, 0x2000, 0x6000_0020, 0x200),
                sec(".data", 0x3000, 0x6bbecc, 0xC000_0040, 0x400),
            ],
            0x1_4000_0000,
            0xc935e799,
            0x0b27c000,
            None,
        ))
        .unwrap();
        assert_eq!(h.version_key(), "c935e799-0b27c000");
        assert_eq!(h.image_base, 0x1_4000_0000);
        assert_eq!(h.sections.len(), 2);
        assert!(h.section(".text").unwrap().is_executable());
        assert!(h.section(".text").unwrap().is_readable());
        assert_eq!(h.section(".text").unwrap().perms_string(), "r-x");
        assert_eq!(h.section(".data").unwrap().virtual_size, 0x6bbecc);
        assert_eq!(h.section(".data").unwrap().raw_size, 0x400);
        assert_eq!(h.section(".data").unwrap().perms_string(), "rw-");
        assert!(h.section(".nada").is_none());
    }

    #[test]
    fn read_file_headers_reads_only_the_header() {
        let mut file = fake_pe(
            &[sec(".text", 0x1000, 0x1000, 0x6000_0020, 0x200)],
            0x1_4000_0000,
            0xc935e799,
            0x3000,
            None,
        );
        // O arquivo passa bastante do que o leitor de cabeçalho precisa.
        file.resize(64 * 1024, 0xcc);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.exe");
        fs::write(&path, &file).unwrap();
        let from_file = read_file_headers(&path).unwrap();
        assert_eq!(
            from_file,
            DiskImage::from_file_bytes(&file).unwrap().headers
        );
        assert_eq!(from_file.size_of_image, 0x3000);
        // Arquivo menor que a janela: lê o que houver.
        let small = tmp.path().join("small.exe");
        fs::write(&small, &file[..0x600]).unwrap();
        assert_eq!(read_file_headers(&small).unwrap(), from_file);
        // Inexistente: erro de E/S, sem pânico.
        assert!(read_file_headers(&tmp.path().join("ausente.exe")).is_err());
    }

    #[test]
    fn functions_from_synthetic_pdata() {
        // Três entradas fora de ordem, uma vazia e uma com fim antes do início; sobra de 4 bytes.
        let mut pdata = Vec::new();
        for (begin, end) in [
            (0x1100u32, 0x1180u32),
            (0x1000, 0x1040),
            (0, 0),
            (0x1200, 0x1100),
            (0x1040, 0x1100),
        ] {
            pdata.extend_from_slice(&begin.to_le_bytes());
            pdata.extend_from_slice(&end.to_le_bytes());
            pdata.extend_from_slice(&0x5000u32.to_le_bytes());
        }
        let size = pdata.len() as u32;
        pdata.extend_from_slice(&[0xAA; 4]);
        let mut section = sec(".pdata", 0x2000, 0x100, 0x4000_0040, 0);
        section.data = pdata;
        let mut file = fake_pe(&[text_with_ptr(), section], 0x1_4000_0000, 1, 0x3000, None);
        // Diretório de exceção: índice 3 das data directories (optional header em 0x58).
        let dir = 0x58 + 112 + 3 * 8;
        file[dir..dir + 4].copy_from_slice(&0x2000u32.to_le_bytes());
        file[dir + 4..dir + 8].copy_from_slice(&(size + 4).to_le_bytes());
        let img = DiskImage::from_file_bytes(&file).unwrap();
        assert_eq!(
            img.functions().unwrap(),
            vec![0x1000..0x1040, 0x1040..0x1100, 0x1100..0x1180]
        );

        // Sem diretório de exceção: lista vazia.
        let none = DiskImage::from_file_bytes(&fake_pe(
            &[text_with_ptr()],
            0x1_4000_0000,
            1,
            0x2000,
            None,
        ))
        .unwrap();
        assert_eq!(none.functions().unwrap(), Vec::<Range<u32>>::new());

        // Diretório fora da imagem: erro.
        let mut bad = file.clone();
        bad[dir..dir + 4].copy_from_slice(&0x2ff8u32.to_le_bytes());
        let img = DiskImage::from_file_bytes(&bad).unwrap();
        assert!(matches!(img.functions(), Err(Error::BadPe(_))));
    }

    #[test]
    fn runtime_functions_report_chained_unwind() {
        // Quatro entradas (uma fora de ordem). O unwind info mora na mesma seção, em 0x2080:
        // versão 1 sem flags (`0x01`), versão 1 com `UNW_FLAG_CHAININFO` (`0x21`); a terceira
        // aponta para fora da imagem e a quarta tem o bit 0 ligado (unwind indireto).
        let entries = [
            (0x1040u32, 0x1080u32, 0x2084u32),
            (0x1000, 0x1040, 0x2080),
            (0x1080, 0x10c0, 0x5000),
            (0x10c0, 0x1100, 0x2081),
        ];
        let mut pdata = vec![0u8; 0x88];
        for (i, (begin, end, unwind)) in entries.iter().enumerate() {
            pdata[i * 12..i * 12 + 4].copy_from_slice(&begin.to_le_bytes());
            pdata[i * 12 + 4..i * 12 + 8].copy_from_slice(&end.to_le_bytes());
            pdata[i * 12 + 8..i * 12 + 12].copy_from_slice(&unwind.to_le_bytes());
        }
        pdata[0x80] = 0x01;
        pdata[0x84] = 0x21;
        let mut section = sec(".pdata", 0x2000, 0x100, 0x4000_0040, 0);
        section.data = pdata;
        let mut file = fake_pe(&[text_with_ptr(), section], 0x1_4000_0000, 1, 0x3000, None);
        let dir = 0x58 + 112 + 3 * 8;
        file[dir..dir + 4].copy_from_slice(&0x2000u32.to_le_bytes());
        file[dir + 4..dir + 8].copy_from_slice(&(4u32 * 12).to_le_bytes());
        let img = DiskImage::from_file_bytes(&file).unwrap();
        let got: Vec<(u32, u32, bool)> = img
            .runtime_functions()
            .unwrap()
            .iter()
            .map(|f| (f.start, f.end, f.chained))
            .collect();
        assert_eq!(
            got,
            vec![
                (0x1000, 0x1040, false),
                (0x1040, 0x1080, true),
                (0x1080, 0x10c0, true),
                (0x10c0, 0x1100, true),
            ]
        );
        // `functions()` continua devolvendo só as faixas.
        assert_eq!(img.functions().unwrap().len(), 4);
    }

    #[test]
    fn rejects_non_pe() {
        assert!(matches!(
            PeHeaders::parse(b"\x7fELF\0\0\0\0"),
            Err(Error::BadPe(_))
        ));
    }

    #[test]
    fn version_mismatch_is_reported() {
        let a = PeHeaders::parse(&fake_pe(&[], 0x1_4000_0000, 1, 0x1000, None)).unwrap();
        let b = PeHeaders::parse(&fake_pe(&[], 0x1_4000_0000, 2, 0x1000, None)).unwrap();
        assert!(matches!(
            check_same_version(&a, &b),
            Err(Error::VersionMismatch { .. })
        ));
        assert!(check_same_version(&a, &a).is_ok());
    }

    #[test]
    fn relocation_dir64_applies_delta() {
        let mut img = DiskImage::from_file_bytes(&fake_pe(
            &[text_with_ptr(), reloc_block()],
            0x1_4000_0000,
            1,
            0x3000,
            Some((0x2000, 12)),
        ))
        .unwrap();
        img.relocate(0x1_4000_0000).unwrap();
        assert_eq!(u64_at(&img, 0x1010), 0x1_4000_2000);
        img.relocate(0x1_5000_0000).unwrap();
        assert_eq!(u64_at(&img, 0x1010), 0x1_5000_2000);
        // A nova base pode ser menor que a atual.
        img.relocate(0x1_3000_0000).unwrap();
        assert_eq!(u64_at(&img, 0x1010), 0x1_3000_2000);
    }

    #[test]
    fn relocation_rejects_tiny_block_and_unknown_type() {
        let mut tiny = reloc_block();
        tiny.data[4..8].copy_from_slice(&4u32.to_le_bytes());
        let mut img = DiskImage::from_file_bytes(&fake_pe(
            &[text_with_ptr(), tiny],
            0x1_4000_0000,
            1,
            0x3000,
            Some((0x2000, 12)),
        ))
        .unwrap();
        assert!(matches!(img.relocate(0x1_5000_0000), Err(Error::BadPe(_))));

        let mut other = reloc_block();
        other.data[8..10].copy_from_slice(&((3u16 << 12) | 0x10).to_le_bytes());
        let mut img = DiskImage::from_file_bytes(&fake_pe(
            &[text_with_ptr(), other],
            0x1_4000_0000,
            1,
            0x3000,
            Some((0x2000, 12)),
        ))
        .unwrap();
        assert!(matches!(img.relocate(0x1_5000_0000), Err(Error::BadPe(_))));
        // Falhou sem aplicar nada.
        assert_eq!(u64_at(&img, 0x1010), 0x1_4000_2000);
    }

    #[test]
    fn bytes_outside_image_is_none() {
        let img = DiskImage::from_file_bytes(&fake_pe(
            &[text_with_ptr(), reloc_block()],
            0x1_4000_0000,
            1,
            0x3000,
            Some((0x2000, 12)),
        ))
        .unwrap();
        assert!(img.bytes(0x2ff0, 0x20).is_none());
        assert!(img.bytes(0x2ff0, 0x10).is_some());
        assert!(img.bytes(u32::MAX, usize::MAX).is_none());
    }

    #[test]
    fn product_version_from_fake_is_none() {
        let img = DiskImage::from_file_bytes(&fake_pe(
            &[text_with_ptr()],
            0x1_4000_0000,
            1,
            0x2000,
            None,
        ))
        .unwrap();
        assert_eq!(img.product_version(), None);
    }

    #[test]
    #[ignore = "exige o executável do jogo em disco"]
    fn real_exe_on_disk() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let path = home.join(".local/share/Steam/steamapps/common/Grounded2/Augusta/Binaries/Win64/Grounded2Steam-Win64-Shipping.exe");
        let img = DiskImage::load(&path).unwrap();
        if img.headers.version_key() != "c935e799-0b27c000" {
            eprintln!("pulado: build diferente");
            return;
        }
        assert_eq!(img.product_version().as_deref(), Some("Augusta-CL-3207493"));
        assert_eq!(img.headers.sections.len(), 8);
    }
}
