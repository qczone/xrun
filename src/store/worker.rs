//! One bounded queue owns the job database connection and all transactions.
use super::jobs::Database;
use anyhow::{Context, Result};
use std::sync::mpsc;
use tokio::sync::mpsc::{Sender, error::TrySendError};

const DATABASE_QUEUE_CAPACITY: usize = 64;
type Command = Box<dyn FnOnce(&mut Database) + Send>;
pub(super) struct Worker(Sender<Command>);
impl Worker {
    pub(super) fn start(mut database: Database) -> Result<Self> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Command>(DATABASE_QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("xrun-job-store".into())
            .spawn(move || {
                while let Some(command) = receiver.blocking_recv() {
                    command(&mut database);
                }
            })?;
        Ok(Self(sender))
    }
    pub(super) fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, result) = mpsc::sync_channel(1);
        let command: Command = Box::new(move |database| {
            let _ = reply.send(operation(database));
        });
        self.enqueue(command)?;
        result.recv().context("job database worker stopped")?
    }
    pub(super) fn enqueue(&self, mut command: Command) -> Result<()> {
        loop {
            match self.0.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Full(waiting)) => {
                    command = waiting;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(TrySendError::Closed(_)) => anyhow::bail!("job database worker stopped"),
            }
        }
        Ok(())
    }
    pub(super) async fn query<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, result) = tokio::sync::oneshot::channel();
        let command: Command = Box::new(move |database| {
            let _ = reply.send(operation(database));
        });
        self.0
            .send(command)
            .await
            .map_err(|_| anyhow::anyhow!("job database worker stopped"))?;
        result.await.context("job database worker stopped")?
    }
}
