//! Shell session management for TUI
//!
//! Provides persistent shell sessions inside containers using a host-side PTY relay.
//! When the user detaches (Ctrl+\), the PTY and docker exec process stay alive,
//! allowing reattachment with full state preserved.

use devc_provider::ProviderType;
use std::io::{self, Read as _, Write};
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};
use std::process::{Command, Stdio};

use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::pty::openpty;
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet};

const CTRL_BACKSLASH: u8 = 0x1c;

/// Reset terminal to sane state using stty
#[cfg(unix)]
pub fn reset_terminal() {
    let _ = Command::new("stty")
        .arg("sane")
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = io::stdout().flush();
}

#[cfg(not(unix))]
pub fn reset_terminal() {
    let _ = io::stdout().flush();
}

/// Configuration for spawning a shell session
pub struct ShellConfig {
    pub provider_type: ProviderType,
    pub container_id: String,
    pub shell: String,
    pub user: Option<String>,
    pub working_dir: Option<String>,
}

/// Why the relay loop stopped
pub enum ShellExitReason {
    /// User pressed Ctrl+\ to return to TUI (session preserved)
    Detached,
    /// Shell process exited
    Exited,
    /// I/O error during relay
    Error(io::Error),
}

/// A persistent shell session backed by a host-side PTY
pub struct PtyShell {
    master_fd: OwnedFd,
    child: std::process::Child,
}

// SIGWINCH flag: set by signal handler, checked in poll loop
static SIGWINCH_RECEIVED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

extern "C" fn sigwinch_handler(_: libc::c_int) {
    SIGWINCH_RECEIVED.store(true, std::sync::atomic::Ordering::SeqCst);
}

impl PtyShell {
    /// Spawn a new shell session connected to a host-side PTY
    pub fn spawn(config: &ShellConfig) -> io::Result<Self> {
        // Get current terminal size
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let winsize = nix::pty::Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        // Open a PTY pair
        let pty = openpty(Some(&winsize), None)
            .map_err(io::Error::other)?;

        let master_fd = pty.master;
        let slave_fd = pty.slave;

        // Build the docker/podman exec command
        // No --detach-keys: we intercept Ctrl+\ in the relay loop ourselves
        let runtime = match config.provider_type {
            ProviderType::Docker => "docker",
            ProviderType::Podman => "podman",
        };

        let mut cmd = Command::new(runtime);
        cmd.args(["exec", "-it"]);

        if let Some(ref user) = config.user {
            cmd.args(["-u", user]);
        }
        if let Some(ref wd) = config.working_dir {
            cmd.args(["-w", wd]);
        }

        cmd.arg(&config.container_id);
        cmd.arg(&config.shell);

        // Connect child stdin/stdout/stderr to the slave PTY fd.
        // Each Stdio::from_raw_fd takes ownership and will close the fd, so we must
        // dup() to create separate fds for stdin and stdout, giving the original to stderr.
        let slave_raw = slave_fd.into_raw_fd(); // consume OwnedFd so it won't double-close
        unsafe {
            cmd.stdin(Stdio::from_raw_fd(libc::dup(slave_raw)));
            cmd.stdout(Stdio::from_raw_fd(libc::dup(slave_raw)));
            cmd.stderr(Stdio::from_raw_fd(slave_raw)); // last one takes the original
        }

        let child = cmd.spawn()?;

        Ok(PtyShell { master_fd, child })
    }

    /// Run the relay loop between the real terminal and the PTY master.
    /// Blocks until detach (Ctrl+\), shell exit, or error.
    pub fn relay(&self) -> ShellExitReason {
        // Install SIGWINCH handler
        SIGWINCH_RECEIVED.store(false, std::sync::atomic::Ordering::SeqCst);
        let sa = SigAction::new(
            SigHandler::Handler(sigwinch_handler),
            SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        let old_sigwinch = unsafe { sigaction(nix::sys::signal::Signal::SIGWINCH, &sa) };

        // Put terminal in raw mode
        if let Err(e) = crossterm::terminal::enable_raw_mode() {
            return ShellExitReason::Error(e);
        }

        let result = self.relay_loop();

        // Restore terminal
        let _ = crossterm::terminal::disable_raw_mode();

        // Restore old SIGWINCH handler
        if let Ok(old) = old_sigwinch {
            let _ = unsafe { sigaction(nix::sys::signal::Signal::SIGWINCH, &old) };
        }

        result
    }

    fn relay_loop(&self) -> ShellExitReason {
        let stdin_raw = io::stdin().as_raw_fd();
        let master_raw = self.master_fd.as_raw_fd();
        // SAFETY: stdin fd 0 is valid for the duration of this function
        let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_raw) };
        let master_borrowed = self.master_fd.as_fd();

