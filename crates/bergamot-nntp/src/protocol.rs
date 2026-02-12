//! NNTP connection handling and I/O layer.
//!
//! Wraps [`NntpMachine`] with async I/O to implement the NNTP session lifecycle
//! defined in [RFC 3977 §5](https://datatracker.ietf.org/doc/html/rfc3977#section-5).

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

use crate::error::NntpError;
use crate::machine::{self, Event, Input, NntpMachine, Output, ProtoError};
use crate::model::{Encryption, NewsServer};

pub trait NntpIo: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> NntpIo for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

pub enum NntpStream {
    Plain(BufReader<Box<dyn NntpIo>>),
    Tls(Box<BufReader<TlsStream<TcpStream>>>),
}

pub struct NntpConnection {
    pub server_id: u32,
    stream: NntpStream,
    machine: NntpMachine,
}

pub struct BodyReader<'a> {
    conn: &'a mut NntpConnection,
}

impl<'a> BodyReader<'a> {
    fn new(conn: &'a mut NntpConnection) -> Self {
        Self { conn }
    }

    pub async fn read_line(&mut self) -> Result<Option<Vec<u8>>, NntpError> {
        // First check if there's a pending NeedBodyLine from a previous call.
        // If the machine has already emitted NeedBodyLine, we proceed to read.
        // If not (e.g. BodyEnd was already emitted), we should not read more.

        let mut buf = Vec::new();
        let bytes = match &mut self.conn.stream {
            NntpStream::Plain(reader) => reader.read_until(b'\n', &mut buf).await?,
            NntpStream::Tls(reader) => reader.read_until(b'\n', &mut buf).await?,
        };

        if bytes == 0 {
            self.conn.machine.handle_input(Input::Eof);
            self.conn.drain_single_event()?;
            return Err(NntpError::ProtocolError("unexpected EOF".into()));
        }

        let trimmed = machine::trim_crlf(&buf);
        if machine::is_body_terminator(trimmed) {
            self.conn.machine.handle_input(Input::BodyEnd);
            self.conn.drain_single_event()?;
            return Ok(None);
        }

        self.conn.machine.handle_input(Input::BodyLine(trimmed));
        self.conn.drain_body_chunk_data()
    }
}

impl NntpConnection {
    pub async fn from_stream(server_id: u32, stream: NntpStream) -> Result<Self, NntpError> {
        let machine = NntpMachine::new();
        let mut conn = NntpConnection {
            server_id,
            stream,
            machine,
        };
        conn.drive_machine().await?;
        Ok(conn)
    }

    /// Connect to an NNTP server, handling TLS/STARTTLS negotiation.
    ///
    /// See [RFC 3977 §5.1](https://datatracker.ietf.org/doc/html/rfc3977#section-5.1) (initial connection)
    /// and [RFC 4642](https://datatracker.ietf.org/doc/html/rfc4642) (STARTTLS).
    pub async fn connect(server: &NewsServer) -> Result<Self, NntpError> {
        let tcp = TcpStream::connect((server.host.as_str(), server.port)).await?;

        match server.encryption {
            Encryption::Tls => {
                let tls = tls_connect(tcp, &server.host, server.cert_verification).await?;
                let stream = NntpStream::Tls(Box::new(BufReader::new(tls)));
                Self::from_stream(server.id, stream).await
            }
            Encryption::StartTls => {
                let stream = NntpStream::Plain(BufReader::new(Box::new(tcp)));
                let mut conn = NntpConnection {
                    server_id: server.id,
                    stream,
                    machine: NntpMachine::new(),
                };
                conn.drive_machine().await?;

                conn.machine.request_starttls();
                conn.drive_machine().await?;

                Ok(conn)
            }
            Encryption::None => {
                let stream = NntpStream::Plain(BufReader::new(Box::new(tcp)));
                Self::from_stream(server.id, stream).await
            }
        }
    }

    /// Authenticate with AUTHINFO USER/PASS ([RFC 4643 §2.3](https://datatracker.ietf.org/doc/html/rfc4643#section-2.3)).
    pub async fn authenticate(&mut self, username: &str, password: &str) -> Result<(), NntpError> {
        self.machine.request_auth(username, password);
        self.drive_machine().await?;
        Ok(())
    }

    /// Select a newsgroup ([RFC 3977 §6.1.1](https://datatracker.ietf.org/doc/html/rfc3977#section-6.1.1)).
    pub async fn join_group(&mut self, group: &str) -> Result<(), NntpError> {
        self.machine.request_group(group);
        self.drive_machine().await?;
        Ok(())
    }

