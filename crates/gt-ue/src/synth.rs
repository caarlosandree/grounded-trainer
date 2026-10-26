//! Reflexão sintética para testes (feature `synth`): uma memória esparsa e um construtor que
//! monta `GUObjectArray`, `FNamePool`, classes, campos e objetos no layout informado.
//!
//! Os offsets dos campos das classes sintéticas (como `Actor.RootComponent`) são arbitrários e
//! só valem para os testes; nada aqui descreve o jogo.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use gt_core::Error as CoreError;
use gt_core::mem::ReadMem;

use crate::layout::UeLayout;

const PAGE: u64 = 4096;
/// Entradas de ponteiro de bloco gravadas (o resto da lista fica zerado).
const NAME_BLOCK_PTRS: usize = 64;
/// Espaço reservado para o bloco 0 do pool de nomes (máximo do índice de 16 bits × 2 bytes).
const NAME_BLOCK_BYTES: u64 = 0x20000;

/// Tamanho de `Actor` sintético (propriedades incluídas).
pub const ACTOR_PROPS_SIZE: u32 = 0x40;
/// Offset de `Actor.RootComponent` sintético.
pub const ACTOR_ROOT_COMPONENT_OFFSET: u32 = 0x30;

/// Memória esparsa, em páginas de 4 KiB. Leitura que toca página não mapeada falha com `Unmapped`.
#[derive(Default)]
pub struct SparseMem {
    pages: Mutex<HashMap<u64, Vec<u8>>>,
}

impl SparseMem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Memória com as escritas de `image` aplicadas, na ordem.
    pub fn from_image(image: &SynthImage) -> Self {
        let mem = Self::new();
        for (addr, bytes) in &image.writes {
            mem.write(*addr, bytes);
        }
        mem
    }

    /// Grava `bytes` em `addr`, mapeando as páginas que faltarem. Serve também para os testes
    /// simularem o jogo mudando a memória.
    pub fn write(&self, addr: u64, bytes: &[u8]) {
        let mut pages = self.pages.lock().unwrap_or_else(PoisonError::into_inner);
        let mut cur = addr;
        let mut rest = bytes;
        while !rest.is_empty() {
            let base = cur & !(PAGE - 1);
            let off = (cur - base) as usize;
            let n = (PAGE as usize - off).min(rest.len());
            let page = pages.entry(base).or_insert_with(|| vec![0; PAGE as usize]);
            page[off..off + n].copy_from_slice(&rest[..n]);
            cur += n as u64;
            rest = &rest[n..];
        }
    }

    /// Mapeia `len` bytes a partir de `addr` com zeros, sem apagar o que já existe.
    pub fn map_zero(&self, addr: u64, len: u64) {
        if len == 0 {
            return;
        }
        let mut pages = self.pages.lock().unwrap_or_else(PoisonError::into_inner);
        let first = addr & !(PAGE - 1);
        let last = (addr + len - 1) & !(PAGE - 1);
        let mut base = first;
        while base <= last {
            pages.entry(base).or_insert_with(|| vec![0; PAGE as usize]);
            base += PAGE;
        }
    }
}

impl ReadMem for SparseMem {
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> gt_core::Result<()> {
        let len = buf.len();
        let unmapped = || CoreError::Unmapped { addr, len };
        if addr.checked_add(len as u64).is_none() {
            return Err(unmapped());
        }
        let pages = self.pages.lock().unwrap_or_else(PoisonError::into_inner);
        let mut cur = addr;
        let mut done = 0usize;
        while done < len {
            let base = cur & !(PAGE - 1);
            let off = (cur - base) as usize;
            let n = (PAGE as usize - off).min(len - done);
            let page = pages.get(&base).ok_or_else(unmapped)?;
            buf[done..done + n].copy_from_slice(&page[off..off + n]);
            cur += n as u64;
            done += n;
        }
        Ok(())
    }
}

/// Campo de uma classe sintética.
#[derive(Debug, Clone, Copy)]
pub struct SynthField {
    pub name: &'static str,
    /// Nome do tipo (`FloatProperty`, `ObjectProperty`, `BoolProperty`, ...).
    pub kind: &'static str,
    pub offset: u32,
    pub size: u32,
    /// Para `BoolProperty`: (`ByteOffset`, `FieldMask`).
    pub bool_mask: Option<(u8, u8)>,
}

