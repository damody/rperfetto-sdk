//! Function entry/exit markers.
//!
//! [`func_begin`]/[`func_end`] (or [`func_scope`] / [`trace_fn!`], or the C
//! `rpf_func_begin`/`rpf_func_end` called from `-finstrument-functions`
//! hooks) bracket a function with a slice on the calling thread's track.
//!
//! The app reads no CPU counters itself, so it needs no root and no perf
//! permissions. Per-function MIPS is computed from the *system* trace: record
//! with the system backend together with `linux.perf` per-CPU instruction
//! counters and `sched_switch`, then run `sql/function_mips.sql`, which
//! spreads each CPU's instruction count over the threads scheduled on it and
//! attributes it to the function slices (same method as the FPSGO
//! "Game Thread MIPS" metric).

use crate::category::Category;
use crate::event::Phase;
use crate::writer::{emit_with_sinks, EventSpec};
use std::cell::RefCell;

/// Conventional category for function markers (what `sql/function_mips.sql`
/// selects by default).
pub static FN_CATEGORY: Category = Category::with_description("fn", "Function entry/exit markers");

struct Frame {
    cat: &'static Category,
    sinks: u8,
}

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

fn spec<'a>(cat: &'static Category, phase: Phase, name: Option<&'a str>) -> EventSpec<'a> {
    EventSpec { cat, phase, track_uuid: None, name, args: &[], counter: None, flows: &[], terminating_flows: &[] }
}

/// Function entry: begins a slice named `name` on the calling thread.
#[inline]
pub fn func_begin(cat: &'static Category, name: &str) {
    let sinks = cat.sinks();
    if sinks != 0 {
        emit_with_sinks(sinks, &spec(cat, Phase::Begin, Some(name)));
    }
    // Pushed even when disabled so that every func_end pairs with its begin
    // and goes to the same backends.
    let _ = STACK.try_with(|s| s.borrow_mut().push(Frame { cat, sinks }));
}

/// Function exit: ends the innermost slice started by [`func_begin`] on this
/// thread. Unmatched calls are ignored.
#[inline]
pub fn func_end() {
    let Ok(Some(f)) = STACK.try_with(|s| s.borrow_mut().pop()) else { return };
    if f.sinks != 0 {
        emit_with_sinks(f.sinks, &spec(f.cat, Phase::End, None));
    }
}

/// Current nesting depth of [`func_begin`] on this thread.
pub fn func_depth() -> usize {
    STACK.try_with(|s| s.borrow().len()).unwrap_or(0)
}

/// Guard returned by [`func_scope`]; calls [`func_end`] on drop.
#[must_use = "the function slice ends when the guard is dropped"]
pub struct FuncGuard(());

impl Drop for FuncGuard {
    #[inline]
    fn drop(&mut self) {
        func_end();
    }
}

/// [`func_begin`] + a guard that calls [`func_end`].
#[inline]
pub fn func_scope(cat: &'static Category, name: &str) -> FuncGuard {
    func_begin(cat, name);
    FuncGuard(())
}

/// Marks the enclosing function until the end of the current block.
/// `trace_fn!();` uses category `fn` and names the slice after the function
/// (`module::func`); `trace_fn!(CAT)` picks the category; `trace_fn!(CAT,
/// "name")` also sets the name.
#[macro_export]
macro_rules! trace_fn {
    () => {
        $crate::trace_fn!($crate::func::FN_CATEGORY)
    };
    ($cat:expr) => {
        let __rpf_fn_guard = $crate::func::func_scope(&$cat, {
            fn __rpf_f() {}
            let n = ::std::any::type_name_of_val(&__rpf_f);
            &n[..n.len() - "::__rpf_f".len()]
        });
    };
    ($cat:expr, $name:expr) => {
        let __rpf_fn_guard = $crate::func::func_scope(&$cat, $name);
    };
}
