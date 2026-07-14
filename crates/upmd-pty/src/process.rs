//! Process management for commands running inside a pseudo-terminal.
//!
//! A [`Process`] owns the PTY master side and spawns a child process attached
//! to the slave side. Output is read from the master on a background thread and
//! emitted as [`PtyEvent::Output`] byte chunks. A second watcher waits for the
//! child exit status, then emits [`PtyEvent::Exit`] followed by
//! [`PtyEvent::Closed`].
//!
//! After spawning the child, the parent drops its copy of the slave handle. The
//! child keeps its own slave handle, and closing the parent's copy is what lets
//! the master reader observe EOF after the child exits.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
    thread::spawn,
};

use crate::signal::ExitSignal;
use anyhow::Result;
use crossbeam_channel::Sender;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtyPair, PtySize};

pub type PtyEventResult = Result<PtyEvent>;

const PTY_READ_BUFFER_SIZE: usize = 32 * 1024;

/// Events emitted by a running PTY session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    /// Raw bytes read from the PTY master.
    Output(Vec<u8>),
    /// Child process exit status.
    Exit(PtyExit),
    /// PTY output is closed; no more events will be emitted.
    Closed,
}

/// Portable child-process exit information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyExit {
    /// Platform exit code when one is available.
    pub code: Option<i32>,
    /// Whether the child reported successful completion.
    pub success: bool,
}

/// Terminal dimensions in character cells.
#[derive(Debug, Clone)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

impl Size {
    /// Reads the current terminal size, falling back to 80x24.
    pub fn from_terminal() -> Self {
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        Self { width, height }
    }
}

impl Default for Size {
    fn default() -> Self {
        Self::from_terminal()
    }
}

impl From<(u16, u16)> for Size {
    fn from((width, height): (u16, u16)) -> Self {
        Self { width, height }
    }
}

/// A command running inside a pseudo-terminal.
pub struct Process {
    tx: Sender<PtyEventResult>,
    cmd: Vec<OsString>,
    pty: Option<PtyPair>,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    envs: BTreeMap<String, String>,
    cwd: PathBuf,
    exit_thread: Option<std::thread::JoinHandle<()>>,
    exit_signal: ExitSignal,
}

impl Process {
    pub fn new(
        cmd: Vec<OsString>,
        tx: Sender<PtyEventResult>,
        size: Size,
        cwd: PathBuf,
    ) -> Result<Self> {
        let pty = create_pty(&size)?;

        Ok(Self {
            tx,
            cmd,
            pty: Some(pty),
            master: None,
            writer: None,
            envs: BTreeMap::new(),
            cwd,
            exit_signal: ExitSignal::new(),
            exit_thread: None,
        })
    }

    /// Spawns the child process and starts reader/exit watcher threads.
    pub fn start(&mut self) -> Result<()> {
        let pty = self.pty.take().expect("start() called twice");
        let portable_pty::PtyPair { slave, master } = pty;

        // The child gets its own slave handle. The parent must drop its copy
        // after taking master I/O handles, or the master reader may never see EOF.
        let child = slave.spawn_command(self.build_command())?;
        let reader = master.try_clone_reader()?;
        let writer = master
            .take_writer()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        drop(slave);

        self.writer = Some(writer);
        self.master = Some(master);

        // Start background readers only after spawn and handle setup succeed.
        let handle =
            spawn_process_watcher(child, reader, self.tx.clone(), self.exit_signal.clone());
        self.exit_thread = Some(handle);

        Ok(())
    }

    /// Returns a signal that becomes notified after the child exits.
    pub fn exit_signal(&self) -> ExitSignal {
        self.exit_signal.clone()
    }

    /// Sets environment variables for the child process.
    pub fn envs(&mut self, envs: BTreeMap<String, String>) {
        self.envs = envs;
    }

    /// Writes bytes to the PTY stdin.
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(0);
        };
        match writer.write_all(data) {
            Ok(()) => Ok(data.len()),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Resizes the PTY.
    pub fn resize(&mut self, size: Size) {
        if let Some(master) = &self.master {
            let _ = master.resize(PtySize {
                rows: size.height,
                cols: size.width,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    fn build_command(&self) -> CommandBuilder {
        let mut cmd = CommandBuilder::from_argv(self.cmd.clone());
        self.envs.iter().for_each(|(k, v)| cmd.env(k, v));
        cmd.cwd(self.cwd.clone());
        cmd
    }
}

fn spawn_process_watcher(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    tx: Sender<PtyEventResult>,
    exit_signal: ExitSignal,
) -> std::thread::JoinHandle<()> {
    spawn(move || {
        let reader_handle = spawn_reader(tx.clone(), reader);
        match child.wait() {
            Ok(status) => {
                let exit = PtyExit {
                    code: Some(status.exit_code() as i32),
                    success: status.success(),
                };
                exit_signal.notify();
                let _ = tx.send(Ok(PtyEvent::Exit(exit)));
            }
            Err(e) => {
                exit_signal.notify();
                let _ = tx.send(Err(anyhow::Error::new(e)));
            }
        }
        let _ = reader_handle.join();
        let _ = tx.send(Ok(PtyEvent::Closed));
    })
}

fn spawn_reader(
    tx: Sender<PtyEventResult>,
    mut reader: Box<dyn Read + Send>,
) -> std::thread::JoinHandle<()> {
    spawn(move || {
        let mut buf = [0; PTY_READ_BUFFER_SIZE];
        loop {
            match reader.read(&mut buf[..]) {
                Ok(0) => break,
                Ok(c) => {
                    if tx.send(Ok(PtyEvent::Output(buf[..c].to_vec()))).is_err() {
                        break;
                    }
                }
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(e) => {
                    let _ = tx.send(Err(anyhow::Error::new(e)));
                    break;
                }
            }
        }
    })
}

fn create_pty(size: &Size) -> Result<PtyPair> {
    let pty = native_pty_system();
    pty.openpty(PtySize {
        rows: size.height,
        cols: size.width,
        pixel_width: 0,
        pixel_height: 0,
    })
}