    /// Fetch article body ([RFC 3977 §6.2.3](https://datatracker.ietf.org/doc/html/rfc3977#section-6.2.3)).
    /// Returns a streaming reader that performs dot-unstuffing ([RFC 3977 §3.1.1](https://datatracker.ietf.org/doc/html/rfc3977#section-3.1.1)).
    pub async fn fetch_body(&mut self, message_id: &str) -> Result<BodyReader<'_>, NntpError> {
        self.machine.request_body(message_id);
        self.drive_until_body_ready().await?;
        Ok(BodyReader::new(self))
    }

    /// Check article existence with STAT ([RFC 3977 §6.2.4](https://datatracker.ietf.org/doc/html/rfc3977#section-6.2.4)).
    pub async fn stat(&mut self, message_id: &str) -> Result<bool, NntpError> {
        self.machine.request_stat(message_id);
        let event = self.drive_machine().await?;
        match event {
            Event::StatResult { exists } => Ok(exists),
            _ => Err(NntpError::ProtocolError("unexpected event from stat".into())),
        }
    }

    /// Gracefully close the connection ([RFC 3977 §5.4](https://datatracker.ietf.org/doc/html/rfc3977#section-5.4)).
    pub async fn quit(&mut self) -> Result<(), NntpError> {
        self.machine.request_quit();
        let _ = self.drive_machine().await;
        Ok(())
    }

    async fn drive_machine(&mut self) -> Result<Event, NntpError> {
        loop {
            match self.machine.poll_output() {
                Some(Output::SendCommand(cmd)) => {
                    self.send_raw(&cmd).await?;
                }
                Some(Output::NeedResponseLine) => {
                    let line = self.read_response_line().await?;
                    self.machine.handle_input(Input::ResponseLine(&line));
                }
                Some(Output::NeedBodyLine) => {
                    return Err(NntpError::ProtocolError(
                        "unexpected NeedBodyLine in drive_machine".into(),
                    ));
                }
                Some(Output::UpgradeToTls) => {
                    self.do_tls_upgrade().await?;
                }
                Some(Output::Event(Event::Error(e))) => {
                    return Err(proto_to_nntp(e));
                }
                Some(Output::Event(event)) => {
                    return Ok(event);
                }
                None => {
                    return Err(NntpError::ProtocolError(
                        "machine produced no output".into(),
                    ));
                }
            }
        }
    }

    async fn drive_until_body_ready(&mut self) -> Result<(), NntpError> {
        loop {
            match self.machine.poll_output() {
                Some(Output::SendCommand(cmd)) => {
                    self.send_raw(&cmd).await?;
                }
                Some(Output::NeedResponseLine) => {
                    let line = self.read_response_line().await?;
                    self.machine.handle_input(Input::ResponseLine(&line));
                }
                Some(Output::NeedBodyLine) => {
                    return Ok(());
                }
                Some(Output::UpgradeToTls) => {
                    self.do_tls_upgrade().await?;
                }
                Some(Output::Event(Event::Error(e))) => {
                    return Err(proto_to_nntp(e));
                }
                Some(Output::Event(_)) => {
                    return Err(NntpError::ProtocolError(
                        "unexpected event while waiting for body".into(),
                    ));
                }
                None => {
                    return Err(NntpError::ProtocolError(
                        "machine produced no output".into(),
                    ));
                }
            }
        }
    }

    fn drain_single_event(&mut self) -> Result<Event, NntpError> {
        loop {
            match self.machine.poll_output() {
                Some(Output::Event(Event::Error(e))) => return Err(proto_to_nntp(e)),
                Some(Output::Event(event)) => return Ok(event),
                Some(Output::NeedBodyLine) => continue,
                Some(other) => {
                    return Err(NntpError::ProtocolError(format!(
                        "unexpected output while draining event: {other:?}"
                    )));
                }
                None => {
                    return Err(NntpError::ProtocolError(
                        "no event produced".into(),
                    ));
                }
            }
        }
    }

    fn drain_body_chunk_data(&mut self) -> Result<Option<Vec<u8>>, NntpError> {
        let mut result = None;
        while let Some(output) = self.machine.poll_output() {
            match output {
                Output::Event(Event::BodyChunk(data)) => {
                    result = Some(data);
                }
                Output::Event(Event::BodyEnd) => return Ok(None),
                Output::Event(Event::Error(e)) => return Err(proto_to_nntp(e)),
                Output::NeedBodyLine => {
                    break;
                }
                other => {
                    return Err(NntpError::ProtocolError(format!(
                        "unexpected output in body drain: {other:?}"
                    )));
                }
            }
        }
        Ok(result)
    }

    async fn send_raw(&mut self, cmd: &str) -> Result<(), NntpError> {
        let line = format!("{cmd}\r\n");
        match &mut self.stream {
            NntpStream::Plain(s) => s.get_mut().write_all(line.as_bytes()).await?,
            NntpStream::Tls(s) => s.get_mut().write_all(line.as_bytes()).await?,
        }
        Ok(())
    }

    async fn read_response_line(&mut self) -> Result<String, NntpError> {
        let mut line = String::new();
        match &mut self.stream {
            NntpStream::Plain(s) => s.read_line(&mut line).await?,
            NntpStream::Tls(s) => s.read_line(&mut line).await?,
        };
        if line.is_empty() {
            return Err(NntpError::ProtocolError("empty response".into()));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
        Ok(trimmed)
    }

    async fn do_tls_upgrade(&mut self) -> Result<(), NntpError> {
        let old_stream = std::mem::replace(
            &mut self.stream,
            NntpStream::Plain(BufReader::new(Box::new(tokio::io::empty())
                as Box<dyn NntpIo>)),
        );

        match old_stream {
            NntpStream::Plain(buf_reader) => {
                let inner = buf_reader.into_inner();
                let tcp: Box<dyn NntpIo> = inner;
                let _ = tcp;
                Err(NntpError::TlsError(
                    "STARTTLS upgrade requires raw TcpStream; use NntpConnection::connect instead"
                        .into(),
                ))
            }
            NntpStream::Tls(_) => {
                Err(NntpError::TlsError(
                    "already using TLS".into(),
                ))
            }
        }
    }
}

