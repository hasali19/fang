use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tracing::debug;

type Id = OsString;

pub struct Poller {
    sender: mpsc::Sender<PollerCommand>,
}

enum PollerCommand {
    Register {
        id: Id,
        interval: Duration,
        handler: Box<dyn FnMut() + Send>,
    },
    Unregister {
        id: Id,
    },
}

impl Poller {
    pub fn spawn() -> Poller {
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let mut tasks: HashMap<Id, (Duration, Box<dyn FnMut() + Send>, Instant)> =
                HashMap::new();

            loop {
                let next_wakeup = tasks.values().map(|(_, _, wake_at)| wake_at).copied().min();

                let cmd = if let Some(next_wakeup) = next_wakeup {
                    match receiver.recv_timeout(next_wakeup - Instant::now()) {
                        Ok(cmd) => Some(cmd),
                        Err(e) => match e {
                            mpsc::RecvTimeoutError::Timeout => None,
                            mpsc::RecvTimeoutError::Disconnected => break,
                        },
                    }
                } else {
                    match receiver.recv() {
                        Ok(cmd) => Some(cmd),
                        Err(_) => break,
                    }
                };

                let now = Instant::now();

                for (interval, handler, wake_at) in tasks.values_mut() {
                    if now >= *wake_at {
                        handler();
                        *wake_at = now + *interval;
                    }
                }

                if let Some(cmd) = cmd {
                    match cmd {
                        PollerCommand::Register {
                            id,
                            interval,
                            handler,
                        } => {
                            debug!(id = %id.display(), ?interval, "Registering poller");
                            tasks.insert(id, (interval, handler, now + interval));
                        }
                        PollerCommand::Unregister { id } => {
                            debug!(id = %id.display(), "Unregistering poller");
                            tasks.remove(&id);
                        }
                    }
                }
            }

            debug!("Poller thread is exiting");
        });

        Poller { sender }
    }

    pub fn register(&self, id: Id, interval: Duration, handler: Box<dyn FnMut() + Send>) {
        self.sender
            .send(PollerCommand::Register {
                id,
                interval,
                handler,
            })
            .expect("Poller thread must be running");
    }

    pub fn unregister(&self, id: Id) {
        self.sender
            .send(PollerCommand::Unregister { id })
            .expect("Poller thread must be running");
    }
}
