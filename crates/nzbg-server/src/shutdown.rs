#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    tx: tokio::sync::watch::Sender<bool>,
}

impl ShutdownHandle {
    pub fn new() -> (Self, tokio::sync::watch::Receiver<bool>) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (Self { tx }, rx)
    }

    pub fn trigger(&self) {
        let _ = self.tx.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_sets_shutdown_flag() {
        let (handle, rx) = ShutdownHandle::new();
        assert!(!*rx.borrow());
        handle.trigger();
        assert!(*rx.borrow());
    }
}