fn proto_to_nntp(e: ProtoError) -> NntpError {
    match e {
        ProtoError::AuthFailed(msg) => NntpError::AuthFailed(msg),
        ProtoError::AuthRequired => NntpError::AuthRequired,
        ProtoError::ArticleNotFound(msg) => NntpError::ArticleNotFound(msg),
        ProtoError::UnexpectedResponse(code, msg) => NntpError::UnexpectedResponse(code, msg),
        ProtoError::ProtocolError(msg) => NntpError::ProtocolError(msg),
    }
}

async fn tls_connect(
    tcp: TcpStream,
    hostname: &str,
    verify_cert: bool,
) -> Result<TlsStream<TcpStream>, NntpError> {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = if verify_cert {
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier::new()))
            .with_no_client_auth()
    };

    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(hostname.to_string())
        .map_err(|_| NntpError::TlsError(format!("invalid hostname: {hostname}")))?;

    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| NntpError::TlsError(e.to_string()))?;

    Ok(tls_stream)
}

#[derive(Debug)]
struct NoVerifier {
    supported_schemes: Vec<rustls::SignatureScheme>,
}

impl NoVerifier {
    fn new() -> Self {
        let schemes = rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes();
        Self {
            supported_schemes: schemes,
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.supported_schemes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn body_reader_unstuffs_and_terminates() {
        let data = b"line1\r\n..dot\r\n.\r\n".to_vec();
        let (client, mut server) = tokio::io::duplex(64);

        tokio::spawn(async move {
            server.write_all(&data).await.unwrap();
        });

        let reader = BufReader::new(Box::new(client) as Box<dyn NntpIo>);
        let stream = NntpStream::Plain(reader);
        let machine = NntpMachine::new_after_greeting();

        let mut conn = NntpConnection {
            server_id: 0,
            stream,
            machine,
        };
        conn.machine.request_body("test@example");

        while let Some(output) = conn.machine.poll_output() {
            match output {
                Output::SendCommand(_) => {}
                Output::NeedResponseLine => break,
                _ => {}
            }
        }
        conn.machine
            .handle_input(Input::ResponseLine("222 body follows"));
        while let Some(output) = conn.machine.poll_output() {
            match output {
                Output::NeedBodyLine => break,
                Output::Event(_) => {}
                _ => {}
            }
        }

        let mut body = BodyReader::new(&mut conn);

        assert_eq!(body.read_line().await.unwrap(), Some(b"line1".to_vec()));
        assert_eq!(body.read_line().await.unwrap(), Some(b".dot".to_vec()));
        assert_eq!(body.read_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn read_response_parses_code_and_message() {
        let resp = machine::parse_response("200 Hello there").unwrap();
        assert_eq!(resp.code, 200);
        assert_eq!(resp.message, "Hello there");
    }

    #[tokio::test]
    async fn read_response_rejects_invalid_line() {
        let err = machine::parse_response("oops").expect_err("should fail");
        match err {
            ProtoError::ProtocolError(message) => {
                assert!(message.contains("invalid response"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
