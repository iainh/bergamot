use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

use crate::error::NntpError;
use crate::model::{Encryption, NewsServer, NntpResponse};

pub(crate) trait NntpIo: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> NntpIo for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

pub enum NntpStream {
    Plain(BufReader<Box<dyn NntpIo>>),
    Tls(Box<BufReader<TlsStream<TcpStream>>>),
}

pub struct NntpConnection {
    pub server_id: u32,
    stream: NntpStream,
    current_group: Option<String>,
    authenticated: bool,
}

pub struct BodyReader<'a> {
    stream: &'a mut NntpStream,
    buffer: String,
    done: bool,
}

impl<'a> BodyReader<'a> {
    fn new(stream: &'a mut NntpStream) -> Self {
        Self {
            stream,
            buffer: String::new(),
            done: false,
        }
    }

    pub async fn read_line(&mut self) -> Result<Option<String>, NntpError> {
        if self.done {
            return Ok(None);
        }

        self.buffer.clear();
        let bytes = match &mut self.stream {
            NntpStream::Plain(reader) => reader.read_line(&mut self.buffer).await?,
            NntpStream::Tls(reader) => reader.read_line(&mut self.buffer).await?,
        };

        if bytes == 0 {
            return Err(NntpError::ProtocolError("unexpected EOF".into()));
        }

        let trimmed = self.buffer.trim_end_matches(['\r', '\n']);
        if trimmed == "." {
            self.done = true;
            return Ok(None);
        }

        let line = if trimmed.starts_with("..") {
            trimmed[1..].to_string()
        } else {
            trimmed.to_string()
        };

        Ok(Some(line))
    }
}

impl NntpConnection {
    pub async fn connect(server: &NewsServer) -> Result<Self, NntpError> {
        let tcp = TcpStream::connect((server.host.as_str(), server.port)).await?;

        let stream = match server.encryption {
            Encryption::Tls => {
                let tls = tls_connect(tcp, &server.host, server.cert_verification).await?;
                NntpStream::Tls(Box::new(BufReader::new(tls)))
            }
            Encryption::StartTls => {
                let mut plain = BufReader::new(tcp);
                let greeting = read_response(&mut plain).await?;
                expect_code(&greeting, &[200, 201])?;
                send_command(&mut plain, "STARTTLS").await?;
                let resp = read_response(&mut plain).await?;
                expect_code(&resp, &[382])?;
                let tcp = plain.into_inner();
                let tls = tls_connect(tcp, &server.host, server.cert_verification).await?;
                NntpStream::Tls(Box::new(BufReader::new(tls)))
            }
            Encryption::None => NntpStream::Plain(BufReader::new(Box::new(tcp))),
        };

        let mut conn = NntpConnection {
            server_id: server.id,
            stream,
            current_group: None,
            authenticated: false,
        };

        if server.encryption != Encryption::StartTls {
            let greeting = conn.read_response().await?;
            expect_code(&greeting, &[200, 201])?;
        }

        Ok(conn)
    }

    pub async fn authenticate(&mut self, username: &str, password: &str) -> Result<(), NntpError> {
        self.send_command(&format!("AUTHINFO USER {username}"))
            .await?;
        let resp = self.read_response().await?;

        match resp.code {
            281 => {
                self.authenticated = true;
                return Ok(());
            }
            381 => {}
            _ => return Err(NntpError::AuthFailed(resp.message)),
        }

        self.send_command(&format!("AUTHINFO PASS {password}"))
            .await?;
        let resp = self.read_response().await?;
        match resp.code {
            281 => {
                self.authenticated = true;
                Ok(())
            }
            _ => Err(NntpError::AuthFailed(resp.message)),
        }
    }

    pub async fn join_group(&mut self, group: &str) -> Result<(), NntpError> {
        if self.current_group.as_deref() == Some(group) {
            return Ok(());
        }
        self.send_command(&format!("GROUP {group}")).await?;
        let resp = self.read_response().await?;
        expect_code(&resp, &[211])?;
        self.current_group = Some(group.to_string());
        Ok(())
    }