/// Resultado do construtor: escritas a aplicar (em ordem) e os endereços das globais.
#[derive(Debug, Clone)]
pub struct SynthImage {
    pub writes: Vec<(u64, Vec<u8>)>,
    /// Endereço do `GUObjectArray`.
    pub gobjects: u64,
    /// Endereço do ponteiro do bloco 0 do `FNamePool`.
    pub names: u64,
}

impl SynthImage {
    /// Atalho para [`SparseMem::from_image`].
    pub fn into_mem(&self) -> SparseMem {
        SparseMem::from_image(self)
    }
}

fn round8(n: u32) -> u32 {
    (n + 7) & !7
}

/// Construtor da reflexão sintética.
pub struct SynthUe {
    layout: UeLayout,
    next: u64,
    end: u64,
    writes: Vec<(u64, Vec<u8>)>,
    objects: Vec<u64>,
    name_data: Vec<u8>,
    name_ids: HashMap<String, u32>,
    names_ptrs: u64,
    names_block: u64,
    gobjects: u64,
    /// Tamanho da instância de cada classe.
    instance_sizes: HashMap<u64, u32>,
    field_classes: HashMap<&'static str, u64>,
    /// `FField` de cada campo, por (classe ou struct, nome).
    field_addrs: HashMap<(u64, &'static str), u64>,
    /// Classe das structs (`ScriptStruct`), criada na primeira [`SynthUe::add_struct`].
    script_struct_class: u64,
    /// `/Script/CoreUObject`.
    pub core_package: u64,
    /// `/Script/Engine`.
    pub engine_package: u64,
    /// Classe dos pacotes.
    pub package_class: u64,
    /// `Class`, classe de si mesma.
    pub class_class: u64,
    pub object_class: u64,
    pub actor_class: u64,
}

impl SynthUe {
    /// `arena` é o primeiro endereço livre e `arena_len` o espaço disponível (endereços virtuais:
    /// só o que é escrito ocupa memória na [`SparseMem`]).
    pub fn new(layout: UeLayout, arena: u64, arena_len: usize) -> Self {
        let mut s = Self {
            layout,
            next: arena,
            end: arena + arena_len as u64,
            writes: Vec::new(),
            objects: Vec::new(),
            name_data: Vec::new(),
            name_ids: HashMap::new(),
            names_ptrs: 0,
            names_block: 0,
            gobjects: 0,
            instance_sizes: HashMap::new(),
            field_classes: HashMap::new(),
            field_addrs: HashMap::new(),
            script_struct_class: 0,
            core_package: 0,
            engine_package: 0,
            package_class: 0,
            class_class: 0,
            object_class: 0,
            actor_class: 0,
        };
        let o = layout.objects;
        let gobjects_size = round8(
            (o.chunks + 8)
                .max(o.max_elements + 4)
                .max(o.num_elements + 4)
                .max(o.max_chunks + 4)
                .max(o.num_chunks + 4)
                .max(0x40),
        );
        s.gobjects = s.alloc(u64::from(gobjects_size), 16);
        s.names_ptrs = s.alloc((NAME_BLOCK_PTRS * 8) as u64, 16);
        s.names_block = s.alloc(NAME_BLOCK_BYTES, 16);
        s.intern_name("None");

        // Os objetos fundamentais se referenciam (Class é classe de si mesma, os pacotes são
        // objetos da classe Package), então os endereços saem antes de os objetos serem montados.
        let header = s.header_size();
        let class_size = s.class_size();
        s.core_package = s.alloc(u64::from(header), 16);
        s.engine_package = s.alloc(u64::from(header), 16);
        s.package_class = s.alloc(u64::from(class_size), 16);
        s.class_class = s.alloc(u64::from(class_size), 16);
        s.object_class = s.alloc(u64::from(class_size), 16);
        s.actor_class = s.alloc(u64::from(class_size), 16);
        let (core, engine) = (s.core_package, s.engine_package);
        let (pkg_c, class_c, object_c, actor_c) = (
            s.package_class,
            s.class_class,
            s.object_class,
            s.actor_class,
        );

        s.init_object(core, pkg_c, header, "/Script/CoreUObject", 0);
        s.init_object(engine, pkg_c, header, "/Script/Engine", 0);
        s.init_object(pkg_c, class_c, class_size, "Package", core);
        s.init_object(class_c, class_c, class_size, "Class", core);
        s.init_object(object_c, class_c, class_size, "Object", core);
        s.init_object(actor_c, class_c, class_size, "Actor", engine);

        s.init_struct(pkg_c, Some(object_c), 0, header);
        s.init_struct(class_c, Some(object_c), 0, class_size);
        s.init_struct(object_c, None, 0, header);
        let root = [SynthField {
            name: "RootComponent",
            kind: "ObjectProperty",
            offset: ACTOR_ROOT_COMPONENT_OFFSET,
            size: 8,
            bool_mask: None,
        }];
        let first = s.build_fields(actor_c, &root);
        s.init_struct(actor_c, Some(object_c), first, ACTOR_PROPS_SIZE);

        s.instance_sizes.insert(pkg_c, header);
        s.instance_sizes.insert(class_c, class_size);
        s.instance_sizes.insert(object_c, header);
        s.instance_sizes
            .insert(actor_c, ACTOR_PROPS_SIZE.max(header));
        s
    }

