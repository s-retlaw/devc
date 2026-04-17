//! Shell session management for TUI
//!
//! Provides persistent shell sessions inside containers using a host-side PTY relay.
//! When the user detaches (Ctrl+\), the PTY and docker exec process stay alive,
//! allowing reattachment with full state preserved.

use std::io::{self, Write};
#[cfg(unix)]
use std::process::{Command, Stdio};

/// OSC escape sequence scanner for URL open requests from the container.
///
/// Filters the PTY output byte stream, intercepting custom OSC sequences
/// of the form `\x1b]devc;open-url;<URL>\x07` and extracting the URLs.
/// Handles sequences split across read boundaries via carry-over state.
pub struct OscUrlScanner {
    state: ScanState,
    /// Bytes accumulated while matching a potential OSC prefix or URL
    partial: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    /// Normal output passthrough
    Normal,
    /// Saw \x1b, waiting for ]
    EscSeen,
    /// Matching the "devc;open-url;" prefix byte by byte
    MatchingPrefix { matched: usize },
    /// Accumulating URL bytes until \x07
    AccumulatingUrl,
}

impl Default for OscUrlScanner {
    fn default() -> Self {
        Self {
            state: ScanState::Normal,
            partial: Vec::new(),
        }
    }
}

impl OscUrlScanner {
    const PREFIX: &[u8] = b"devc;open-url;";
    const MAX_URL_LEN: usize = 8192;

    pub fn new() -> Self {
        Self::default()
    }

    /// Filter a chunk of PTY output, stripping any devc URL OSC sequences.
    /// Returns the filtered output to write to the terminal and any extracted URLs.
    pub fn filter(&mut self, data: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut output = Vec::with_capacity(data.len());
        let mut urls = Vec::new();

        for &byte in data {
            match self.state {
                ScanState::Normal => {
                    if byte == 0x1b {
                        self.state = ScanState::EscSeen;
                        self.partial.clear();
                        self.partial.push(byte);
                    } else {
                        output.push(byte);
                    }
                }
                ScanState::EscSeen => {
                    if byte == b']' {
                        self.partial.push(byte);
                        self.state = ScanState::MatchingPrefix { matched: 0 };
                    } else {
                        // Not an OSC — flush buffered \x1b and this byte
                        output.extend_from_slice(&self.partial);
                        output.push(byte);
                        self.partial.clear();
                        self.state = ScanState::Normal;
                    }
                }
                ScanState::MatchingPrefix { matched } => {
                    if byte == Self::PREFIX[matched] {
                        self.partial.push(byte);
                        let next = matched + 1;
                        if next == Self::PREFIX.len() {
                            // Full prefix matched, now accumulate URL
                            self.partial.clear();
                            self.state = ScanState::AccumulatingUrl;
                        } else {
                            self.state = ScanState::MatchingPrefix { matched: next };
                        }
                    } else {
                        // Prefix diverged — flush accumulated bytes as normal output
                        output.extend_from_slice(&self.partial);
                        output.push(byte);
                        self.partial.clear();
                        self.state = ScanState::Normal;
                    }
                }
                ScanState::AccumulatingUrl => {
                    if byte == 0x07 {
                        // OSC terminator — extract URL
                        if let Ok(url) = String::from_utf8(self.partial.clone()) {
                            urls.push(url);
                        }
                        self.partial.clear();
                        self.state = ScanState::Normal;
                    } else if self.partial.len() >= Self::MAX_URL_LEN {
                        // Overflow — flush as false positive
                        // Reconstruct the original prefix bytes for output
                        output.extend_from_slice(b"\x1b]");
                        output.extend_from_slice(Self::PREFIX);
                        output.extend_from_slice(&self.partial);
                        output.push(byte);
                        self.partial.clear();
                        self.state = ScanState::Normal;
                    } else {
                        self.partial.push(byte);
                    }
                }
            }
        }

        (output, urls)
    }
}

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
    pub runtime_program: String,
    pub runtime_prefix: Vec<String>,
    pub container_id: String,
    pub shell: String,
    pub user: Option<String>,
    pub working_dir: Option<String>,
    /// Extra environment variables to inject (e.g. GH_TOKEN)
    pub env: std::collections::HashMap<String, String>,
    /// Host-side path to the browser URL queue file
    pub browser_queue_path: Option<String>,
    /// Whether auto-forwarding is enabled (global setting)
    pub auto_forward_enabled: bool,
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

// --- Unix-only: PTY shell implementation ---

