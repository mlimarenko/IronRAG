use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct ShutdownSignal {
    tx: broadcast::Sender<()>,
}

impl ShutdownSignal {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(8);
        Self { tx }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }

    pub fn trigger(&self) {
        let _ = self.tx.send(());
    }
}
