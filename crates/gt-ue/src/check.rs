//! Conferência barata do layout (spec 005, 6.2): feita na busca de padrões, sem varrer objetos.

use gt_core::mem::ReadMem;

use crate::layout::UeLayout;
use crate::names::Names;
use crate::objects::ObjectArray;
use crate::read::read_u32;

/// Itens do começo da tabela que entram na amostra.
const SAMPLE_HEAD: u32 = 16;
/// Itens espalhados por igual até `NumElements` que entram na amostra.
const SAMPLE_SPREAD: u32 = 16;

/// Os três itens baratos: o nome 0 é `None`; as contagens são coerentes; numa amostra de itens,
/// `item[o.InternalIndex].Object == o`. O `Err` traz o detalhe para o log; quem mostra ao usuário
/// usa sempre `reflexão não reconhecida`.
pub fn cheap_check<M: ReadMem + ?Sized>(
    mem: &M,
    layout: &UeLayout,
    gobjects: u64,
    names_blocks: u64,
) -> Result<(), String> {
    let names = Names::new(mem, names_blocks).map_err(|e| format!("tabela de nomes: {e}"))?;
    let first = names
        .name(0)
        .map_err(|e| format!("nome de índice 0: {e}"))?;
    if first != "None" {
        return Err(format!("o nome de índice 0 é {first:?}, esperado \"None\""));
    }

    let array =
        ObjectArray::read(mem, layout, gobjects).map_err(|e| format!("GUObjectArray: {e}"))?;
    if array.num_elements == 0 {
        return Err("NumElements é 0".to_string());
    }
    if array.num_elements > array.max_elements {
        return Err(format!(
            "NumElements ({}) maior que MaxElements ({})",
            array.num_elements, array.max_elements
        ));
    }
    if array.num_chunks > array.max_chunks {
        return Err(format!(
            "NumChunks ({}) maior que MaxChunks ({})",
            array.num_chunks, array.max_chunks
        ));
    }
    let capacity = u64::from(array.num_chunks) * u64::from(layout.objects.items_per_chunk);
    if capacity < u64::from(array.num_elements) {
        return Err(format!(
            "NumChunks ({}) × {} itens não cobre NumElements ({})",
            array.num_chunks, layout.objects.items_per_chunk, array.num_elements
        ));
    }

    let mut verified = 0u32;
    for index in sample_indices(array.num_elements) {
        let object = array
            .object_at(mem, layout, index)
            .map_err(|e| format!("item {index}: {e}"))?;
        // Item vazio é legítimo (objeto liberado).
        let Some(object) = object else { continue };
        let internal = read_u32(mem, object + u64::from(layout.uobject.index))
            .map_err(|e| format!("objeto do item {index} em {object:#x}: {e}"))?;
        if internal != index {
            return Err(format!(
                "o objeto do item {index} ({object:#x}) tem InternalIndex {internal}"
            ));
        }
        verified += 1;
    }
    if verified == 0 {
        return Err("nenhum item da amostra tem objeto".to_string());
    }
    Ok(())
}

/// Índices da amostra: os primeiros e mais alguns espalhados por igual até `num_elements`.
fn sample_indices(num_elements: u32) -> Vec<u32> {
    let mut out: Vec<u32> = (0..SAMPLE_HEAD.min(num_elements)).collect();
    for i in 0..SAMPLE_SPREAD {
        let idx =
            (u64::from(num_elements) * u64::from(i + 1) / u64::from(SAMPLE_SPREAD + 1)) as u32;
        if idx < num_elements && !out.contains(&idx) {
            out.push(idx);
        }
    }
    out
}
