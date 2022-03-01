//! Run a function or closure in a pseudoterminal.
//!
//! This library works by forking the current process into a pseudoterminal,
//! running the closure, and then reporting the resulting status code.
//!
//! - `0`: success, returned as `Ok(())`
//! - Anything else: failure, returned as [`Err(Error)`]
//!
//! See [`run_in_pty`] for usage.

#![deny(clippy::all)]
#![deny(clippy::correctness)]
#![deny(clippy::style)]
#![deny(clippy::complexity)]
#![deny(clippy::perf)]
#![deny(clippy::pedantic)]
#![deny(rustdoc::missing_doc_code_examples)]
#![deny(
    absolute_paths_not_starting_with_crate,
    anonymous_parameters,
    bad_style,
    const_err,
    dead_code,
    keyword_idents,
    improper_ctypes,
    macro_use_extern_crate,
    meta_variable_misuse, // May have false positives
    missing_abi,
    missing_debug_implementations, // can affect compile time/code size
    missing_docs,
    no_mangle_generic_items,
    non_shorthand_field_patterns,
    noop_method_call,
    overflowing_literals,
    path_statements,
    patterns_in_fns_without_body,
    pointer_structural_match,
    private_in_public,
    semicolon_in_expressions_from_macros,
    single_use_lifetimes,
    trivial_casts,
    trivial_numeric_casts,
    unaligned_references,
    unconditional_recursion,
    unreachable_pub,
    unused,
    unused_allocation,
    unused_comparisons,
    unused_extern_crates,
    unused_import_braces,
    unused_lifetimes,
    unused_parens,
    unused_qualifications,
    variant_size_differences,
    while_true
)]

use nix::errno::Errno;
use nix::pty::{forkpty, ForkptyResult};
use nix::sys::signal::Signal;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::ForkResult;
use thiserror::Error;

