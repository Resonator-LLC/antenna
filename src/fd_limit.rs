// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Clamp `RLIMIT_NOFILE` before opening a disk-backed Oxigraph store.
//!
//! Oxigraph 0.4.11 panics on macOS with `TryFromIntError` because
//! `storage::rocksdb_wrapper::db_options` does
//! `(available_fd - 48).try_into::<c_int>().unwrap()` where `available_fd` is
//! a `libc::rlim_t` (u64). macOS reports `RLIMIT_NOFILE` hard limits up to
//! `i64::MAX` (and `RLIM_INFINITY == u64::MAX`), which overflow `i32`.
//! Oxigraph v0.5.x fixed this as `.unwrap_or(8192)` — until we upgrade
//! (breaking API), we side-step by lowering our own soft limit so oxigraph's
//! cast fits.

#[cfg(unix)]
const SAFE_SOFT_LIMIT: libc::rlim_t = 10_240;

#[cfg(unix)]
pub fn clamp_for_rocksdb() {
    // SAFETY: `rl` is a stack-owned POD `libc::rlimit` passed by mutable
    // pointer to `getrlimit`, which only writes into the struct. `setrlimit`
    // reads from a stack-owned `libc::rlimit`. Both syscalls are thread-safe
    // and take no Rust-side references beyond the locals in this block.
    unsafe {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) != 0 {
            tracing::warn!(
                err = %std::io::Error::last_os_error(),
                "getrlimit(RLIMIT_NOFILE) failed; skipping fd clamp"
            );
            return;
        }
        let Some(new_soft) = desired_soft_limit(rl.rlim_cur, rl.rlim_max) else {
            return;
        };
        let new_rl = libc::rlimit {
            rlim_cur: new_soft,
            rlim_max: rl.rlim_max,
        };
        if libc::setrlimit(libc::RLIMIT_NOFILE, &new_rl) != 0 {
            tracing::warn!(
                err = %std::io::Error::last_os_error(),
                prev_soft = rl.rlim_cur,
                attempted = new_soft,
                "setrlimit(RLIMIT_NOFILE) failed; oxigraph may still panic"
            );
        } else {
            tracing::debug!(
                from = rl.rlim_cur,
                to = new_soft,
                "clamped RLIMIT_NOFILE for oxigraph 0.4 compat"
            );
        }
    }
}

#[cfg(not(unix))]
pub fn clamp_for_rocksdb() {}

/// Compute the soft limit we want to apply, or `None` if the current limit is
/// already safe. The effective usable limit is `min(rlim_cur, rlim_max)`; if
/// that is already ≤ `SAFE_SOFT_LIMIT` we leave everything alone.
#[cfg(unix)]
pub(crate) fn desired_soft_limit(
    rlim_cur: libc::rlim_t,
    rlim_max: libc::rlim_t,
) -> Option<libc::rlim_t> {
    let effective = rlim_cur.min(rlim_max);
    if effective <= SAFE_SOFT_LIMIT {
        return None;
    }
    Some(SAFE_SOFT_LIMIT.min(rlim_max))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn zero_effective_limit_is_left_alone() {
        // available_fd = 0 — below SAFE_SOFT_LIMIT, nothing to do.
        assert_eq!(desired_soft_limit(0, 0), None);
        assert_eq!(desired_soft_limit(0, libc::rlim_t::MAX), None);
    }

    #[test]
    fn below_threshold_is_left_alone() {
        // available_fd < 48 (oxigraph subtracts 48; doesn't matter here — we
        // only clamp when we're *above* SAFE_SOFT_LIMIT).
        assert_eq!(desired_soft_limit(10, 4096), None);
        assert_eq!(desired_soft_limit(47, libc::rlim_t::MAX), None);
        assert_eq!(desired_soft_limit(48, libc::rlim_t::MAX), None);
    }

    #[test]
    fn already_safe_soft_limit_is_left_alone() {
        assert_eq!(desired_soft_limit(1024, 4096), None);
        assert_eq!(desired_soft_limit(SAFE_SOFT_LIMIT, SAFE_SOFT_LIMIT), None);
        assert_eq!(desired_soft_limit(512, libc::rlim_t::MAX), None);
    }

    #[test]
    fn i32_max_soft_limit_is_clamped() {
        // available_fd = i32::MAX — above SAFE_SOFT_LIMIT so we clamp.
        // (i32::MAX - 48) fits in i32, but oxigraph 0.4.11 still panics because
        // the *cast* is performed via rlim_t→i32 on the full value in some
        // code paths. Clamp to the safe sentinel either way.
        assert_eq!(
            desired_soft_limit(i32::MAX as libc::rlim_t, libc::rlim_t::MAX),
            Some(SAFE_SOFT_LIMIT)
        );
    }

    #[test]
    fn u64_max_soft_limit_is_clamped() {
        // RLIM_INFINITY on macOS — the headline failure case.
        assert_eq!(
            desired_soft_limit(libc::rlim_t::MAX, libc::rlim_t::MAX),
            Some(SAFE_SOFT_LIMIT)
        );
        assert_eq!(
            desired_soft_limit(1_048_576, i64::MAX as libc::rlim_t),
            Some(SAFE_SOFT_LIMIT)
        );
    }

    #[test]
    fn clamp_respects_hard_limit() {
        // If the hard limit is below SAFE_SOFT_LIMIT we can't raise past it.
        // Effective is hard (5000), which is already below SAFE_SOFT_LIMIT,
        // so we return None.
        assert_eq!(desired_soft_limit(libc::rlim_t::MAX, 5_000), None);
    }

    #[test]
    fn cast_to_i32_succeeds_for_clamped_value() {
        // The whole point: oxigraph's `(fd - 48).try_into::<i32>()` must not
        // overflow after we clamp.
        let after = SAFE_SOFT_LIMIT - 48;
        let as_i32: Result<i32, _> = after.try_into();
        assert!(as_i32.is_ok());
    }

    #[test]
    fn clamp_for_rocksdb_does_not_panic() {
        clamp_for_rocksdb();
    }
}