    pub async fn fetch_body(&mut self, message_id: &str) -> Result<BodyReader<'_>, NntpError> {
        self.send_command(&format!("BODY <{message_id}>")).await?;
        let resp = self.read_response().await?;

        match resp.code {
            222 => Ok(BodyReader::new(&mut self.stream)),
            430 => Err(NntpError::ArticleNotFound(message_id.to_string())),
            480 => Err(NntpError::AuthRequired),
            _ => Err(NntpError::UnexpectedResponse(resp.code, resp.message)),
        }
    }

    pub async fn stat(&mut self, message_id: &str) -> Result<bool, NntpError> {
        self.send_command(&format!("STAT <{message_id}>")).await?;
        let resp = self.read_response().await?;
        match resp.code {
            223 => Ok(true),
            430 => Ok(false),
            _ => Err(NntpError::UnexpectedResponse(resp.code, resp.message)),
        }
    }

    pub async fn quit(&mut self) -> Result<(), NntpError> {
        self.send_command("QUIT").await?;
        let _ = self.read_response().await;
        Ok(())
    }

    async fn send_command(&mut self, cmd: &str) -> Result<(), NntpError> {
        let line = format!("{cmd}\r\n");
        match &mut self.stream {
            NntpStream::Plain(s) => s.get_mut().write_all(line.as_bytes()).await?,
            NntpStream::Tls(s) => s.get_mut().write_all(line.as_bytes()).await?,
        }
        Ok(())
    }

    async fn read_response(&mut self) -> Result<NntpResponse, NntpError> {
        match &mut self.stream {
            NntpStream::Plain(s) => read_response(s).await,
            NntpStream::Tls(s) => read_response(s).await,
        }
    }
}

async fn read_response<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
) -> Result<NntpResponse, NntpError> {
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    if line.is_empty() {
        return Err(NntpError::ProtocolError("empty response".into()));
    }

    let code = line
        .get(..3)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| NntpError::ProtocolError("invalid response line".into()))?;

    let message = line[3..].trim().to_string();
    Ok(NntpResponse { code, message })
}

fn expect_code(resp: &NntpResponse, expected: &[u16]) -> Result<(), NntpError> {
    if expected.contains(&resp.code) {
        Ok(())
    } else {
        Err(NntpError::UnexpectedResponse(
            resp.code,
            resp.message.clone(),
        ))
    }
}

async fn send_command<R: AsyncWriteExt + Unpin>(
    writer: &mut R,
    cmd: &str,
) -> Result<(), NntpError> {
    let line = format!("{cmd}\r\n");
    writer.write_all(line.as_bytes()).await?;
    Ok(())
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

    #[tokio::test]
    async fn body_reader_unstuffs_and_terminates() {
        let data = b"line1\r\n..dot\r\n.\r\n".to_vec();
        let cursor = std::io::Cursor::new(data);
        let (client, mut server) = tokio::io::duplex(64);
        let reader = BufReader::new(Box::new(client) as Box<dyn NntpIo>);

        tokio::spawn(async move {
            let _ = server.write_all(&cursor.into_inner()).await;
        });

        let mut stream = NntpStream::Plain(reader);

        let mut body = BodyReader::new(&mut stream);

        assert_eq!(body.read_line().await.unwrap(), Some("line1".to_string()));
        assert_eq!(body.read_line().await.unwrap(), Some(".dot".to_string()));
        assert_eq!(body.read_line().await.unwrap(), None);
        assert_eq!(body.read_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn read_response_parses_code_and_message() {
        let data = b"200 Hello there\r\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(data));

        let resp = read_response(&mut reader).await.unwrap();
        assert_eq!(resp.code, 200);
        assert_eq!(resp.message, "Hello there");
    }

    #[tokio::test]
    async fn read_response_rejects_invalid_line() {
        let data = b"oops\r\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(data));

        let err = read_response(&mut reader).await.expect_err("should fail");
        match err {
            NntpError::ProtocolError(message) => {
                assert!(message.contains("invalid response"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
