#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! Gancho de pânico (spec 003, 6.8): na thread principal restaura o terminal antes do gancho
//! anterior; em outra thread só marca `elsewhere`. Um teste só: o gancho é global ao processo.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use gt_tui::term::{RestoreOnce, install_panic_hook};

#[test]
fn panic_hook_restores_on_main_and_flags_elsewhere() {
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    // O "gancho anterior" é o do teste: grava a ordem em vez de imprimir.
    let prev_order = Arc::clone(&order);
    panic::set_hook(Box::new(move |_| {
        prev_order.lock().unwrap().push("previous")
    }));

    // A marca é a mesma do `main`: o gancho restaura pela marca, uma vez só.
    let once = Arc::new(RestoreOnce::new());
    let hook_order = Arc::clone(&order);
    let hook_once = Arc::clone(&once);
    let elsewhere = Arc::new(AtomicBool::new(false));
    install_panic_hook(
        thread::current().id(),
        Arc::new(move || {
            let _ = hook_once.run(|| {
                hook_order.lock().unwrap().push("restore");
                Ok(())
            });
        }),
        Arc::clone(&elsewhere),
    );

    // Pânico na thread principal (a do teste): restaura e depois chama o anterior.
    let res = panic::catch_unwind(AssertUnwindSafe(|| panic!("pânico na principal")));
    assert!(res.is_err());
    assert_eq!(*order.lock().unwrap(), ["restore", "previous"]);
    assert!(once.done());
    assert!(!elsewhere.load(Ordering::SeqCst));

    // Pânico em outra thread: só marca `elsewhere`; nem restaura nem chama o anterior.
    order.lock().unwrap().clear();
    let joined = thread::spawn(|| panic!("pânico na busca")).join();
    assert!(joined.is_err());
    assert!(elsewhere.load(Ordering::SeqCst));
    assert!(order.lock().unwrap().is_empty());

    let _ = panic::take_hook();
}
