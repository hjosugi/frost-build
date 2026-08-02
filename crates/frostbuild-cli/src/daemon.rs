//! The build daemon's lifecycle: starting one, and `frost daemon`.
//!
//! Building *through* a daemon is part of the build path and lives in
//! [`crate::build`]; what is here is spawning, addressing and stopping it.

use std::path::Path;
// Only the Windows `spawn_daemon` below needs these, and a host build never
// compiles it — so they are gated rather than plain, or `cargo fix` deletes
// them as unused on Linux and Windows CI is the first thing to notice.
#[cfg(windows)]
use std::ffi::OsStr;

#[cfg(windows)]
use anyhow::Context;
use anyhow::{bail, Result};

use crate::build::is_protocol_mismatch;
use crate::cli::DaemonCmd;

#[cfg(not(windows))]
fn spawn_daemon(executable: &Path, root: &Path) -> Result<()> {
    std::process::Command::new(executable)
        .arg("-C")
        .arg(root)
        .args(["daemon", "serve"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(windows)]
fn spawn_daemon(executable: &Path, root: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
        PROCESS_INFORMATION, STARTUPINFOW,
    };

    // CreateProcessW accepts one mutable command-line buffer. Always quoting
    // each argument keeps whitespace, trailing backslashes and embedded quotes
    // round-trippable through the Windows C argv parser.
    let mut command_line = Vec::<u16>::new();
    for argument in [
        executable.as_os_str(),
        OsStr::new("-C"),
        root.as_os_str(),
        OsStr::new("daemon"),
        OsStr::new("serve"),
    ] {
        if !command_line.is_empty() {
            command_line.push(b' ' as u16);
        }
        command_line.push(b'"' as u16);
        let mut backslashes = 0;
        for unit in argument.encode_wide() {
            if unit == b'\\' as u16 {
                backslashes += 1;
            } else if unit == b'"' as u16 {
                command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
                command_line.push(unit);
                backslashes = 0;
            } else {
                command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
                command_line.push(unit);
                backslashes = 0;
            }
        }
        command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
        command_line.push(b'"' as u16);
    }
    command_line.push(0);

    let application: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let current_dir: Vec<u16> = root
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both structures are plain Win32 output records whose all-zero
    // state is documented initialization. `cb` is set before CreateProcessW.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: all pointers reference live, NUL-terminated buffers for the
    // duration of the call. Security/environment pointers are null by design.
    // Most importantly, bInheritHandles is FALSE: the resident daemon cannot
    // keep a caller's captured stdout/stderr pipes alive.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            current_dir.as_ptr(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to start frostd");
    }
    // SAFETY: CreateProcessW succeeded and returned two owned kernel handles.
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}

pub(crate) fn daemon_command(root: &std::path::Path, command: DaemonCmd) -> Result<i32> {
    use frostbuild_daemon::{Request, PROTOCOL_VERSION};
    match command {
        DaemonCmd::Serve => {
            frostbuild_daemon::serve(root)?;
            Ok(0)
        }
        DaemonCmd::Start => {
            match frostbuild_daemon::request(
                root,
                &Request::Status {
                    version: PROTOCOL_VERSION,
                },
            ) {
                Ok(response) if !is_protocol_mismatch(&response) => {
                    println!("frostd: already running");
                    return Ok(0);
                }
                // A daemon is resident but speaks another protocol version, so
                // binding a second one would fail after a pointless wait.
                Ok(_) => bail!(
                    "a frostd from another frost version is running for this workspace; stop it \
                     with `frost daemon stop` (a daemon older than 0.4.0 may have to be terminated \
                     manually)"
                ),
                Err(_) => {}
            }
            let executable = std::env::current_exe()?;
            spawn_daemon(&executable, root)?;
            for _ in 0..50 {
                if frostbuild_daemon::request(
                    root,
                    &Request::Status {
                        version: PROTOCOL_VERSION,
                    },
                )
                .is_ok()
                {
                    println!("frostd: started");
                    return Ok(0);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            bail!("frostd did not become ready");
        }
        DaemonCmd::Status => {
            let response = frostbuild_daemon::request(
                root,
                &Request::Status {
                    version: PROTOCOL_VERSION,
                },
            )?;
            if is_protocol_mismatch(&response) {
                println!(
                    "frostd: running, but speaks protocol {} rather than {PROTOCOL_VERSION}",
                    response.version
                );
                return Ok(response.code);
            }
            println!(
                "frostd: {} (protocol {})",
                response.stdout, response.version
            );
            Ok(response.code)
        }
        DaemonCmd::Stop => {
            let response = frostbuild_daemon::request(
                root,
                &Request::Shutdown {
                    version: PROTOCOL_VERSION,
                },
            )?;
            println!("frostd: {}", response.stdout);
            Ok(response.code)
        }
        DaemonCmd::Restart => {
            let _ = frostbuild_daemon::request(
                root,
                &Request::Shutdown {
                    version: PROTOCOL_VERSION,
                },
            );
            daemon_command(root, DaemonCmd::Start)
        }
    }
}