    /// Objetos criados, na ordem do índice na tabela.
    pub fn objects(&self) -> &[u64] {
        &self.objects
    }

    /// Índice de comparação do nome `text` no pool (cria a entrada se for nova).
    pub fn intern_name(&mut self, text: &str) -> u32 {
        if let Some(&id) = self.name_ids.get(text) {
            return id;
        }
        assert!(text.len() < 1024, "nome sintético grande demais");
        let wide = !text.is_ascii();
        let (len, body): (usize, Vec<u8>) = if wide {
            let units: Vec<u16> = text.encode_utf16().collect();
            (
                units.len(),
                units.iter().flat_map(|u| u.to_le_bytes()).collect(),
            )
        } else {
            (text.len(), text.as_bytes().to_vec())
        };
        assert!(len < 1024, "nome sintético grande demais");
        let header = ((len as u16) << 6) | u16::from(wide);
        let offset = self.name_data.len();
        assert!(
            offset.is_multiple_of(2) && offset / 2 <= 0xffff,
            "bloco de nomes cheio"
        );
        self.name_data.extend_from_slice(&header.to_le_bytes());
        self.name_data.extend_from_slice(&body);
        if !self.name_data.len().is_multiple_of(2) {
            self.name_data.push(0);
        }
        assert!(
            (self.name_data.len() as u64) <= NAME_BLOCK_BYTES,
            "bloco de nomes cheio"
        );
        let id = (offset / 2) as u32;
        self.name_ids.insert(text.to_string(), id);
        id
    }

    /// Cria um pacote (objeto da classe `Package`, sem outer).
    pub fn add_package(&mut self, name: &str) -> u64 {
        let header = self.header_size();
        let addr = self.alloc(u64::from(header), 16);
        self.init_object(addr, self.package_class, header, name, 0);
        addr
    }

    /// Cria uma classe em `pkg`, com a cadeia de campos `fields` (na ordem dada).
    pub fn add_class(
        &mut self,
        pkg: u64,
        name: &str,
        super_class: Option<u64>,
        props_size: u32,
        fields: &[SynthField],
    ) -> u64 {
        self.add_class_of(self.class_class, pkg, name, super_class, props_size, fields)
    }

    /// Como [`SynthUe::add_class`], mas com a meta-classe do objeto dada: uma classe de Blueprint
    /// tem como classe uma `BlueprintGeneratedClass`, que é filha de `Class`.
    pub fn add_class_of(
        &mut self,
        meta_class: u64,
        pkg: u64,
        name: &str,
        super_class: Option<u64>,
        props_size: u32,
        fields: &[SynthField],
    ) -> u64 {
        let class_size = self.class_size();
        let addr = self.alloc(u64::from(class_size), 16);
        self.init_object(addr, meta_class, class_size, name, pkg);
        let first = self.build_fields(addr, fields);
        self.init_struct(addr, super_class, first, props_size);
        self.instance_sizes
            .insert(addr, props_size.max(self.header_size()));
        addr
    }

    /// Cria uma `UScriptStruct` em `pkg` com os campos dados (offsets relativos ao início da
    /// struct). Ela não é uma classe: não entra no índice, só é alcançada por uma `StructProperty`
    /// ligada com [`SynthUe::link_struct`].
    pub fn add_struct(&mut self, pkg: u64, name: &str, size: u32, fields: &[SynthField]) -> u64 {
        if self.script_struct_class == 0 {
            let (core, object) = (self.core_package, self.object_class);
            self.script_struct_class =
                self.add_class(core, "ScriptStruct", Some(object), self.class_size(), &[]);
        }
        let class_size = self.class_size();
        let addr = self.alloc(u64::from(class_size), 16);
        self.init_object(addr, self.script_struct_class, class_size, name, pkg);
        let first = self.build_fields(addr, fields);
        self.init_struct(addr, None, first, size);
        addr
    }