        let mut buf = [0u8; 4096];

        loop {
            // Check for SIGWINCH
            if SIGWINCH_RECEIVED.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.propagate_winsize();
            }

            let stdin_pollfd = PollFd::new(stdin_borrowed, PollFlags::POLLIN);
            let master_pollfd = PollFd::new(master_borrowed, PollFlags::POLLIN);
            let mut fds = [stdin_pollfd, master_pollfd];

            match nix::poll::poll(&mut fds, PollTimeout::from(200u16)) {
                Ok(0) => continue, // timeout, loop to check SIGWINCH
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => {
                    return ShellExitReason::Error(io::Error::other(e));
                }
                Ok(_) => {}
            }

            // Check master fd first (output from shell)
            if let Some(revents) = fds[1].revents() {
                if revents.contains(PollFlags::POLLIN) {
                    let n = nix::unistd::read(master_raw, &mut buf);
                    match n {
                        Ok(0) | Err(_) => return ShellExitReason::Exited,
                        Ok(n) => {
                            let mut stdout = io::stdout().lock();
                            if stdout.write_all(&buf[..n]).is_err() {
                                return ShellExitReason::Exited;
                            }
                            let _ = stdout.flush();
                        }
                    }
                }
                if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                    // Drain any remaining data
                    loop {
                        match nix::unistd::read(master_raw, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut stdout = io::stdout().lock();
                                let _ = stdout.write_all(&buf[..n]);
                                let _ = stdout.flush();
                            }
                        }
                    }
                    return ShellExitReason::Exited;
                }
            }

            // Check stdin (input from user)
            if let Some(revents) = fds[0].revents() {
                if revents.contains(PollFlags::POLLIN) {
                    let mut stdin = io::stdin().lock();
                    match stdin.read(&mut buf) {
                        Ok(0) => return ShellExitReason::Exited,
                        Err(e) => {
                            return ShellExitReason::Error(e);
                        }
                        Ok(n) => {
                            // Scan for Ctrl+\ (0x1c)
                            if let Some(pos) = buf[..n].iter().position(|&b| b == CTRL_BACKSLASH)
                            {
                                // Write bytes before the detach key to master
                                if pos > 0 {
                                    let _ =
                                        nix::unistd::write(&self.master_fd, &buf[..pos]);
                                }
                                return ShellExitReason::Detached;
                            }

                            // Forward all bytes to master
                            if nix::unistd::write(&self.master_fd, &buf[..n]).is_err() {
                                return ShellExitReason::Exited;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Propagate current terminal size to the PTY and child process
    fn propagate_winsize(&self) {
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            let ws = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            unsafe {
                libc::ioctl(self.master_fd.as_raw_fd(), libc::TIOCSWINSZ, &ws);
            }
            // TIOCSWINSZ only sends SIGWINCH to the slave's foreground process group,
            // but we never set one up (no setsid/TIOCSCTTY). Explicitly signal the
            // child (docker/podman exec) so it queries the new size and propagates it
            // to the container's PTY.
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(self.child.id() as i32),
                nix::sys::signal::Signal::SIGWINCH,
            );
        }
    }

    /// Check if the shell process is still alive
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Set the PTY size (call before relay when reattaching after a resize in the TUI)
    pub fn set_size(&self, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.master_fd.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }
    }
}

impl Drop for PtyShell {
    fn drop(&mut self) {
        // Kill the child process when the PtyShell is dropped
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// PtyShell holds OwnedFd and Child which are Send
// The OwnedFd is only accessed from one thread at a time (relay runs in spawn_blocking)
unsafe impl Send for PtyShell {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_config_docker() {
        let config = ShellConfig {
            provider_type: ProviderType::Docker,
            container_id: "abc123".to_string(),
            shell: "/bin/bash".to_string(),
            user: None,
            working_dir: None,
        };
        assert_eq!(config.container_id, "abc123");
        assert_eq!(config.shell, "/bin/bash");
    }

    #[test]
    fn test_shell_config_podman_with_options() {
        let config = ShellConfig {
            provider_type: ProviderType::Podman,
            container_id: "def456".to_string(),
            shell: "/bin/zsh".to_string(),
            user: Some("root".to_string()),
            working_dir: Some("/workspace".to_string()),
        };
        assert_eq!(config.user, Some("root".to_string()));
        assert_eq!(config.working_dir, Some("/workspace".to_string()));
    }

    #[test]
    fn test_shell_exit_reason_variants() {
        // Just verify the enum can be constructed
        let _d = ShellExitReason::Detached;
        let _e = ShellExitReason::Exited;
        let _err = ShellExitReason::Error(io::Error::new(io::ErrorKind::Other, "test"));
    }
}
