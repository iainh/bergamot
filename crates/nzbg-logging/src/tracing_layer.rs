use std::sync::Arc;

use tracing::Subscriber;
use tracing_subscriber::Layer;

use crate::{LogBuffer, LogLevel, LogMessage};

pub struct BufferLayer {
    buffer: Arc<LogBuffer>,
}

impl BufferLayer {
    pub fn new(buffer: Arc<LogBuffer>) -> Self {
        Self { buffer }
    }
}

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => LogLevel::Error,
            tracing::Level::WARN => LogLevel::Warning,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::DEBUG => LogLevel::Detail,
            tracing::Level::TRACE => LogLevel::Debug,
        };

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        self.buffer.push(LogMessage {
            id: 0,
            kind: level,
            time: chrono::Utc::now(),
            text: visitor.message,
            nzb_id: None,
        });
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else if self.message.is_empty() {
            self.message = format!("{}: {value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else if self.message.is_empty() {
            self.message = format!("{}: {value}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn buffer_layer_captures_events() {
        let buffer = Arc::new(LogBuffer::new(100));
        let layer = BufferLayer::new(buffer.clone());

        let subscriber = tracing_subscriber::registry().with(layer);
        let _guard = tracing::subscriber::set_default(subscriber);

        tracing::info!("test message");
        tracing::warn!("warning message");

        let messages = buffer.messages_since(0);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].kind, LogLevel::Info);
        assert_eq!(messages[0].text, "test message");
        assert_eq!(messages[1].kind, LogLevel::Warning);
    }

    #[test]
    fn buffer_layer_maps_levels() {
        let buffer = Arc::new(LogBuffer::new(100));
        let layer = BufferLayer::new(buffer.clone());

        let subscriber = tracing_subscriber::registry().with(layer);
        let _guard = tracing::subscriber::set_default(subscriber);

        tracing::error!("err");
        tracing::debug!("dbg");

        let messages = buffer.messages_since(0);
        assert_eq!(messages[0].kind, LogLevel::Error);
        assert_eq!(messages[1].kind, LogLevel::Detail);
    }
}