    /// Liga a `StructProperty` `field` de `owner` (classe ou struct) à struct `strukt`.
    pub fn link_struct(&mut self, owner: u64, field: &'static str, strukt: u64) {
        let Some(&addr) = self.field_addrs.get(&(owner, field)) else {
            panic!("campo sintético {field} inexistente");
        };
        let at = addr + u64::from(self.layout.fstruct.struct_ptr);
        self.write_u64(at, strukt);
    }

    /// Cria uma instância de `class`, zerada, com o cabeçalho preenchido.
    pub fn add_object(&mut self, class: u64, name: &str, outer: u64) -> u64 {
        let size = self
            .instance_sizes
            .get(&class)
            .copied()
            .unwrap_or_else(|| self.header_size());
        let addr = self.alloc(u64::from(size), 16);
        self.init_object(addr, class, size, name, outer);
        addr
    }

    /// Grava `bytes` em `obj + offset` (depois de tudo o que já foi montado).
    pub fn set(&mut self, obj: u64, offset: u32, bytes: &[u8]) {
        self.writes.push((obj + u64::from(offset), bytes.to_vec()));
    }

    /// Fecha a imagem: tabela de itens, cabeçalho do `GUObjectArray` e pool de nomes.
    pub fn finish(mut self) -> SynthImage {
        let o = self.layout.objects;
        let count = self.objects.len() as u64;
        let per_chunk = u64::from(o.items_per_chunk);
        let num_chunks = count.div_ceil(per_chunk).max(1);
        let chunk_bytes = per_chunk * u64::from(o.item_size);
        let bases: Vec<u64> = (0..num_chunks)
            .map(|_| self.alloc(chunk_bytes, 16))
            .collect();
        let list = self.alloc(num_chunks * 8, 16);

        let gobjects_size = round8((o.chunks + 8).max(o.num_chunks + 4).max(0x40));
        let mut writes = std::mem::take(&mut self.writes);
        let mut push = |addr: u64, bytes: Vec<u8>| writes.push((addr, bytes));

        // Nomes: lista de blocos (só o bloco 0) e o conteúdo do bloco.
        let mut ptrs = vec![0u8; NAME_BLOCK_PTRS * 8];
        ptrs[..8].copy_from_slice(&self.names_block.to_le_bytes());
        push(self.names_ptrs, ptrs);
        push(self.names_block, self.name_data.clone());

        // Tabela de itens, um item por objeto.
        for (index, &obj) in self.objects.iter().enumerate() {
            let index = index as u64;
            let mut item = vec![0u8; o.item_size as usize];
            let at = o.item_object as usize;
            item[at..at + 8].copy_from_slice(&obj.to_le_bytes());
            let base = bases[(index / per_chunk) as usize];
            push(base + (index % per_chunk) * u64::from(o.item_size), item);
        }
        let list_bytes: Vec<u8> = bases.iter().flat_map(|b| b.to_le_bytes()).collect();
        push(list, list_bytes);

        // Cabeçalho do GUObjectArray.
        push(self.gobjects, vec![0u8; gobjects_size as usize]);
        let max_chunks = num_chunks + 2;
        push(
            self.gobjects + u64::from(o.chunks),
            list.to_le_bytes().to_vec(),
        );
        push(
            self.gobjects + u64::from(o.max_elements),
            ((max_chunks * per_chunk) as u32).to_le_bytes().to_vec(),
        );
        push(
            self.gobjects + u64::from(o.num_elements),
            (count as u32).to_le_bytes().to_vec(),
        );
        push(
            self.gobjects + u64::from(o.max_chunks),
            (max_chunks as u32).to_le_bytes().to_vec(),
        );
        push(
            self.gobjects + u64::from(o.num_chunks),
            (num_chunks as u32).to_le_bytes().to_vec(),
        );
        SynthImage {
            writes,
            gobjects: self.gobjects,
            names: self.names_ptrs,
        }
    }

    fn alloc(&mut self, size: u64, align: u64) -> u64 {
        let addr = (self.next + align - 1) & !(align - 1);
        assert!(addr + size <= self.end, "arena sintética esgotada");
        self.next = addr + size;
        addr
    }

    /// Tamanho do cabeçalho de `UObject`.
    fn header_size(&self) -> u32 {
        let u = self.layout.uobject;
        round8(
            (u.flags + 4)
                .max(u.index + 4)
                .max(u.class + 8)
                .max(u.name + 8)
                .max(u.outer + 8),
        )
    }

