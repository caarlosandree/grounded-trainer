//! Layout das estruturas de reflexão do motor (offsets e tamanhos), vindo da configuração.
//!
//! Não há `Default`: o código de produção nunca assume os valores de um build. Eles vêm do `[ue]`
//! do `generic.toml`. Com a feature `synth`, [`UeLayout::observed`] devolve os valores do build
//! documentado em `RESEARCH.md`, só para testes.

/// `GUObjectArray` e a tabela de itens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectsLayout {
    /// Offset do ponteiro para a lista de blocos.
    pub chunks: u32,
    pub max_elements: u32,
    pub num_elements: u32,
    pub max_chunks: u32,
    pub num_chunks: u32,
    /// Itens por bloco.
    pub items_per_chunk: u32,
    /// Tamanho de cada item, em bytes.
    pub item_size: u32,
    /// Offset do ponteiro do objeto dentro do item.
    pub item_object: u32,
}

/// Cabeçalho de `UObject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UObjectLayout {
    pub flags: u32,
    pub index: u32,
    pub class: u32,
    pub name: u32,
    pub outer: u32,
    /// Bit de `ObjectFlags` que marca o CDO.
    pub cdo_flag: u32,
}

/// `UStruct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UStructLayout {
    pub super_struct: u32,
    pub child_properties: u32,
    pub properties_size: u32,
}

/// `FField`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FFieldLayout {
    pub class_private: u32,
    /// Offset do nome do tipo (`FName`) dentro da `FFieldClass`.
    pub class_name: u32,
    pub next: u32,
    pub name: u32,
}

/// `FProperty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FPropLayout {
    pub element_size: u32,
    pub offset: u32,
}

/// `FBoolProperty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FBoolLayout {
    pub field_size: u32,
    pub byte_offset: u32,
    pub byte_mask: u32,
    pub field_mask: u32,
}

/// `FStructProperty` e `FObjectPropertyBase`: o primeiro ponteiro depois do `FProperty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FStructLayout {
    /// Offset do ponteiro para a `UScriptStruct` de uma `StructProperty`, que também é o da
    /// classe apontada (`PropertyClass`) de um campo de objeto.
    pub struct_ptr: u32,
}

/// Layout completo da reflexão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UeLayout {
    pub objects: ObjectsLayout,
    pub uobject: UObjectLayout,
    pub ustruct: UStructLayout,
    pub ffield: FFieldLayout,
    pub fprop: FPropLayout,
    pub fbool: FBoolLayout,
    pub fstruct: FStructLayout,
}

#[cfg(feature = "synth")]
impl UeLayout {
    /// Valores do build atual (spec 005, 6.2). Só para testes.
    pub fn observed() -> Self {
        Self {
            objects: ObjectsLayout {
                chunks: 0x0,
                max_elements: 0x10,
                num_elements: 0x14,
                max_chunks: 0x18,
                num_chunks: 0x1c,
                items_per_chunk: 65536,
                item_size: 24,
                item_object: 0x8,
            },
            uobject: UObjectLayout {
                flags: 0x8,
                index: 0xc,
                class: 0x10,
                name: 0x18,
                outer: 0x20,
                cdo_flag: 0x10,
            },
            ustruct: UStructLayout {
                super_struct: 0x40,
                child_properties: 0x50,
                properties_size: 0x58,
            },
            ffield: FFieldLayout {
                class_private: 0x8,
                class_name: 0x0,
                next: 0x18,
                name: 0x20,
            },
            fprop: FPropLayout {
                element_size: 0x34,
                offset: 0x44,
            },
            fbool: FBoolLayout {
                field_size: 0x70,
                byte_offset: 0x71,
                byte_mask: 0x72,
                field_mask: 0x73,
            },
            fstruct: FStructLayout { struct_ptr: 0x70 },
        }
    }
}
