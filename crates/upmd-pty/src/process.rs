//! Process management for commands running inside a pseudo-terminal.
//!
//! A [`Process`] owns the PTY master side and spawns a child process attached
//! to the slave side. Output is read from the master on a background thread and
//! emitted as [`PtyEvent::Output`] byte chunks. A watcher waits for the child to
//! exit, closes the master handles, and lets the reader finish before publishing
//! the exit status and [`PtyEvent::Closed`].
//!
//! After spawning the child, the parent drops its copy of the slave handle. The
//! child retains its terminal connection. Releasing the parent's copy lets the
//! Unix reader reach EOF and releases the slave's reference to ConPTY.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread::spawn,
};

use crate::signal::ExitSignal;
use anyhow::Result;
use crossbeam_channel::Sender;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtyPair, PtySize};

pub type PtyEventResult = Result<PtyEvent>;

const PTY_READ_BUFFER_SIZE: usize = 32 * 1024;

#[derive(Default)]
struct PtyHandles {
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
}

impl PtyHandles {
    fn close(&self) {
        // Release the locks before dropping the handles. On older Windows,
        // closing ConPTY can block until pending output is consumed.
        let writer = self.writer.lock().unwrap().take();
        drop(writer);
        let master = self.master.lock().unwrap().take();
        drop(master);
    }
}

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

