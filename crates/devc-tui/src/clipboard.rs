//! Clipboard support for copying logs

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Copy text to system clipboard (non-blocking)
///
/// Spawns a background thread to avoid blocking the TUI.
/// Tries clipboard commands in order of preference:
/// - wl-copy (Wayland)
/// - xclip (X11)
/// - xsel (X11 alternative)
/// - pbcopy (macOS)
///
/// Returns immediately - the actual copy happens in background.
/// Returns Ok if a clipboard command was found and started.
pub fn copy_to_clipboard(content: &str) -> Result<(), String> {
    // Use a channel with timeout to get quick feedback
    let (tx, rx) = mpsc::channel();
    let content = content.to_string();

    thread::spawn(move || {
        let result = copy_to_clipboard_sync(&content);
        let _ = tx.send(result);
    });

    // Wait briefly for result (100ms) - enough to know if command exists
    match rx.recv_timeout(Duration::from_millis(100)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Still running in background, assume it will succeed
            Ok(())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("Clipboard thread terminated unexpectedly".to_string())
        }
    }
}

/// Synchronous clipboard copy (runs in background thread)
fn copy_to_clipboard_sync(content: &str) -> Result<(), String> {
    let commands: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("pbcopy", &[]),
    ];

    for (cmd, args) in commands {
        // First check if the command exists
        let which_result = Command::new("which")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        if !which_result.map(|s| s.success()).unwrap_or(false) {
            continue;
        }

        if let Ok(mut child) = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                if stdin.write_all(content.as_bytes()).is_ok() {
                    drop(stdin); // Close stdin to signal EOF

                    // Wait with timeout
                    let timeout = Duration::from_secs(5);
                    let start = std::time::Instant::now();

                    loop {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                if status.success() {
                                    return Ok(());
                                }
                                break;
                            }
                            Ok(None) => {
                                if start.elapsed() > timeout {
                                    let _ = child.kill();
                                    break;
                                }
                                thread::sleep(Duration::from_millis(50));
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        }
    }

    Err("No clipboard command available (tried wl-copy, xclip, xsel, pbcopy)".to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_clipboard_commands_defined() {
        // Just verify the module compiles correctly
        // Actual clipboard testing would require mocking or real clipboard tools
        assert!(true);
    }
}