/// The possible errors encountered when calling [`run_in_pty`].
#[derive(Debug, Error)]
pub enum Error {
    /// Error when calling [`forkpty`].
    ///
    /// See `man 3 forkpty` and [`forkpty`] for more.
    #[error("failed to fork: {0}")]
    Fork(#[source] Errno),
    /// Error while waiting for the child process to finish (i.e., calling [`waitpid`]).
    ///
    /// See `man 3 waitpid` and [`waitpid`] for more.
    #[error("failed to wait on child: {0}")]
    Wait(#[source] Errno),
    /// Returned if the child process was killed by some OS [`Signal`].
    #[error("child process killed by signal {0}")]
    KilledBySignal(Signal),
    /// Returned if the child process exited with a non-zero (i.e., with a failure) status code.
    #[error("child process exited with code {0}")]
    NonZeroExitCode(i32),
}

/// Run a closure in a forked pseudoterminal process.
///
/// # Limitations
///
/// - No method of reading/writing the child's stdout/stdin
/// - Will not return until the child process exits or is killed.
///   - That is, a stopped process that may be resumed will leave this function
///     waiting until the process continues and then exits or is killed.
///
/// # Errors
///
/// See [`Error`] for the possible error cases.
///
/// # Safety
///
/// See [`nix::pty::forkpty`]. In short, do not use this in multithreaded applications unless you
/// know what you are doing.
pub unsafe fn run_in_pty<F>(func: F) -> Result<(), Error>
where
    F: FnOnce() -> Result<(), i32>,
{
    let ForkptyResult {
        master: _,
        fork_result,
    } = forkpty(None, None).map_err(Error::Fork)?;

    match fork_result {
        ForkResult::Child => {
            let exit_code = match func() {
                Ok(()) => 0,
                Err(code) => code,
            };

            std::process::exit(exit_code);
        }
        ForkResult::Parent { child } => {
            loop {
                let result = waitpid(child, None).map_err(Error::Wait)?;
                #[allow(clippy::match_same_arms)]
                match result {
                    WaitStatus::Exited(_, status_code) => {
                        if status_code == 0 {
                            break Ok(());
                        }

                        break Err(Error::NonZeroExitCode(status_code));
                    }
                    WaitStatus::Signaled(_, signal, _generated_core_dump) => {
                        break Err(Error::KilledBySignal(signal));
                    }
                    // grcov: ignore-start
                    // Stopped/Continued should not happen because the appropriate flags were not
                    // passed to `waitpid`. If they do get returned, we don't care because we are
                    // still waiting for the process to exit.
                    WaitStatus::Stopped(..) | WaitStatus::Continued(..) => {}
                    // This function does not care about linux-specific ptrace statuses.
                    #[cfg(any(target_os = "linux", target_os = "android"))]
                    WaitStatus::PtraceEvent(..) | WaitStatus::PtraceSyscall(..) => {}
                    // StillAlive also requires an option flag to be passed and gives us no useful information.
                    WaitStatus::StillAlive => {}
                    // grcov: ignore-end
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[allow(clippy::unnecessary_wraps)]
    fn successful_func() -> Result<(), i32> {
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    fn assert_exit_code(exit_code: i32, result: Result<(), Error>) {
        if exit_code == 0 {
            assert!(
                matches!(result, Ok(())),
                "expected success, got {:?}",
                result
            );
        } else {
            assert!(
                matches!(result, Err(Error::NonZeroExitCode(code)) if code == exit_code),
                "expected child to exit with code {}, instead got {:?}",
                exit_code,
                result
            );
        }
    }

    #[test]
    fn test_basic_successful() {
        unsafe {
            run_in_pty(|| Ok(())).expect("running successful closure should not error");
        }
        unsafe {
            run_in_pty(successful_func).expect("running successful function should not error");
        }
    }

    #[test]
    fn test_basic_erroring_closure() {
        // From the notes on [`std::process::ExitStatus::code()`]:
        //
        // > Note that on Unix the exit status is truncated to 8 bits, and that values that didn’t come from a program’s call
        // > to exit may be invented by the runtime system (often, for example, 255, 254, 127 or 126).
        //
        // Because of this, the test uses i8 instead of i32, despite i32 being the type returned.
        for code in 1..i8::MAX {
            let result = unsafe { run_in_pty(move || Err(code.into())) };
            assert_exit_code(code.into(), result);
        }
    }

    #[test]
    fn test_delayed_closure() {
        unsafe {
            assert_exit_code(
                0,
                run_in_pty(|| {
                    std::thread::sleep(Duration::from_secs(3));
                    Ok(())
                }),
            );

            assert_exit_code(
                9,
                run_in_pty(|| {
                    std::thread::sleep(Duration::from_secs(3));
                    Err(9)
                }),
            );
        }
    }

    #[test]
    fn test_closure_capturing_moved_values() {
        unsafe {
            let success = Ok(());
            let error_code = 23;
            let error = Err(error_code);

            assert_exit_code(0, run_in_pty(move || success));
            assert_exit_code(error_code, run_in_pty(move || error));
        }
    }

    #[test]
    fn test_closure_allocating_memory() {
        for _ in 0..10 {
            unsafe {
                assert_exit_code(
                    0,
                    run_in_pty(|| {
                        const CAPACITY: usize = 1024 * 1024 * 512;
                        let mut v = Vec::with_capacity(CAPACITY);
                        v.resize(CAPACITY, 100_u8);
                        assert_eq!(v.len(), CAPACITY);
                        Ok(())
                    }),
                );
            }
        }
    }

    #[test]
    fn test_file_access() {
        use std::io::Write;
        unsafe {
            assert_exit_code(
                0,
                run_in_pty(|| {
                    let mut random_file =
                        tempfile::tempfile().expect("creating temp file should not fail");
                    random_file
                        .write_all(&vec![0; 0x4000_0000])
                        .expect("writing to temp file should not fail");
                    Ok(())
                }),
            );
        }
    }

    #[test]
    fn test_kill_with_signal() {
        use nix::sys::signal;
        use nix::unistd::Pid;
        unsafe {
            let result = run_in_pty(|| {
                signal::kill(Pid::this(), Signal::SIGKILL).expect("sending signal should succeed");
                std::thread::sleep(Duration::from_secs(3));
                Ok(())
            });

            assert!(
                matches!(result, Err(Error::KilledBySignal(Signal::SIGKILL))),
                "expected child process to kill itself with SIGKILL"
            );
        }
    }
}
