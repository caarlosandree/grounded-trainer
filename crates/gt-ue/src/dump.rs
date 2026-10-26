//! Descrição de uma classe para `gt ue class` (spec 005, 6.8): campos da classe e das superclasses.

use gt_core::mem::ReadMem;

use crate::error::UeError;
use crate::index::{Field, ReflectionIndex};

/// Campos de uma classe, com o caminho `Pacote.Nome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDump {
    pub path: String,
    /// Da classe e das superclasses, a classe primeiro.
    pub fields: Vec<Field>,
}

/// Resultado de uma consulta por classe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassQuery {
    Found(ClassDump),
    /// A consulta só tinha o nome e 2+ classes o têm: os caminhos `Pacote.Nome`, sem escolher.
    Ambiguous(Vec<String>),
}

/// Descreve a classe pedida. `query` é `Pacote.Nome` (ex.: `/Script/Maine.SurvivalCharacter`) ou
/// só `Nome`. Classe inexistente: `NotFound`.
pub fn describe_class<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    query: &str,
) -> Result<ClassQuery, UeError> {
    let (pkg, class) = match query.rsplit_once('.') {
        Some((pkg, name)) => (pkg.to_string(), index.find_class(pkg, name)?),
        None => {
            let mut hits = index.classes_named(query);
            match hits.len() {
                0 => return Err(UeError::NotFound(format!("classe {query}"))),
                1 => hits.remove(0),
                _ => {
                    return Ok(ClassQuery::Ambiguous(
                        hits.into_iter()
                            .map(|(pkg, _)| format!("{pkg}.{query}"))
                            .collect(),
                    ));
                }
            }
        }
    };
    let name = query.rsplit_once('.').map_or(query, |(_, n)| n);
    Ok(ClassQuery::Found(ClassDump {
        path: format!("{pkg}.{name}"),
        fields: index.fields_of(mem, class)?,
    }))
}
