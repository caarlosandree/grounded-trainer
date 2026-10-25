//! Processos: valor de `ptrace_scope` e (nas próximas tarefas) listagem e `cmdline`.

/// Valor atual de `kernel.yama.ptrace_scope`, quando conhecido.
///
/// Esqueleto: a Tarefa 6 lê o valor real de `/proc/sys/kernel/yama/ptrace_scope`.
pub fn ptrace_scope() -> Option<u8> {
    None
}
