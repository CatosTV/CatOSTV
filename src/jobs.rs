//! Background jobs: run blocking system calls off the UI thread.
//!
//! Every `nmcli`, `bluetoothctl`, `wpctl`, `lsblk`, `wtype` and installer
//! call goes through [`Jobs::spawn`]. The result is sent back as a message
//! and the UI is woken with `request_repaint`, so the TV never freezes on a
//! slow or hung system tool.

use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

pub struct Jobs<M: Send + 'static> {
    tx: Sender<M>,
    rx: Receiver<M>,
    ctx: egui::Context,
}

impl<M: Send + 'static> Jobs<M> {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            ctx: ctx.clone(),
        }
    }

    /// Run `f` on a new thread and deliver its message.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() -> M + Send + 'static,
    {
        self.try_spawn(f);
    }

    /// Like [`Jobs::spawn`] but returns false (and logs) if the thread could
    /// not be started, for callers that track "in flight" state.
    pub fn try_spawn<F>(&self, f: F) -> bool
    where
        F: FnOnce() -> M + Send + 'static,
    {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        match thread::Builder::new()
            .name("catostv-job".into())
            .spawn(move || {
                let msg = f();
                let _ = tx.send(msg);
                ctx.request_repaint();
            }) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("catostv: could not start background job: {e}");
                false
            }
        }
    }

    /// A sender + repaint handle for long-running producers (IPC listener,
    /// installer executor) that emit many messages.
    pub fn sender(&self) -> (Sender<M>, egui::Context) {
        (self.tx.clone(), self.ctx.clone())
    }

    /// All messages received since the last call.
    pub fn drain(&self) -> Vec<M> {
        self.rx.try_iter().collect()
    }
}
