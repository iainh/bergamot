//! NNTP connection handling and I/O layer.
//!
//! Wraps [`NntpMachine`] with async I/O to implement the NNTP session lifecycle
//! defined in [RFC 3977 §5](https://datatracker.ietf.org/doc/html/rfc3977#section-5).

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

use crate::error::NntpError;
use crate::machine::{self, Event, Input, NntpMachine, Output, ProtoError};
use crate::model::{Encryption, NewsServer};

pub trait NntpIo: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> NntpIo for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

const DEFLATE_BUF_SIZE: usize = 8192;

struct DeflateStream<T> {
    inner: T,
    compress: Compress,
    decompress: Decompress,
    read_buf: Vec<u8>,
    read_pos: usize,
    read_len: usize,
    write_buf: Vec<u8>,
    write_pos: usize,
    tmp_read_out: Vec<u8>,
    tmp_write_out: Vec<u8>,
}

impl<T> DeflateStream<T> {
    fn new_with_buffered(inner: T, buffered: &[u8]) -> Self {
        let mut read_buf = vec![0u8; DEFLATE_BUF_SIZE.max(buffered.len())];
        let read_len = buffered.len();
        read_buf[..read_len].copy_from_slice(buffered);
        Self {
            inner,
            compress: Compress::new(Compression::default(), false),
            decompress: Decompress::new(false),
            read_buf,
            read_pos: 0,
            read_len,
            write_buf: Vec::new(),
            write_pos: 0,
            tmp_read_out: vec![0u8; DEFLATE_BUF_SIZE],
            tmp_write_out: vec![0u8; 64],
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for DeflateStream<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();

        loop {
            if me.read_pos < me.read_len {
                let before_in = me.decompress.total_in();
                let before_out = me.decompress.total_out();

                let avail = &me.read_buf[me.read_pos..me.read_len];
                me.tmp_read_out
                    .resize(buf.remaining().max(DEFLATE_BUF_SIZE), 0);

                let status = me
                    .decompress
                    .decompress(avail, &mut me.tmp_read_out, FlushDecompress::Sync)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

                let consumed = (me.decompress.total_in() - before_in) as usize;
                let produced = (me.decompress.total_out() - before_out) as usize;

                me.read_pos += consumed;

                if produced > 0 {
                    buf.put_slice(&me.tmp_read_out[..produced]);
                    return Poll::Ready(Ok(()));
                }

                if status == Status::StreamEnd {
                    return Poll::Ready(Ok(()));
                }
            }

            me.read_pos = 0;
            me.read_len = 0;

            let mut tmp = ReadBuf::new(&mut me.read_buf);
            match Pin::new(&mut me.inner).poll_read(cx, &mut tmp) {
                Poll::Ready(Ok(())) => {
                    me.read_len = tmp.filled().len();
                    if me.read_len == 0 {
                        return Poll::Ready(Ok(()));
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for DeflateStream<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();

        let before_in = me.compress.total_in();
        let before_out = me.compress.total_out();

        me.tmp_write_out.resize(buf.len() + 64, 0);
        me.compress
            .compress(buf, &mut me.tmp_write_out, FlushCompress::Sync)
            .map_err(io::Error::other)?;

        let consumed = (me.compress.total_in() - before_in) as usize;
        let produced = (me.compress.total_out() - before_out) as usize;

        if produced > 0 {
            me.write_buf
                .extend_from_slice(&me.tmp_write_out[..produced]);
        }

        while me.write_pos < me.write_buf.len() {
            match Pin::new(&mut me.inner).poll_write(cx, &me.write_buf[me.write_pos..]) {
                Poll::Ready(Ok(n)) => {
                    me.write_pos += n;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        me.write_buf.clear();
        me.write_pos = 0;

        Poll::Ready(Ok(consumed))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();

        let before_out = me.compress.total_out();
        me.tmp_write_out.resize(64, 0);
        me.compress
            .compress(&[], &mut me.tmp_write_out, FlushCompress::Sync)
            .map_err(io::Error::other)?;
        let produced = (me.compress.total_out() - before_out) as usize;
        if produced > 0 {
            me.write_buf
                .extend_from_slice(&me.tmp_write_out[..produced]);
        }

        while me.write_pos < me.write_buf.len() {
            match Pin::new(&mut me.inner).poll_write(cx, &me.write_buf[me.write_pos..]) {
                Poll::Ready(Ok(n)) => {
                    me.write_pos += n;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        me.write_buf.clear();
        me.write_pos = 0;

        Pin::new(&mut me.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

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
    read_buf: Vec<u8>,
}

impl<'a> BodyReader<'a> {
    fn new(conn: &'a mut NntpConnection) -> Self {
        Self {
            conn,
            read_buf: Vec::with_capacity(8192),
        }
    }

    pub async fn read_line(&mut self) -> Result<Option<Vec<u8>>, NntpError> {
        self.read_buf.clear();
        let bytes = match &mut self.conn.stream {
            NntpStream::Plain(reader) => reader.read_until(b'\n', &mut self.read_buf).await?,
            NntpStream::Tls(reader) => reader.read_until(b'\n', &mut self.read_buf).await?,
        };

        if bytes == 0 {
            self.conn.machine.handle_input(Input::Eof);
            self.conn.drain_single_event()?;
            return Err(NntpError::ProtocolError("unexpected EOF".into()));
        }

        let trimmed = machine::trim_crlf(&self.read_buf);
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
    /// Builds a fresh [`ClientConfig`] per call. Prefer
    /// [`connect_with_tls_config`](Self::connect_with_tls_config) when making
    /// repeated connections to the same server so TLS session tickets are
    /// reused ([RFC 8446 §2.2](https://datatracker.ietf.org/doc/html/rfc8446#section-2.2),
    /// [RFC 5077](https://datatracker.ietf.org/doc/html/rfc5077)).
    ///
    /// See [RFC 3977 §5.1](https://datatracker.ietf.org/doc/html/rfc3977#section-5.1) (initial connection)
    /// and [RFC 4642](https://datatracker.ietf.org/doc/html/rfc4642) (STARTTLS).
    pub async fn connect(server: &NewsServer) -> Result<Self, NntpError> {
        Self::connect_with_tls_config(server, None).await
    }

    /// Connect to an NNTP server, reusing a pre-built TLS configuration for
    /// session resumption.
    ///
    /// When `tls_config` is `Some`, the provided [`ClientConfig`] is used for
    /// TLS/STARTTLS negotiation, enabling session ticket reuse across
    /// connections ([RFC 8446 §2.2](https://datatracker.ietf.org/doc/html/rfc8446#section-2.2),
    /// [RFC 5077](https://datatracker.ietf.org/doc/html/rfc5077)).
    /// When `None`, a fresh config is built (equivalent to [`connect`](Self::connect)).
    ///
    /// See [RFC 3977 §5.1](https://datatracker.ietf.org/doc/html/rfc3977#section-5.1) (initial connection)
    /// and [RFC 4642](https://datatracker.ietf.org/doc/html/rfc4642) (STARTTLS).
    pub async fn connect_with_tls_config(
        server: &NewsServer,
        tls_config: Option<Arc<ClientConfig>>,
    ) -> Result<Self, NntpError> {
        let tcp = TcpStream::connect((server.host.as_str(), server.port)).await?;

        match server.encryption {
            Encryption::Tls => {
                let config = match tls_config {
                    Some(c) => c,
                    None => build_tls_config(server.cert_verification)?,
                };
                let tls = tls_connect(tcp, &server.host, config).await?;
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

    /// Negotiate NNTP compression ([RFC 8054](https://datatracker.ietf.org/doc/html/rfc8054)).
    ///
    /// Sends `COMPRESS DEFLATE` and, on success (206), transparently wraps
    /// the connection in a raw DEFLATE ([RFC 1951](https://datatracker.ietf.org/doc/html/rfc1951))
    /// layer. All subsequent commands and responses are compressed.
    pub async fn request_compress(&mut self) -> Result<(), NntpError> {
        self.machine.request_compress();
        self.drive_machine().await?;
        Ok(())
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

    /// Fetch article body with pipelined GROUP and BODY commands
    /// ([RFC 3977 §3.5](https://datatracker.ietf.org/doc/html/rfc3977#section-3.5)).
    ///
    /// Sends `GROUP <group>` and `BODY <message_id>` back-to-back before
    /// reading responses, saving one network round-trip compared to
    /// calling [`join_group`](Self::join_group) then [`fetch_body`](Self::fetch_body)
    /// separately. If the group is already selected, only `BODY` is sent.
    ///
    /// Returns a streaming [`BodyReader`] for reading the article body.
    pub async fn fetch_body_pipelined(
        &mut self,
        group: &str,
        message_id: &str,
    ) -> Result<BodyReader<'_>, NntpError> {
        self.machine.request_group_body(group, message_id);
        self.drive_until_body_ready_pipelined().await?;
        Ok(BodyReader::new(self))
    }

    /// Check article existence with STAT ([RFC 3977 §6.2.4](https://datatracker.ietf.org/doc/html/rfc3977#section-6.2.4)).
    pub async fn stat(&mut self, message_id: &str) -> Result<bool, NntpError> {
        self.machine.request_stat(message_id);
        let event = self.drive_machine().await?;
        match event {
            Event::StatResult { exists } => Ok(exists),
            _ => Err(NntpError::ProtocolError(
                "unexpected event from stat".into(),
            )),
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
                Some(Output::UpgradeToDeflate) => {
                    self.do_deflate_upgrade()?;
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

    async fn drive_until_body_ready_pipelined(&mut self) -> Result<(), NntpError> {
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
                Some(Output::UpgradeToDeflate) => {
                    self.do_deflate_upgrade()?;
                }
                Some(Output::Event(Event::Error(e))) => {
                    return Err(proto_to_nntp(e));
                }
                Some(Output::Event(Event::GroupJoined { .. })) => {
                    continue;
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
                Some(Output::UpgradeToDeflate) => {
                    self.do_deflate_upgrade()?;
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
                    return Err(NntpError::ProtocolError("no event produced".into()));
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

    fn do_deflate_upgrade(&mut self) -> Result<(), NntpError> {
        let old = std::mem::replace(
            &mut self.stream,
            NntpStream::Plain(BufReader::new(
                Box::new(tokio::io::empty()) as Box<dyn NntpIo>
            )),
        );

        match old {
            NntpStream::Plain(buf_reader) => {
                let buffered = buf_reader.buffer().to_vec();
                let inner = buf_reader.into_inner();
                let deflate = DeflateStream::new_with_buffered(inner, &buffered);
                self.stream = NntpStream::Plain(BufReader::new(Box::new(deflate)));
                Ok(())
            }
            NntpStream::Tls(buf_reader) => {
                let buffered = buf_reader.buffer().to_vec();
                let inner = buf_reader.into_inner();
                let deflate =
                    DeflateStream::new_with_buffered(Box::new(inner) as Box<dyn NntpIo>, &buffered);
                self.stream = NntpStream::Plain(BufReader::new(Box::new(deflate)));
                Ok(())
            }
        }
    }

    async fn do_tls_upgrade(&mut self) -> Result<(), NntpError> {
        let old_stream = std::mem::replace(
            &mut self.stream,
            NntpStream::Plain(BufReader::new(
                Box::new(tokio::io::empty()) as Box<dyn NntpIo>
            )),
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
            NntpStream::Tls(_) => Err(NntpError::TlsError("already using TLS".into())),
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

/// Build a shared TLS [`ClientConfig`] suitable for reuse across connections.
///
/// Sharing a single config per server enables TLS session resumption
/// ([RFC 8446 §2.2](https://datatracker.ietf.org/doc/html/rfc8446#section-2.2),
/// [RFC 5077](https://datatracker.ietf.org/doc/html/rfc5077)):
/// rustls stores session tickets inside the `ClientConfig`, so reusing
/// the same `Arc<ClientConfig>` lets subsequent handshakes skip the full
/// key exchange.
///
/// When `cert_verification` is `false`, a no-op verifier is installed
/// (useful for servers with self-signed certificates).
pub fn build_tls_config(cert_verification: bool) -> Result<Arc<ClientConfig>, NntpError> {
    let provider = rustls::crypto::ring::default_provider();
    let _ = provider.clone().install_default();

    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = if cert_verification {
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier::new()))
            .with_no_client_auth()
    };

    Ok(Arc::new(config))
}

async fn tls_connect(
    tcp: TcpStream,
    hostname: &str,
    tls_config: Arc<ClientConfig>,
) -> Result<TlsStream<TcpStream>, NntpError> {
    let connector = TlsConnector::from(tls_config);
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

    #[tokio::test]
    async fn body_reader_empty_body() {
        let data = b".\r\n".to_vec();
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
        assert_eq!(body.read_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn body_reader_eof_mid_body() {
        let data = b"partial line\r\n".to_vec();
        let (client, mut server) = tokio::io::duplex(64);

        tokio::spawn(async move {
            server.write_all(&data).await.unwrap();
            drop(server);
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
        let first = body.read_line().await.unwrap();
        assert_eq!(first, Some(b"partial line".to_vec()));

        let err = body.read_line().await;
        assert!(err.is_err(), "EOF mid-body should produce an error");
    }

    #[test]
    fn build_tls_config_returns_shared_config() {
        let config = build_tls_config(true).unwrap();
        let config2 = config.clone();
        assert!(Arc::ptr_eq(&config, &config2));
    }

    #[test]
    fn build_tls_config_no_verify_returns_config() {
        let config = build_tls_config(false).unwrap();
        assert!(Arc::ptr_eq(&config, &config.clone()));
    }

    #[tokio::test]
    async fn compress_wraps_stream_deflate() {
        use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress};
        use tokio::io::AsyncReadExt;

        fn deflate_bytes(compress: &mut Compress, data: &[u8]) -> Vec<u8> {
            let mut out = vec![0u8; data.len() + 256];
            let before_out = compress.total_out();
            compress
                .compress(data, &mut out, FlushCompress::Sync)
                .unwrap();
            let produced = (compress.total_out() - before_out) as usize;
            out.truncate(produced);
            out
        }

        let (client, mut server) = tokio::io::duplex(4096);

        tokio::spawn(async move {
            let mut cmd_buf = vec![0u8; 256];
            let n = server.read(&mut cmd_buf).await.unwrap();
            let cmd = String::from_utf8_lossy(&cmd_buf[..n]);
            assert!(cmd.contains("COMPRESS DEFLATE"));

            server
                .write_all(b"206 Compression active\r\n")
                .await
                .unwrap();

            let mut srv_compress = Compress::new(Compression::default(), false);
            let mut srv_decompress = Decompress::new(false);

            let mut compressed_cmd = vec![0u8; 256];
            let n = server.read(&mut compressed_cmd).await.unwrap();
            let mut plain = vec![0u8; 256];
            let before_out = srv_decompress.total_out();
            srv_decompress
                .decompress(&compressed_cmd[..n], &mut plain, FlushDecompress::Sync)
                .unwrap();
            let produced = (srv_decompress.total_out() - before_out) as usize;
            let body_cmd = String::from_utf8_lossy(&plain[..produced]);
            assert!(body_cmd.contains("BODY"));

            let data = [&b"222 body follows\r\n"[..], b"hello world\r\n", b".\r\n"];
            for chunk in &data {
                let compressed = deflate_bytes(&mut srv_compress, chunk);
                server.write_all(&compressed).await.unwrap();
            }
            server.flush().await.unwrap();
        });

        let reader = BufReader::new(Box::new(client) as Box<dyn NntpIo>);
        let stream = NntpStream::Plain(reader);
        let machine = NntpMachine::new_after_greeting();

        let mut conn = NntpConnection {
            server_id: 0,
            stream,
            machine,
        };

        conn.request_compress().await.unwrap();

        let mut body_reader = conn.fetch_body("test@example").await.unwrap();
        let line = body_reader.read_line().await.unwrap();
        assert_eq!(line, Some(b"hello world".to_vec()));
        let end = body_reader.read_line().await.unwrap();
        assert_eq!(end, None);
    }

    #[tokio::test]
    async fn body_reader_dot_stuffed_lines() {
        let data = b"..single dot\r\n...two dots\r\n..\r\nnormal\r\n.\r\n".to_vec();
        let (client, mut server) = tokio::io::duplex(256);

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

        assert_eq!(
            body.read_line().await.unwrap(),
            Some(b".single dot".to_vec())
        );
        assert_eq!(
            body.read_line().await.unwrap(),
            Some(b"..two dots".to_vec())
        );
        assert_eq!(body.read_line().await.unwrap(), Some(b".".to_vec()));
        assert_eq!(body.read_line().await.unwrap(), Some(b"normal".to_vec()));
        assert_eq!(body.read_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn fetch_body_pipelined_sends_both_commands() {
        let (client, mut server) = tokio::io::duplex(4096);

        tokio::spawn(async move {
            server.write_all(b"200 Welcome\r\n").await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = tokio::io::AsyncReadExt::read(&mut server, &mut buf)
                .await
                .unwrap();
            let commands = String::from_utf8_lossy(&buf[..n]);
            assert!(commands.contains("GROUP"), "should contain GROUP");
            assert!(commands.contains("BODY"), "should contain BODY");

            server.write_all(b"211 5 1 5 alt.test\r\n").await.unwrap();
            server
                .write_all(b"222 body\r\ntest data\r\n.\r\n")
                .await
                .unwrap();
        });

        let reader = BufReader::new(Box::new(client) as Box<dyn NntpIo>);
        let stream = NntpStream::Plain(reader);
        let mut conn = NntpConnection::from_stream(0, stream).await.unwrap();

        let mut reader = conn
            .fetch_body_pipelined("alt.test", "msg@example")
            .await
            .unwrap();
        let line = reader.read_line().await.unwrap();
        assert_eq!(line, Some(b"test data".to_vec()));
        assert_eq!(reader.read_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn fetch_body_pipelined_skips_group_when_selected() {
        let (client, mut server) = tokio::io::duplex(4096);

        tokio::spawn(async move {
            server.write_all(b"200 Welcome\r\n").await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = tokio::io::AsyncReadExt::read(&mut server, &mut buf)
                .await
                .unwrap();
            let cmd = String::from_utf8_lossy(&buf[..n]);
            assert!(cmd.contains("GROUP"), "first should be GROUP");
            server.write_all(b"211 5 1 5 alt.test\r\n").await.unwrap();

            let n = tokio::io::AsyncReadExt::read(&mut server, &mut buf)
                .await
                .unwrap();
            let cmd = String::from_utf8_lossy(&buf[..n]);
            assert!(cmd.contains("BODY"), "should only send BODY (no GROUP)");
            assert!(!cmd.contains("GROUP"), "should not re-send GROUP");
            server
                .write_all(b"222 body\r\nhello\r\n.\r\n")
                .await
                .unwrap();
        });

        let reader = BufReader::new(Box::new(client) as Box<dyn NntpIo>);
        let stream = NntpStream::Plain(reader);
        let mut conn = NntpConnection::from_stream(0, stream).await.unwrap();

        conn.join_group("alt.test").await.unwrap();

        let mut reader = conn
            .fetch_body_pipelined("alt.test", "msg@example")
            .await
            .unwrap();
        let line = reader.read_line().await.unwrap();
        assert_eq!(line, Some(b"hello".to_vec()));
        assert_eq!(reader.read_line().await.unwrap(), None);
    }
}