#[cfg(unix)]
mod pty {
    use super::*;
    use std::io::Read as _;
    use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};

    use nix::libc;
    use nix::poll::{PollFd, PollFlags, PollTimeout};
    use nix::pty::openpty;
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet};

    const CTRL_BACKSLASH: u8 = 0x1c;

    /// A persistent shell session backed by a host-side PTY
    pub struct PtyShell {
        master_fd: OwnedFd,
        child: std::process::Child,
        in_alternate_screen: bool,
        url_scanner: OscUrlScanner,
        /// Host-side path to browser URL queue file (shared via workspace bind mount)
        browser_queue_path: Option<std::path::PathBuf>,
        /// Runtime info for on-demand port forwarding
        runtime_program: String,
        runtime_prefix: Vec<String>,
        container_id: String,
        auto_forward_enabled: bool,
        /// Forwarders spawned on-demand for browser URL requests, kept alive for the shell's lifetime
        on_demand_forwarders: Vec<crate::tunnel::PortForwarder>,
        /// Ports we've already attempted to forward on-demand, to avoid retrying on repeat URLs
        attempted_on_demand_ports: std::collections::HashSet<u16>,
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
            let pty = openpty(Some(&winsize), None).map_err(io::Error::other)?;

            let master_fd = pty.master;
            let slave_fd = pty.slave;

            // Build the exec command using pre-resolved runtime args
            // No --detach-keys: we intercept Ctrl+\ in the relay loop ourselves
            let mut cmd = Command::new(&config.runtime_program);
            cmd.args(&config.runtime_prefix);
            cmd.args(["exec", "-it"]);

            if let Some(ref user) = config.user {
                cmd.args(["-u", user]);
            }
            if let Some(ref wd) = config.working_dir {
                cmd.args(["-w", wd]);
            }

            for (key, val) in &config.env {
                cmd.args(["-e", &format!("{}={}", key, val)]);
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

            Ok(PtyShell {
                master_fd,
                child,
                in_alternate_screen: false,
                url_scanner: OscUrlScanner::new(),
                browser_queue_path: config
                    .browser_queue_path
                    .as_ref()
                    .map(std::path::PathBuf::from),
                runtime_program: config.runtime_program.clone(),
                runtime_prefix: config.runtime_prefix.clone(),
                container_id: config.container_id.clone(),
                auto_forward_enabled: config.auto_forward_enabled,
                on_demand_forwarders: Vec::new(),
                attempted_on_demand_ports: std::collections::HashSet::new(),
            })
        }

        /// Whether the child app is currently using the alternate screen buffer
        pub fn is_in_alternate_screen(&self) -> bool {
            self.in_alternate_screen
        }

        /// Scan relay output for alternate screen enter/leave sequences.
        /// Tracks the last occurrence to determine current state.
        fn scan_alternate_screen(&mut self, data: &[u8]) {
            const ENTER: &[u8] = b"\x1b[?1049h";
            const LEAVE: &[u8] = b"\x1b[?1049l";
            let mut last_enter = None;
            let mut last_leave = None;
            for i in 0..data.len() {
                if data[i..].starts_with(ENTER) {
                    last_enter = Some(i);
                } else if data[i..].starts_with(LEAVE) {
                    last_leave = Some(i);
                }
            }
            match (last_enter, last_leave) {
                (Some(e), Some(l)) => self.in_alternate_screen = e > l,
                (Some(_), None) => self.in_alternate_screen = true,
                (None, Some(_)) => self.in_alternate_screen = false,
                (None, None) => {}
            }
        }

        /// Run the relay loop between the real terminal and the PTY master.
        /// Blocks until detach (Ctrl+\), shell exit, or error.
        /// If `force_redraw` is true, injects Ctrl+L through the PTY data channel
        /// so that TUI apps (e.g. nvim) redraw after a reattach.
        pub fn relay(&mut self, force_redraw: bool) -> ShellExitReason {
            // Install SIGWINCH handler.
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

            // On reattach, update PTY size and inject Ctrl+L to trigger redraw
            if force_redraw {
                self.propagate_winsize();
                let _ = nix::unistd::write(&self.master_fd, b"\x0c");
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

        fn relay_loop(&mut self) -> ShellExitReason {
            let stdin_raw = io::stdin().as_raw_fd();
            let master_raw = self.master_fd.as_raw_fd();
            // SAFETY: these fds are valid for the duration of this function.
            // We use borrow_raw for master_borrowed instead of as_fd() so that
            // the borrow doesn't tie up &self, allowing &mut self in scan_alternate_screen.
            let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_raw) };
            let master_borrowed = unsafe { BorrowedFd::borrow_raw(master_raw) };

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
                    Ok(0) => {
                        // Timeout — check for browser URL queue file
                        self.check_browser_queue();
                        continue;
                    }
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
                                self.scan_alternate_screen(&buf[..n]);
                                // OSC scanner: fallback for containers without queue file
                                let (filtered, urls) = self.url_scanner.filter(&buf[..n]);
                                for url in &urls {
                                    self.open_url_ensuring_forwarded(url);
                                }
                                let mut stdout = io::stdout().lock();
                                if stdout.write_all(&filtered).is_err() {
                                    return ShellExitReason::Exited;
                                }
                                let _ = stdout.flush();
                            }
                        }
                    }
                    if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR)
                    {
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
                                if let Some(pos) =
                                    buf[..n].iter().position(|&b| b == CTRL_BACKSLASH)
                                {
                                    // Write bytes before the detach key to master
                                    if pos > 0 {
                                        let _ = nix::unistd::write(&self.master_fd, &buf[..pos]);
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

        /// Open a URL, pre-spawning forwarders for every localhost port the URL
        /// references. The URL's top-level host may point at an external auth
        /// provider while the actual OAuth callback is an embedded localhost
        /// port (e.g. `aws sso login`); we forward every localhost port we see.
        /// Dedup via `attempted_on_demand_ports`: at most one spawn attempt per
        /// (shell session, port). A spawn failure typically means the port is
        /// already bound (by the app-level auto-forwarder or a prior spawn).
        fn open_url_ensuring_forwarded(&mut self, url: &str) {
            if self.auto_forward_enabled {
                for port in crate::tunnel::extract_localhost_ports(url) {
                    if !self.attempted_on_demand_ports.insert(port) {
                        continue;
                    }
                    let rt = tokio::runtime::Handle::current();
                    if let Ok(forwarder) = rt.block_on(crate::tunnel::spawn_forwarder(
                        self.runtime_program.clone(),
                        self.runtime_prefix.clone(),
                        self.container_id.clone(),
                        port,
                        port,
                    )) {
                        self.on_demand_forwarders.push(forwarder);
                    }
                }
            }
            let _ = crate::tunnel::open_url(url);
        }

        /// Check the browser URL queue file on the host filesystem.
        /// The wrapper script inside the container writes URLs to a file in the
        /// shared workspace mount. We read them here and open on the host.
        fn check_browser_queue(&mut self) {
            let path = match &self.browser_queue_path {
                Some(p) => p,
                None => return,
            };
            // Read and remove atomically-ish
            let content = match std::fs::read_to_string(path) {
                Ok(c) if !c.is_empty() => c,
                _ => return,
            };
            let _ = std::fs::remove_file(path);

            for line in content.lines() {
                let url = line.trim();
                if crate::tunnel::is_browser_openable_url(url) {
                    self.open_url_ensuring_forwarded(url);
                }
            }
        }

        /// Set the PTY to a specific size and send SIGWINCH to the child process.
        ///
        /// Used on detach to set a dummy 1x1 size so the next reattach's
        /// `propagate_winsize()` is a genuine change that docker exec will propagate.
        pub fn set_size_and_signal(&self, cols: u16, rows: u16) {
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

        /// Propagate current terminal size to the PTY and child process
        fn propagate_winsize(&self) {
            if let Ok((cols, rows)) = crossterm::terminal::size() {
                self.set_size_and_signal(cols, rows);
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
}

#[cfg(unix)]
pub use pty::PtyShell;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_config_docker() {
        let config = ShellConfig {
            runtime_program: "docker".to_string(),
            runtime_prefix: vec![],
            container_id: "abc123".to_string(),
            shell: "/bin/bash".to_string(),
            user: None,
            working_dir: None,
            env: std::collections::HashMap::new(),
            browser_queue_path: None,
            auto_forward_enabled: false,
        };
        assert_eq!(config.container_id, "abc123");
        assert_eq!(config.shell, "/bin/bash");
    }

    #[test]
    fn test_shell_config_podman_with_options() {
        let config = ShellConfig {
            runtime_program: "podman".to_string(),
            runtime_prefix: vec![],
            container_id: "def456".to_string(),
            shell: "/bin/zsh".to_string(),
            user: Some("root".to_string()),
            working_dir: Some("/workspace".to_string()),
            env: std::collections::HashMap::new(),
            browser_queue_path: None,
            auto_forward_enabled: false,
        };
        assert_eq!(config.user, Some("root".to_string()));
        assert_eq!(config.working_dir, Some("/workspace".to_string()));
    }

    #[test]
    fn test_shell_config_toolbox() {
        let config = ShellConfig {
            runtime_program: "flatpak-spawn".to_string(),
            runtime_prefix: vec!["--host".to_string(), "podman".to_string()],
            container_id: "toolbox123".to_string(),
            shell: "/bin/bash".to_string(),
            user: None,
            working_dir: None,
            env: std::collections::HashMap::new(),
            browser_queue_path: None,
            auto_forward_enabled: false,
        };
        assert_eq!(config.runtime_program, "flatpak-spawn");
        assert_eq!(config.runtime_prefix, vec!["--host", "podman"]);
    }

    #[test]
    fn test_shell_exit_reason_variants() {
        // Just verify the enum can be constructed
        let _d = ShellExitReason::Detached;
        let _e = ShellExitReason::Exited;
        let _err = ShellExitReason::Error(io::Error::other("test"));
    }

    #[test]
    fn test_osc_scanner_complete_sequence() {
        let mut scanner = OscUrlScanner::new();
        let input = b"\x1b]devc;open-url;https://example.com\x07";
        let (output, urls) = scanner.filter(input);
        assert!(output.is_empty());
        assert_eq!(urls, vec!["https://example.com"]);
    }

    #[test]
    fn test_osc_scanner_with_surrounding_text() {
        let mut scanner = OscUrlScanner::new();
        let input = b"hello \x1b]devc;open-url;https://example.com\x07 world";
        let (output, urls) = scanner.filter(input);
        assert_eq!(output, b"hello  world");
        assert_eq!(urls, vec!["https://example.com"]);
    }

    #[test]
    fn test_osc_scanner_split_across_reads() {
        let mut scanner = OscUrlScanner::new();

        // Split in the middle of the prefix
        let (out1, urls1) = scanner.filter(b"before\x1b]dev");
        assert_eq!(out1, b"before");
        assert!(urls1.is_empty());

        let (out2, urls2) = scanner.filter(b"c;open-url;https://example.com\x07after");
        assert_eq!(out2, b"after");
        assert_eq!(urls2, vec!["https://example.com"]);
    }

    #[test]
    fn test_osc_scanner_split_at_esc() {
        let mut scanner = OscUrlScanner::new();

        let (out1, urls1) = scanner.filter(b"text\x1b");
        assert_eq!(out1, b"text");
        assert!(urls1.is_empty());

        let (out2, urls2) = scanner.filter(b"]devc;open-url;https://test.dev\x07done");
        assert_eq!(out2, b"done");
        assert_eq!(urls2, vec!["https://test.dev"]);
    }

    #[test]
    fn test_osc_scanner_split_in_url() {
        let mut scanner = OscUrlScanner::new();

        let (out1, urls1) = scanner.filter(b"\x1b]devc;open-url;https://exa");
        assert!(out1.is_empty());
        assert!(urls1.is_empty());

        let (out2, urls2) = scanner.filter(b"mple.com/path\x07");
        assert!(out2.is_empty());
        assert_eq!(urls2, vec!["https://example.com/path"]);
    }

    #[test]
    fn test_osc_scanner_multiple_urls() {
        let mut scanner = OscUrlScanner::new();
        let input = b"\x1b]devc;open-url;https://a.com\x07mid\x1b]devc;open-url;https://b.com\x07";
        let (output, urls) = scanner.filter(input);
        assert_eq!(output, b"mid");
        assert_eq!(urls, vec!["https://a.com", "https://b.com"]);
    }

    #[test]
    fn test_osc_scanner_false_esc_passthrough() {
        let mut scanner = OscUrlScanner::new();
        // Alternate screen sequence should pass through
        let input = b"\x1b[?1049h";
        let (output, urls) = scanner.filter(input);
        assert_eq!(output, b"\x1b[?1049h");
        assert!(urls.is_empty());
    }

    #[test]
    fn test_osc_scanner_other_osc_passthrough() {
        let mut scanner = OscUrlScanner::new();
        // Other OSC sequences (e.g. set title) should pass through
        let input = b"\x1b]0;my title\x07";
        let (output, urls) = scanner.filter(input);
        assert_eq!(output, b"\x1b]0;my title\x07");
        assert!(urls.is_empty());
    }

    #[test]
    fn test_osc_scanner_overflow_protection() {
        let mut scanner = OscUrlScanner::new();
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b]devc;open-url;");
        // Add more than MAX_URL_LEN bytes without terminator
        input.extend(std::iter::repeat_n(b'x', OscUrlScanner::MAX_URL_LEN + 1));
        let (output, urls) = scanner.filter(&input);
        assert!(urls.is_empty());
        assert!(!output.is_empty()); // Flushed as normal output
    }

    #[test]
    fn test_osc_scanner_prefix_diverges() {
        let mut scanner = OscUrlScanner::new();
        // Starts like our prefix but diverges
        let input = b"\x1b]devc;other-cmd;data\x07rest";
        let (output, urls) = scanner.filter(input);
        assert!(urls.is_empty());
        // All bytes should pass through
        assert_eq!(output, b"\x1b]devc;other-cmd;data\x07rest");
    }

    #[test]
    fn test_osc_scanner_normal_text_passthrough() {
        let mut scanner = OscUrlScanner::new();
        let input = b"just normal terminal output\r\n";
        let (output, urls) = scanner.filter(input);
        assert_eq!(output, input.as_slice());
        assert!(urls.is_empty());
    }
}
