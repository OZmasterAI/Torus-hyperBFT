//! R01b: mark a `catch_unwind` whose panic is RECOVERED (the caller carries on
//! with a fallback), so the node's fail-stop panic hook does not end the
//! process for a panic that never kills its thread.
//!
//! The hook (`torus-node`) exits 70 on a panic on a consensus / execution
//! thread. A panic hook runs before unwinding and cannot see whether a
//! `catch_unwind` up the stack will catch it, so a recovering site must say so
//! by catching through [`catch_recoverable`]. Sites whose caught panic is
//! itself a fail-stop (the execution loop, the flush worker, market workers)
//! keep plain `catch_unwind`: exiting from the hook is the same outcome.

/// Run `f` under `catch_unwind`, marking this thread as inside a recovering
/// scope for the duration (see the module docs).
pub fn catch_recoverable<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    DEPTH.with(|d| d.set(d.get() + 1));
    // `catch_unwind` never unwinds, so the decrement always runs.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    DEPTH.with(|d| d.set(d.get() - 1));
    result
}

/// True while this thread runs inside [`catch_recoverable`].
pub fn in_recoverable_scope() -> bool {
    DEPTH.with(|d| d.get() > 0)
}

thread_local! {
    /// Nesting depth of [`catch_recoverable`] on this thread.
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_set_only_inside_and_restored_after_a_panic() {
        assert!(!in_recoverable_scope());
        let inner = catch_recoverable(|| {
            let nested = catch_recoverable(in_recoverable_scope).unwrap();
            (in_recoverable_scope(), nested)
        })
        .unwrap();
        assert_eq!(inner, (true, true));
        assert!(!in_recoverable_scope());
        let seen = std::cell::Cell::new(false);
        let r = catch_recoverable(|| {
            seen.set(in_recoverable_scope());
            panic!("recovered");
        });
        assert!(r.is_err());
        assert!(seen.get());
        assert!(
            !in_recoverable_scope(),
            "depth must be restored after an unwind"
        );
    }
}
