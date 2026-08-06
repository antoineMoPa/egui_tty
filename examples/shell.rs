//! A login shell in a window: `cargo run --example shell`.
//!
//! The interesting part is [`Pty`] — about forty lines that hand a real pty to the widget.

use std::{
    io::{Read, Write},
    sync::{Arc, Mutex, mpsc},
};

use egui_tty::{Terminal, TerminalStyle, Tty, TtyStream};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// A pty, as a [`Tty`]: keystrokes go in the master's writer, and resizes go to the master
/// itself so the shell's foreground process gets `SIGWINCH`.
struct Pty {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
}

impl Tty for Pty {
    fn write(&self, bytes: &[u8]) -> egui_tty::Result<()> {
        let mut writer = self.writer.lock().map_err(egui_tty::Error::msg)?;
        writer.write_all(bytes).map_err(egui_tty::Error::msg)?;
        writer.flush().map_err(egui_tty::Error::msg)
    }

    fn resize(&self, cols: u16, rows: u16) -> egui_tty::Result<()> {
        self.master
            .lock()
            .map_err(egui_tty::Error::msg)?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(egui_tty::Error::msg)
    }
}

/// Start a shell on a pty and hand back the stream a terminal is built from.
fn spawn_shell() -> anyhow::Result<TtyStream> {
    let pty = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut command = CommandBuilder::new(shell);
    command.arg("-l");
    command.env("TERM", "xterm-256color");
    let _child = pty.slave.spawn_command(command)?;
    // The slave handle has to be dropped, or the reader below never sees the shell exit.
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader()?;
    let writer = pty.master.take_writer()?;
    let (sender, output) = mpsc::channel();

    // One thread reading the pty, forwarding whatever the shell prints to the UI thread.
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 8 * 1024];
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 || sender.send(buffer[..count].to_vec()).is_err() {
                break;
            }
        }
    });

    Ok(TtyStream {
        output,
        tty: Arc::new(Pty {
            writer: Mutex::new(writer),
            master: Mutex::new(pty.master),
        }),
    })
}

struct App {
    terminal: Terminal,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let style = TerminalStyle::from_visuals(ui.visuals());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x10, 0x14, 0x1c)))
            .show(ui, |ui| {
                self.terminal.ui(ui, &style);
            });
    }
}

fn main() -> anyhow::Result<()> {
    let mut terminal = Terminal::new(spawn_shell()?)?.with_label("shell");
    // The window opens with the keyboard already in the shell, so it can be typed into.
    terminal.request_focus();

    eframe::run_native(
        "egui_tty",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 560.0]),
            ..Default::default()
        },
        Box::new(|_creation| Ok(Box::new(App { terminal }))),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}
