use std::{ffi::OsString, path::PathBuf, thread::spawn, time::Duration};

use anyhow::Result;
use crossbeam_channel::{after, bounded, select, Sender};
use upmd_pty::PtyEvent;

use crate::{apps::config::Envs, pty::stream::Stream};

pub use upmd_pty::Size;

pub struct Process {
    inner: upmd_pty::Process,
    tx: Sender<Stream>,
    state: upmd_runner::StateCapture,
    state_done_tx: Sender<()>,
}

impl Process {
    pub fn new(
        cmd: Vec<OsString>,
        tx: Sender<Stream>,
        size: Size,
        cwd: PathBuf,
        state: upmd_runner::StateCapture,
    ) -> Result<Self> {
        // Keep this rendezvous-sized so the extracted `upmd-pty` reader cannot
        // build a second backlog ahead of the app's real stream channel. Fast
        // producers like `yes` need backpressure here; otherwise Ctrl-C reaches
        // the PTY but shutdown still waits behind queued output in this bridge.
        let (pty_tx, pty_rx) = bounded(0);
        let inner = upmd_pty::Process::new(cmd, pty_tx, size, cwd)?;
        let stream_tx = tx.clone();
        // Handshake channel: the state-capture thread signals when it has
        // queued Env/Cwd messages, so we can send End only after that.
        let (state_done_tx, state_done_rx) = bounded(1);
        let reader_state_done_rx = state_done_rx;

        // Detach the PTY bridge thread; it exits on PtyEvent::Closed or when
        // the PTY/channel is dropped, so no join handle is needed.
        let _ = spawn(move || {
            // On PTY close, wait (with timeout) for the state-capture thread to
            // signal that Env/Cwd have been queued, then send End.
            while let Ok(event) = pty_rx.recv() {
                match event {
                    Ok(PtyEvent::Output(bytes)) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        if stream_tx.send(Stream::Out(text)).is_err() {
                            break;
                        }
                    }
                    Ok(PtyEvent::Exit(exit)) => {
                        // Legacy Stream::Exit cannot represent unknown status.
                        let code = exit.code.unwrap_or(-1);
                        let _ = stream_tx.send(Stream::Exit(code));
                    }
                    Ok(PtyEvent::Closed) => {
                        // Wait (with timeout) for state capture to finish,
                        // ensuring Env/Cwd arrive before End.
                        let timeout = after(Duration::from_millis(500));
                        select! {
                            recv(reader_state_done_rx) -> _ => {}
                            recv(timeout) -> _ => {}
                        }
                        let _ = stream_tx.send(Stream::End);
                        break;
                    }
                    Err(err) => {
                        let _ = stream_tx.send(Stream::Out(err.to_string()));
                        let _ = stream_tx.send(Stream::Exit(-1));
                        let _ = stream_tx.send(Stream::End);
                        break;
                    }
                }
            }
        });

        Ok(Self {
            inner,
            tx,
            state,
            state_done_tx,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        self.inner.start()?;
        // Start FIFO readers only after child spawn succeeds.
        spawn_state_capture(
            self.state.dir.clone(),
            self.state.fifos.take(),
            self.tx.clone(),
            self.inner.exit_signal(),
            self.state_done_tx.clone(),
        );
        Ok(())
    }

    pub fn envs(&mut self, envs: Envs) {
        self.inner.envs(envs);
    }

    pub fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.inner.write(data)
    }

    pub fn resize(&mut self, size: Size) {
        self.inner.resize(size);
    }
}

fn spawn_state_capture(
    state_dir: PathBuf,
    state_fifos: Option<upmd_runner::FifoPaths>,
    tx: Sender<Stream>,
    exit: upmd_pty::ExitSignal,
    done_tx: Sender<()>,
) {
    spawn(move || {
        crate::pty::state::read_state(state_dir, state_fifos, tx, exit);
        // Signal the PTY reader that state has been queued.
        let _ = done_tx.send(());
    });
}