    /// Tamanho de um objeto de classe (`UStruct` incluso).
    fn class_size(&self) -> u32 {
        let s = self.layout.ustruct;
        round8(
            self.header_size()
                .max(s.super_struct + 8)
                .max(s.child_properties + 8)
                .max(s.properties_size + 4),
        )
    }

    /// Tamanho de um `FField` com os campos de propriedade e de bool.
    fn field_size(&self) -> u32 {
        let (f, p, b) = (self.layout.ffield, self.layout.fprop, self.layout.fbool);
        round8(
            (f.class_private + 8)
                .max(f.next + 8)
                .max(f.name + 8)
                .max(p.element_size + 4)
                .max(p.offset + 4)
                .max(b.field_size + 1)
                .max(b.byte_offset + 1)
                .max(b.byte_mask + 1)
                .max(b.field_mask + 1)
                .max(self.layout.fstruct.struct_ptr + 8),
        )
    }

    fn write_u32(&mut self, addr: u64, v: u32) {
        self.writes.push((addr, v.to_le_bytes().to_vec()));
    }

    fn write_u64(&mut self, addr: u64, v: u64) {
        self.writes.push((addr, v.to_le_bytes().to_vec()));
    }

    fn write_fname(&mut self, addr: u64, name: &str) {
        let id = self.intern_name(name);
        self.write_u32(addr, id);
        self.write_u32(addr + 4, 0);
    }

    /// Zera o objeto em `addr` e preenche o cabeçalho; registra o objeto na tabela.
    fn init_object(&mut self, addr: u64, class: u64, size: u32, name: &str, outer: u64) {
        let index = self.objects.len() as u32;
        self.objects.push(addr);
        let u = self.layout.uobject;
        self.writes.push((addr, vec![0u8; size as usize]));
        self.write_u32(addr + u64::from(u.index), index);
        self.write_u64(addr + u64::from(u.class), class);
        self.write_fname(addr + u64::from(u.name), name);
        self.write_u64(addr + u64::from(u.outer), outer);
    }

    /// Preenche a parte `UStruct` de um objeto de classe.
    fn init_struct(&mut self, addr: u64, super_class: Option<u64>, first_field: u64, props: u32) {
        let s = self.layout.ustruct;
        self.write_u64(addr + u64::from(s.super_struct), super_class.unwrap_or(0));
        self.write_u64(addr + u64::from(s.child_properties), first_field);
        self.write_u32(addr + u64::from(s.properties_size), props);
    }

    /// `FFieldClass` do tipo `kind` (nome do tipo em `ffield.class_name`), um por tipo.
    fn field_class(&mut self, kind: &'static str) -> u64 {
        if let Some(&addr) = self.field_classes.get(kind) {
            return addr;
        }
        let name_at = self.layout.ffield.class_name;
        let size = round8(0x20.max(name_at + 8));
        let addr = self.alloc(u64::from(size), 16);
        self.writes.push((addr, vec![0u8; size as usize]));
        self.write_fname(addr + u64::from(name_at), kind);
        self.field_classes.insert(kind, addr);
        addr
    }

    /// Monta a lista encadeada de `FField` de `owner`; devolve o primeiro (ou 0 sem campos).
    fn build_fields(&mut self, owner: u64, fields: &[SynthField]) -> u64 {
        let size = u64::from(self.field_size());
        let addrs: Vec<u64> = fields.iter().map(|_| self.alloc(size, 16)).collect();
        for (field, &addr) in fields.iter().zip(&addrs) {
            self.field_addrs.insert((owner, field.name), addr);
        }
        let (f, p, b) = (self.layout.ffield, self.layout.fprop, self.layout.fbool);
        for (i, field) in fields.iter().enumerate() {
            let addr = addrs[i];
            self.writes.push((addr, vec![0u8; size as usize]));
            let class = self.field_class(field.kind);
            self.write_u64(addr + u64::from(f.class_private), class);
            self.write_u64(
                addr + u64::from(f.next),
                addrs.get(i + 1).copied().unwrap_or(0),
            );
            self.write_fname(addr + u64::from(f.name), field.name);
            self.write_u32(addr + u64::from(p.element_size), field.size);
            self.write_u32(addr + u64::from(p.offset), field.offset);
            if let Some((byte_offset, mask)) = field.bool_mask {
                let w =
                    |s: &mut Self, off: u32, v: u8| s.writes.push((addr + u64::from(off), vec![v]));
                w(self, b.field_size, 1);
                w(self, b.byte_offset, byte_offset);
                w(self, b.byte_mask, mask);
                w(self, b.field_mask, mask);
            }
        }
        addrs.first().copied().unwrap_or(0)
    }
}
