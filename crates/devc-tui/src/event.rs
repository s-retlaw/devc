//! Terminal event handling

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Terminal events
#[derive(Debug, Clone)]
pub enum Event {
    /// Terminal tick (for animations/updates)
    Tick,
    /// Key press
    Key(KeyEvent),
    /// Mouse event
    Mouse(MouseEvent),
    /// Terminal resize
    Resize(u16, u16),
}

/// Event handler that runs in a separate task
pub struct EventHandler {
    /// Event receiver
    rx: mpsc::UnboundedReceiver<Event>,
    /// Stop signal sender (Some if task is running, None after stop)
    stop_tx: Option<oneshot::Sender<()>>,
}

impl EventHandler {
    /// Create a new event handler with the given tick rate
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (stop_tx, mut stop_rx) = oneshot::channel();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Stop signal received - exit the loop
                    _ = &mut stop_rx => break,

                    // Poll for events with timeout
                    _ = async {
                        if event::poll(tick_rate).unwrap_or(false) {
                            match event::read() {
                                Ok(CrosstermEvent::Key(key)) => {
                                    let _ = tx.send(Event::Key(key));
                                }
                                Ok(CrosstermEvent::Mouse(mouse)) => {
                                    let _ = tx.send(Event::Mouse(mouse));
                                }
                                Ok(CrosstermEvent::Resize(w, h)) => {
                                    let _ = tx.send(Event::Resize(w, h));
                                }
                                _ => {}
                            }
                        } else {
                            let _ = tx.send(Event::Tick);
                        }
                    } => {}
                }
            }
        });

        Self {
            rx,
            stop_tx: Some(stop_tx),
        }
    }

    /// Stop the event handler task
    ///
    /// This signals the background task to exit. After calling stop(),
    /// the EventHandler should be dropped and a new one created.
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }

    /// Receive the next event
    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}