impl From<Size> for PtySize {
    fn from(size: Size) -> Self {
        Self {
            rows: size.height,
            cols: size.width,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// A command running inside a pseudo-terminal.
pub struct Process {
    tx: Sender<PtyEventResult>,
    cmd: Vec<OsString>,
    pty: Option<PtyPair>,
    handles: Arc<PtyHandles>,
    envs: BTreeMap<String, String>,
    cwd: PathBuf,
    exit_signal: ExitSignal,
}

impl Process {
    pub fn new(
        cmd: Vec<OsString>,
        tx: Sender<PtyEventResult>,
        size: Size,
        cwd: PathBuf,
    ) -> Result<Self> {
        let pty = native_pty_system().openpty(size.into())?;

        Ok(Self {
            tx,
            cmd,
            pty: Some(pty),
            handles: Arc::default(),
            envs: BTreeMap::new(),
            cwd,
            exit_signal: ExitSignal::new(),
        })
    }

    /// Runs the configured command in its PTY and begins delivering events.
    pub fn start(&mut self) -> Result<()> {
        let PtyPair { slave, master } = self.pty.take().expect("start() called twice");
        let reader = master.try_clone_reader()?;
        *self.handles.writer.lock().unwrap() = Some(master.take_writer()?);
        *self.handles.master.lock().unwrap() = Some(master);

        // Start the reader before spawning the child so it can answer ConPTY's startup queries.
        let tx = self.tx.clone();
        let handles = self.handles.clone();
        let reader = spawn(move || {
            if let Err(e) = read_output(reader, &tx, &handles) {
                let _ = tx.send(Err(e.into()));
            }
        });

        // Drop the parent's slave handle after spawning so it can't keep the PTY open.
        let child = slave.spawn_command(self.build_command());
        drop(slave);
        let child = child.inspect_err(|_| self.handles.close())?;
        self.spawn_exit_watcher(child, reader);
        Ok(())
    }

    fn spawn_exit_watcher(
        &self,
        mut child: Box<dyn portable_pty::Child + Send + Sync>,
        reader: std::thread::JoinHandle<()>,
    ) {
        let handles = self.handles.clone();
        let tx = self.tx.clone();
        let exit = self.exit_signal.clone();

        spawn(move || {
            let status = child.wait();
            exit.notify();
            // Close the master so the ConPTY output reader can reach EOF.
            handles.close();
            let _ = reader.join();
            let event = status.map(|status| {
                PtyEvent::Exit(PtyExit {
                    code: Some(status.exit_code() as i32),
                    success: status.success(),
                })
            });
            let _ = tx.send(event.map_err(Into::into));
            let _ = tx.send(Ok(PtyEvent::Closed));
        });
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
        let mut guard = self.handles.writer.lock().unwrap();
        let Some(writer) = guard.as_mut() else {
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
        if let Some(master) = self.handles.master.lock().unwrap().as_ref() {
            let _ = master.resize(size.into());
        }
    }

    fn build_command(&self) -> CommandBuilder {
        let mut cmd = CommandBuilder::from_argv(self.cmd.clone());
        self.envs.iter().for_each(|(k, v)| cmd.env(k, v));
        cmd.cwd(self.cwd.clone());
        cmd
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.handles.close();
    }
}

/// State for answering ConPTY startup queries before the app can consume output.
/// Buffers startup bytes across reads so forwarding them cannot stall child startup.
#[cfg(any(windows, test))]
#[derive(Default)]
struct ConptyStartup {
    pending: Vec<u8>,
    setup: Vec<u8>,
    complete: bool,
}

#[cfg(any(windows, test))]
impl ConptyStartup {
    #[cfg(windows)]
    fn handle_output(&mut self, chunk: &[u8], handles: &PtyHandles) -> std::io::Result<Vec<u8>> {
        self.consume(chunk, |reply| {
            let mut writer = handles.writer.lock().unwrap();
            let Some(writer) = writer.as_mut() else {
                return Ok(());
            };
            match writer.write_all(reply).and_then(|()| writer.flush()) {
                // Keep draining output so the watcher can report the child's exit status.
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                result => result,
            }
        })
    }

    fn consume(
        &mut self,
        chunk: &[u8],
        mut reply: impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<Vec<u8>> {
        const CURSOR: &[u8] = b"\x1b[6n";
        const ATTRIBUTES: &[u8] = b"\x1b[c";
        const FOCUS: &[u8] = b"\x1b[?1004h";
        const WIN32_INPUT: &[u8] = b"\x1b[?9001h";
        const SEQUENCES: [&[u8]; 4] = [CURSOR, ATTRIBUTES, FOCUS, WIN32_INPUT];

        if self.complete {
            return Ok(chunk.to_vec());
        }
        if chunk.is_empty() {
            return Ok(self.finish(chunk));
        }
        for (i, &byte) in chunk.iter().enumerate() {
            self.pending.push(byte);
            let seq = self.pending.as_slice();
            match seq {
                CURSOR => {
                    // Report the cursor at row 1, column 1.
                    reply(b"\x1b[1;1R")?;
                }
                ATTRIBUTES => {
                    // Identify the terminal as a basic VT100.
                    reply(b"\x1b[?1;0c")?;
                    self.pending.clear();
                    return Ok(self.finish(&chunk[i + 1..]));
                }
                FOCUS | WIN32_INPUT if self.setup.len() < 64 => {
                    self.setup.append(&mut self.pending)
                }
                _ if SEQUENCES
                    .iter()
                    .any(|s| s.len() > seq.len() && s.starts_with(seq)) =>
                {
                    continue
                }
                _ => return Ok(self.finish(&chunk[i + 1..])),
            }
            self.pending.clear();
        }
        Ok(Vec::new())
    }

    fn finish(&mut self, tail: &[u8]) -> Vec<u8> {
        self.complete = true;
        self.setup.append(&mut self.pending);
        self.setup.extend_from_slice(tail);
        std::mem::take(&mut self.setup)
    }
}

fn read_output(
    mut reader: impl Read,
    tx: &Sender<PtyEventResult>,
    _handles: &PtyHandles,
) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut startup = ConptyStartup::default();
    let mut buf = [0; PTY_READ_BUFFER_SIZE];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            #[cfg(windows)]
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::EIO) => 0,
            Err(e) => return Err(e),
        };
        #[cfg(windows)]
        let bytes = startup.handle_output(&buf[..n], _handles)?;
        #[cfg(not(windows))]
        let bytes = buf[..n].to_vec();
        if !bytes.is_empty() && tx.send(Ok(PtyEvent::Output(bytes))).is_err() {
            return Ok(());
        }
        if n == 0 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conpty_startup_preserves_output_and_answers_queries() {
        type Case = (&'static [u8], &'static [u8], &'static [u8], &'static [u8]);
        let cases: &[Case] = &[
            (
                b"\x1b[?9001h\x1b[6n\x1b[c\x1b[?1004hhello\x1b[6n",
                b"\x1b[1;1R\x1b[?1;0c",
                b"\x1b[?9001h\x1b[?1004hhello\x1b[6n",
                b"",
            ),
            (b"prompt> ", b"", b"prompt> ", b""),
            (b"\x1b[", b"", b"", b"\x1b["),
            (b"\x1b[3;4H\x1b[c", b"", b"\x1b[3;4H\x1b[c", b""),
        ];
        for &(input, expected_replies, expected_output, expected_tail) in cases {
            for width in 1..=input.len() {
                let mut startup = ConptyStartup::default();
                let mut replies = Vec::new();
                let mut output = Vec::new();
                for chunk in input.chunks(width) {
                    let bytes = startup
                        .consume(chunk, |reply| {
                            replies.extend_from_slice(reply);
                            Ok(())
                        })
                        .unwrap();
                    if !bytes.is_empty() {
                        assert_eq!(replies, expected_replies);
                    }
                    output.extend(bytes);
                }
                assert_eq!(replies, expected_replies);
                assert_eq!(output, expected_output);
                let tail = startup
                    .consume(&[], |_| panic!("unexpected reply"))
                    .unwrap();
                assert_eq!(tail, expected_tail);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn dropping_process_unblocks_child_waiting_on_stdin() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let cmd = vec![
            OsString::from("sh"),
            OsString::from("-c"),
            OsString::from("read line"),
        ];
        let mut proc = Process::new(
            cmd,
            tx,
            Size::from((80, 24)),
            std::env::current_dir().unwrap(),
        )
        .unwrap();
        proc.start().unwrap();
        let exit_signal = proc.exit_signal();

        drop(proc);

        let start = std::time::Instant::now();
        while !exit_signal.is_done() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(2),
                "child blocked on read did not exit after Process was dropped"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
