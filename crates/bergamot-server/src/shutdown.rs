use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    token: CancellationToken,
}

impl ShutdownHandle {
    pub fn new() -> (Self, CancellationToken) {
        let token = CancellationToken::new();
        (
            Self {
                token: token.clone(),
            },
            token,
        )
    }

    pub fn trigger(&self) {
        self.token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_sets_shutdown_flag() {
        let (handle, token) = ShutdownHandle::new();
        assert!(!token.is_cancelled());
        handle.trigger();
        assert!(token.is_cancelled());
    }
}
