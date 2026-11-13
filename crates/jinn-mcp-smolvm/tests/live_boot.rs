//! Live boot test: boots a real machine through the production boot
//! sequence and runs one command in the guest.
//!
//! Compiled by default (so a signature drift breaks `just check`) but the
//! body exits early unless the `live-vm` feature is on AND the host can
//! run local machines — the workspace suite must stay green on
//! hypervisor-less hosts. Run it with:
//!
//! ```sh
//! cargo test -p jinn-mcp-smolvm --features live-vm live_boot -- --ignored
//! ```
#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use jinn_mcp_smolvm::backend::SmolvmBackend;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

/// The live boot: compile-gated by the feature, runtime-gated by the host.
#[rstest::rstest]
#[test]
#[cfg_attr(
    not(feature = "live-vm"),
    ignore = "requires --features live-vm and a hypervisor"
)]
fn live_boot_execs_uname_in_a_real_guest() {
    // Given a temp directory to mount and the shipped manifest shape.
    let dir = jinn_testutil::tempdir();
    let cwd = MarkedPathBuf::<Absolute>::new(dir.path.as_path()).expect("temp root is absolute");
    let manifest = jinn_mcp_smolvm::manifest::MachineManifest {
        image: "alpine:latest".to_owned(),
        cpus: 1,
        memory: 512,
        net: true,
        init: vec!["true".to_owned()],
    };

    // When booting through the production sequence.
    let backend = {
        // A hypervisor-less host skips rather than fails: the suite must
        // stay green everywhere.
        if smolmachines::local_availability().is_err() {
            #[expect(
                clippy::print_stderr,
                reason = "the skip note is the test's only output channel"
            )]
            {
                eprintln!("skipping: this host cannot run local machines");
            }
            return;
        }
        jinn_mcp_smolvm::backend::borrow_engine_assets().expect("configure engine assets");
        jinn_mcp_smolvm::backend::SmolMachineBackend::boot(&cwd.as_marked_path(), &manifest)
            .expect("boot a real machine")
    };

    // Then a command runs in the guest and reports its kernel.
    let outcome = backend
        .exec(
            vec!["uname".to_owned(), "-r".to_owned()],
            jinn_mcp_smolvm::backend::ExecOptions::default(),
        )
        .expect("exec uname");
    assert_eq!(outcome.exit_code, 0);
    assert!(!outcome.stdout.trim().is_empty(), "kernel version printed");

    // And deleting the machine (the ephemeral lifetime) succeeds.
    drop(backend);
}
