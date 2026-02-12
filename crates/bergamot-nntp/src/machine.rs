//! Pure state machine for the NNTP protocol ([RFC 3977](https://datatracker.ietf.org/doc/html/rfc3977)).
//!
//! Drives command/response exchanges without performing I/O directly.

use std::collections::VecDeque;

use crate::model::NntpResponse;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input<'a> {
    ResponseLine(&'a str),
    BodyLine(&'a [u8]),
    BodyEnd,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    SendCommand(String),
    NeedResponseLine,
    NeedBodyLine,
    UpgradeToTls,
    UpgradeToDeflate,
    Event(Event),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    AuthFailed(String),
    AuthRequired,
    ArticleNotFound(String),
    UnexpectedResponse(u16, String),
    ProtocolError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    GreetingOk { code: u16 },
    Authenticated,
    GroupJoined { group: String },
    StatResult { exists: bool },
    BodyChunk(Vec<u8>),
    BodyEnd,
    CompressActive,
    QuitAck,
    Error(ProtoError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitGreeting,
    Idle,
    AwaitAuthUser,
    AwaitAuthPass,
    AwaitGroup,
    AwaitStat,
    AwaitBodyResponse,
    ReadingBody,
    AwaitStartTlsResponse,
    AwaitStartTlsGreeting,
    AwaitPipelinedGroupResponse,
    AwaitPipelinedBodyResponse,
    AwaitCompressResponse,
    AwaitQuit,
    Done,
}

#[derive(Debug)]
pub struct NntpMachine {
    state: State,
    outputs: VecDeque<Output>,
    current_group: Option<String>,
    pending_group: Option<String>,
    pending_password: Option<String>,
    pending_message_id: Option<String>,
    authenticated: bool,
}

impl NntpMachine {
    pub fn new() -> Self {
        let mut m = Self {
            state: State::AwaitGreeting,
            outputs: VecDeque::new(),
            current_group: None,
            pending_group: None,
            pending_password: None,
            pending_message_id: None,
            authenticated: false,
        };
        m.outputs.push_back(Output::NeedResponseLine);
        m
    }

    pub fn new_after_greeting() -> Self {
        Self {
            state: State::Idle,
            outputs: VecDeque::new(),
            current_group: None,
            pending_group: None,
            pending_password: None,
            pending_message_id: None,
            authenticated: false,
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    pub fn current_group(&self) -> Option<&str> {
        self.current_group.as_deref()
    }

    /// Initiate STARTTLS upgrade ([RFC 4642 §2.7](https://datatracker.ietf.org/doc/html/rfc4642#section-2.7)).
    pub fn request_starttls(&mut self) {
        self.outputs
            .push_back(Output::SendCommand("STARTTLS".to_string()));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitStartTlsResponse;
    }

    /// Negotiate NNTP compression ([RFC 8054](https://datatracker.ietf.org/doc/html/rfc8054)).
    pub fn request_compress(&mut self) {
        self.outputs
            .push_back(Output::SendCommand("COMPRESS DEFLATE".to_string()));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitCompressResponse;
    }

    /// Authenticate with AUTHINFO USER/PASS ([RFC 4643 §2.3](https://datatracker.ietf.org/doc/html/rfc4643#section-2.3)).
    pub fn request_auth(&mut self, user: &str, pass: &str) {
        self.pending_password = Some(pass.to_string());
        self.outputs
            .push_back(Output::SendCommand(format!("AUTHINFO USER {user}")));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitAuthUser;
    }

    /// Select a newsgroup with GROUP ([RFC 3977 §6.1.1](https://datatracker.ietf.org/doc/html/rfc3977#section-6.1.1)).
    /// Skips the command if the group is already selected.
    pub fn request_group(&mut self, group: &str) {
        if self.current_group.as_deref() == Some(group) {
            self.outputs.push_back(Output::Event(Event::GroupJoined {
                group: group.to_string(),
            }));
            return;
        }
        self.pending_group = Some(group.to_string());
        self.outputs
            .push_back(Output::SendCommand(format!("GROUP {group}")));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitGroup;
    }

    /// Check article existence with STAT ([RFC 3977 §6.2.4](https://datatracker.ietf.org/doc/html/rfc3977#section-6.2.4)).
    pub fn request_stat(&mut self, message_id: &str) {
        self.outputs
            .push_back(Output::SendCommand(format!("STAT <{message_id}>")));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitStat;
    }

    /// Pipeline GROUP and BODY commands ([RFC 3977 §3.5](https://datatracker.ietf.org/doc/html/rfc3977#section-3.5)).
    ///
    /// Sends both commands back-to-back before reading any responses,
    /// saving one network round-trip compared to sequential execution.
    /// If the group is already selected, falls back to a simple BODY request.
    pub fn request_group_body(&mut self, group: &str, message_id: &str) {
        if self.current_group.as_deref() == Some(group) {
            self.request_body(message_id);
            return;
        }
        self.pending_group = Some(group.to_string());
        self.pending_message_id = Some(message_id.to_string());
        self.outputs
            .push_back(Output::SendCommand(format!("GROUP {group}")));
        self.outputs
            .push_back(Output::SendCommand(format!("BODY <{message_id}>")));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitPipelinedGroupResponse;
    }

    /// Fetch article body with BODY ([RFC 3977 §6.2.3](https://datatracker.ietf.org/doc/html/rfc3977#section-6.2.3)).
    pub fn request_body(&mut self, message_id: &str) {
        self.pending_message_id = Some(message_id.to_string());
        self.outputs
            .push_back(Output::SendCommand(format!("BODY <{message_id}>")));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitBodyResponse;
    }

    /// Close the connection with QUIT ([RFC 3977 §5.4](https://datatracker.ietf.org/doc/html/rfc3977#section-5.4)).
    pub fn request_quit(&mut self) {
        self.outputs
            .push_back(Output::SendCommand("QUIT".to_string()));
        self.outputs.push_back(Output::NeedResponseLine);
        self.state = State::AwaitQuit;
    }

    pub fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }

    pub fn handle_input(&mut self, input: Input<'_>) {
        match (&self.state, input) {
            (State::Done, _) => {}

            (_, Input::Eof) => {
                self.emit_error(ProtoError::ProtocolError("unexpected EOF".into()));
                self.state = State::Done;
            }

            (State::AwaitGreeting, Input::ResponseLine(line)) => match parse_response(line) {
                Ok(resp) => {
                    if resp.code == 200 || resp.code == 201 {
                        self.outputs
                            .push_back(Output::Event(Event::GreetingOk { code: resp.code }));
                        self.state = State::Idle;
                    } else {
                        self.emit_error(ProtoError::UnexpectedResponse(resp.code, resp.message));
                        self.state = State::Done;
                    }
                }
                Err(e) => {
                    self.emit_error(e);
                    self.state = State::Done;
                }
            },

            (State::AwaitAuthUser, Input::ResponseLine(line)) => match parse_response(line) {
                Ok(resp) => match resp.code {
                    281 => {
                        self.authenticated = true;
                        self.pending_password = None;
                        self.outputs.push_back(Output::Event(Event::Authenticated));
                        self.state = State::Idle;
                    }
                    381 => {
                        if let Some(pass) = self.pending_password.take() {
                            self.outputs
                                .push_back(Output::SendCommand(format!("AUTHINFO PASS {pass}")));
                            self.outputs.push_back(Output::NeedResponseLine);
                            self.state = State::AwaitAuthPass;
                        } else {
                            self.emit_error(ProtoError::AuthFailed(
                                "password required but not provided".into(),
                            ));
                            self.state = State::Done;
                        }
                    }
                    _ => {
                        self.pending_password = None;
                        self.emit_error(ProtoError::AuthFailed(resp.message));
                        self.state = State::Done;
                    }
                },
                Err(e) => {
                    self.pending_password = None;
                    self.emit_error(e);
                    self.state = State::Done;
                }
            },

            (State::AwaitAuthPass, Input::ResponseLine(line)) => match parse_response(line) {
                Ok(resp) => match resp.code {
                    281 => {
                        self.authenticated = true;
                        self.outputs.push_back(Output::Event(Event::Authenticated));
                        self.state = State::Idle;
                    }
                    _ => {
                        self.emit_error(ProtoError::AuthFailed(resp.message));
                        self.state = State::Done;
                    }
                },
                Err(e) => {
                    self.emit_error(e);
                    self.state = State::Done;
                }
            },

            (State::AwaitGroup, Input::ResponseLine(line)) => match parse_response(line) {
                Ok(resp) => {
                    if resp.code == 211 {
                        let group = self.pending_group.take().unwrap_or_default();
                        self.current_group = Some(group.clone());
                        self.outputs
                            .push_back(Output::Event(Event::GroupJoined { group }));
                        self.state = State::Idle;
                    } else {
                        let group = self.pending_group.take();
                        self.emit_error(ProtoError::UnexpectedResponse(resp.code, resp.message));
                        let _ = group;
                        self.state = State::Idle;
                    }
                }
                Err(e) => {
                    self.pending_group = None;
                    self.emit_error(e);
                    self.state = State::Done;
                }
            },

            (State::AwaitStat, Input::ResponseLine(line)) => match parse_response(line) {
                Ok(resp) => match resp.code {
                    223 => {
                        self.outputs
                            .push_back(Output::Event(Event::StatResult { exists: true }));
                        self.state = State::Idle;
                    }
                    430 => {
                        self.outputs
                            .push_back(Output::Event(Event::StatResult { exists: false }));
                        self.state = State::Idle;
                    }
                    _ => {
                        self.emit_error(ProtoError::UnexpectedResponse(resp.code, resp.message));
                        self.state = State::Idle;
                    }
                },
                Err(e) => {
                    self.emit_error(e);
                    self.state = State::Done;
                }
            },

            (State::AwaitPipelinedGroupResponse, Input::ResponseLine(line)) => {
                match parse_response(line) {
                    Ok(resp) => {
                        if resp.code == 211 {
                            let group = self.pending_group.take().unwrap_or_default();
                            self.current_group = Some(group.clone());
                            self.outputs
                                .push_back(Output::Event(Event::GroupJoined { group }));
                            self.outputs.push_back(Output::NeedResponseLine);
                            self.state = State::AwaitPipelinedBodyResponse;
                        } else {
                            self.pending_group = None;
                            self.pending_message_id = None;
                            self.emit_error(ProtoError::UnexpectedResponse(
                                resp.code,
                                resp.message,
                            ));
                            self.state = State::Done;
                        }
                    }
                    Err(e) => {
                        self.pending_group = None;
                        self.pending_message_id = None;
                        self.emit_error(e);
                        self.state = State::Done;
                    }
                }
            }

            (State::AwaitPipelinedBodyResponse, Input::ResponseLine(line)) => {
                match parse_response(line) {
                    Ok(resp) => match resp.code {
                        222 => {
                            self.outputs.push_back(Output::NeedBodyLine);
                            self.state = State::ReadingBody;
                        }
                        430 => {
                            let msg_id = self
                                .pending_message_id
                                .take()
                                .unwrap_or_else(|| "unknown".to_string());
                            self.emit_error(ProtoError::ArticleNotFound(msg_id));
                            self.state = State::Idle;
                        }
                        480 => {
                            self.pending_message_id = None;
                            self.emit_error(ProtoError::AuthRequired);
                            self.state = State::Idle;
                        }
                        _ => {
                            self.pending_message_id = None;
                            self.emit_error(ProtoError::UnexpectedResponse(
                                resp.code,
                                resp.message,
                            ));
                            self.state = State::Idle;
                        }
                    },
                    Err(e) => {
                        self.pending_message_id = None;
                        self.emit_error(e);
                        self.state = State::Done;
                    }
                }
            }

            (State::AwaitBodyResponse, Input::ResponseLine(line)) => match parse_response(line) {
                Ok(resp) => match resp.code {
                    222 => {
                        self.outputs.push_back(Output::NeedBodyLine);
                        self.state = State::ReadingBody;
                    }
                    430 => {
                        let msg_id = self
                            .pending_message_id
                            .take()
                            .unwrap_or_else(|| "unknown".to_string());
                        self.emit_error(ProtoError::ArticleNotFound(msg_id));
                        self.state = State::Idle;
                    }
                    480 => {
                        self.pending_message_id = None;
                        self.emit_error(ProtoError::AuthRequired);
                        self.state = State::Idle;
                    }
                    _ => {
                        self.pending_message_id = None;
                        self.emit_error(ProtoError::UnexpectedResponse(resp.code, resp.message));
                        self.state = State::Idle;
                    }
                },
                Err(e) => {
                    self.pending_message_id = None;
                    self.emit_error(e);
                    self.state = State::Done;
                }
            },

            (State::ReadingBody, Input::BodyEnd) => {
                self.pending_message_id = None;
                self.outputs.push_back(Output::Event(Event::BodyEnd));
                self.state = State::Idle;
            }

            // Dot-unstuffing per RFC 3977 §3.1.1
            // <https://datatracker.ietf.org/doc/html/rfc3977#section-3.1.1>
            (State::ReadingBody, Input::BodyLine(data)) => {
                let unstuffed = if data.starts_with(b"..") {
                    data[1..].to_vec()
                } else {
                    data.to_vec()
                };
                self.outputs
                    .push_back(Output::Event(Event::BodyChunk(unstuffed)));
                self.outputs.push_back(Output::NeedBodyLine);
            }

            (State::AwaitStartTlsResponse, Input::ResponseLine(line)) => {
                match parse_response(line) {
                    Ok(resp) => {
                        if resp.code == 382 {
                            self.outputs.push_back(Output::UpgradeToTls);
                            self.outputs.push_back(Output::NeedResponseLine);
                            self.state = State::AwaitStartTlsGreeting;
                        } else {
                            self.emit_error(ProtoError::UnexpectedResponse(
                                resp.code,
                                resp.message,
                            ));
                            self.state = State::Done;
                        }
                    }
                    Err(e) => {
                        self.emit_error(e);
                        self.state = State::Done;
                    }
                }
            }

            (State::AwaitStartTlsGreeting, Input::ResponseLine(line)) => {
                match parse_response(line) {
                    Ok(resp) => {
                        if resp.code == 200 || resp.code == 201 {
                            self.outputs
                                .push_back(Output::Event(Event::GreetingOk { code: resp.code }));
                            self.state = State::Idle;
                        } else {
                            self.emit_error(ProtoError::UnexpectedResponse(
                                resp.code,
                                resp.message,
                            ));
                            self.state = State::Done;
                        }
                    }
                    Err(e) => {
                        self.emit_error(e);
                        self.state = State::Done;
                    }
                }
            }

            (State::AwaitCompressResponse, Input::ResponseLine(line)) => {
                match parse_response(line) {
                    Ok(resp) => {
                        if resp.code == 206 {
                            self.outputs.push_back(Output::UpgradeToDeflate);
                            self.outputs.push_back(Output::Event(Event::CompressActive));
                            self.state = State::Idle;
                        } else {
                            self.emit_error(ProtoError::UnexpectedResponse(
                                resp.code,
                                resp.message,
                            ));
                            self.state = State::Idle;
                        }
                    }
                    Err(e) => {
                        self.emit_error(e);
                        self.state = State::Idle;
                    }
                }
            }

            (State::AwaitQuit, Input::ResponseLine(_)) => {
                self.outputs.push_back(Output::Event(Event::QuitAck));
                self.state = State::Done;
            }

            (State::Idle, Input::ResponseLine(line)) => {
                self.emit_error(ProtoError::ProtocolError(format!(
                    "unexpected response while idle: {line}"
                )));
            }

            (state, input) => {
                self.emit_error(ProtoError::ProtocolError(format!(
                    "unexpected input {input:?} in state {state:?}"
                )));
                self.state = State::Done;
            }
        }
    }

    fn emit_error(&mut self, err: ProtoError) {
        self.outputs.push_back(Output::Event(Event::Error(err)));
    }
}

impl Default for NntpMachine {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse_response(line: &str) -> Result<NntpResponse, ProtoError> {
    if line.len() < 3 {
        return Err(ProtoError::ProtocolError("invalid response line".into()));
    }
    let code = line[..3]
        .parse::<u16>()
        .map_err(|_| ProtoError::ProtocolError("invalid response line".into()))?;
    let message = line[3..].trim().to_string();
    Ok(NntpResponse { code, message })
}

pub fn is_body_terminator(line: &[u8]) -> bool {
    line == b"."
}

pub fn trim_crlf(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && buf[end - 1] == b'\r' {
        end -= 1;
    }
    &buf[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain_outputs(m: &mut NntpMachine) -> Vec<Output> {
        let mut out = Vec::new();
        while let Some(o) = m.poll_output() {
            out.push(o);
        }
        out
    }

    fn find_event(outputs: &[Output]) -> Option<&Event> {
        outputs.iter().find_map(|o| match o {
            Output::Event(e) => Some(e),
            _ => None,
        })
    }

    fn find_events(outputs: &[Output]) -> Vec<&Event> {
        outputs
            .iter()
            .filter_map(|o| match o {
                Output::Event(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn greeting_ok() {
        let mut m = NntpMachine::new();
        let out = drain_outputs(&mut m);
        assert_eq!(out, vec![Output::NeedResponseLine]);

        m.handle_input(Input::ResponseLine("200 Welcome"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::GreetingOk { code: 200 }));
    }

    #[test]
    fn greeting_201_posting_not_allowed() {
        let mut m = NntpMachine::new();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("201 No posting"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::GreetingOk { code: 201 }));
    }

    #[test]
    fn greeting_bad_code() {
        let mut m = NntpMachine::new();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("502 Go away"));
        let out = drain_outputs(&mut m);
        assert!(matches!(find_event(&out), Some(Event::Error(_))));
    }

    #[test]
    fn auth_success_281_immediate() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_auth("user", "pass");
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::SendCommand("AUTHINFO USER user".to_string())));
        assert!(out.contains(&Output::NeedResponseLine));

        m.handle_input(Input::ResponseLine("281 OK"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::Authenticated));
        assert!(m.is_authenticated());
    }

    #[test]
    fn auth_success_381_then_281() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_auth("user", "secret");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("381 More auth info"));
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::SendCommand("AUTHINFO PASS secret".to_string())));

        m.handle_input(Input::ResponseLine("281 OK"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::Authenticated));
        assert!(m.is_authenticated());
    }

    #[test]
    fn auth_failure() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_auth("user", "wrong");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("381 More auth"));
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("452 Auth failed"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::AuthFailed(_)))
        ));
    }

    #[test]
    fn group_join() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group("alt.binaries.test");
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::SendCommand("GROUP alt.binaries.test".to_string())));

        m.handle_input(Input::ResponseLine("211 1234 1 1234 alt.binaries.test"));
        let out = drain_outputs(&mut m);
        assert_eq!(
            find_event(&out),
            Some(&Event::GroupJoined {
                group: "alt.binaries.test".to_string()
            })
        );
        assert_eq!(m.current_group(), Some("alt.binaries.test"));
    }

    #[test]
    fn group_already_joined_skips_command() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group("alt.test");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("211 0 0 0 alt.test"));
        drain_outputs(&mut m);

        m.request_group("alt.test");
        let out = drain_outputs(&mut m);
        assert_eq!(
            find_event(&out),
            Some(&Event::GroupJoined {
                group: "alt.test".to_string()
            })
        );
        assert!(!out.iter().any(|o| matches!(o, Output::SendCommand(_))));
    }

    #[test]
    fn stat_found() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_stat("test@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("223 0 <test@example>"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::StatResult { exists: true }));
    }

    #[test]
    fn stat_not_found() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_stat("missing@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("430 No Such Article"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::StatResult { exists: false }));
    }

    #[test]
    fn body_fetch_success() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("222 body follows"));
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::NeedBodyLine));

        m.handle_input(Input::BodyLine(b"line1"));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(b"line1".to_vec()));
        assert!(out.contains(&Output::NeedBodyLine));

        m.handle_input(Input::BodyLine(b"..dot"));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(b".dot".to_vec()));

        m.handle_input(Input::BodyEnd);
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::BodyEnd));
    }

    #[test]
    fn body_not_found() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("missing@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("430 No Such Article"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::ArticleNotFound(_)))
        ));
    }

    #[test]
    fn body_auth_required() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("480 Auth required"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::AuthRequired))
        ));
    }

    #[test]
    fn quit() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_quit();
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::SendCommand("QUIT".to_string())));

        m.handle_input(Input::ResponseLine("205 bye"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::QuitAck));
    }

    #[test]
    fn starttls_flow() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_starttls();
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::SendCommand("STARTTLS".to_string())));

        m.handle_input(Input::ResponseLine("382 Begin TLS"));
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::UpgradeToTls));
        assert!(out.contains(&Output::NeedResponseLine));

        m.handle_input(Input::ResponseLine("200 Welcome (TLS)"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::GreetingOk { code: 200 }));
    }

    #[test]
    fn eof_produces_error() {
        let mut m = NntpMachine::new();
        drain_outputs(&mut m);
        m.handle_input(Input::Eof);
        let out = drain_outputs(&mut m);
        assert!(matches!(find_event(&out), Some(Event::Error(_))));
    }

    #[test]
    fn parse_response_valid() {
        let resp = parse_response("200 Hello there").unwrap();
        assert_eq!(resp.code, 200);
        assert_eq!(resp.message, "Hello there");
    }

    #[test]
    fn parse_response_code_only() {
        let resp = parse_response("200").unwrap();
        assert_eq!(resp.code, 200);
        assert_eq!(resp.message, "");
    }

    #[test]
    fn parse_response_invalid() {
        assert!(parse_response("xx").is_err());
        assert!(parse_response("").is_err());
    }

    #[test]
    fn trim_crlf_variants() {
        assert_eq!(trim_crlf(b"hello\r\n"), b"hello");
        assert_eq!(trim_crlf(b"hello\n"), b"hello");
        assert_eq!(trim_crlf(b"hello"), b"hello");
        assert_eq!(trim_crlf(b""), b"");
    }

    #[test]
    fn body_terminator_detection() {
        assert!(is_body_terminator(b"."));
        assert!(!is_body_terminator(b".."));
        assert!(!is_body_terminator(b"hello"));
        assert!(!is_body_terminator(b""));
    }

    #[test]
    fn full_session_greeting_auth_group_body_quit() {
        let mut m = NntpMachine::new();
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("200 Welcome"));
        drain_outputs(&mut m);

        m.request_auth("user", "pass");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("281 OK"));
        drain_outputs(&mut m);

        m.request_group("alt.test");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("211 0 0 0 alt.test"));
        drain_outputs(&mut m);

        m.request_body("msg@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body"));
        drain_outputs(&mut m);
        m.handle_input(Input::BodyLine(b"data"));
        drain_outputs(&mut m);
        m.handle_input(Input::BodyEnd);
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::BodyEnd));

        m.request_quit();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("205 bye"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::QuitAck));
    }

    // --- RFC 3977 §5.1: Greeting edge cases ---

    #[test]
    fn greeting_400_service_unavailable() {
        let mut m = NntpMachine::new();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("400 Service unavailable"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(400, _)))
        ));
    }

    #[test]
    fn greeting_201_posting_prohibited() {
        let mut m = NntpMachine::new();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("201 Posting prohibited"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::GreetingOk { code: 201 }));
    }

    #[test]
    fn greeting_malformed_line() {
        let mut m = NntpMachine::new();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("garbage"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::ProtocolError(_)))
        ));
    }

    // --- RFC 3977 §6.1.1: GROUP error responses ---

    #[test]
    fn group_411_no_such_group() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group("no.such.group");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("411 No such group"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(411, _)))
        ));
        assert_eq!(m.current_group(), None);
    }

    #[test]
    fn group_error_then_retry_succeeds() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group("no.such.group");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("411 No such group"));
        drain_outputs(&mut m);

        m.request_group("alt.binaries.test");
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::SendCommand("GROUP alt.binaries.test".to_string())));

        m.handle_input(Input::ResponseLine("211 100 1 100 alt.binaries.test"));
        let out = drain_outputs(&mut m);
        assert_eq!(
            find_event(&out),
            Some(&Event::GroupJoined {
                group: "alt.binaries.test".to_string()
            })
        );
        assert_eq!(m.current_group(), Some("alt.binaries.test"));
    }

    // --- RFC 3977 §6.2.4: STAT additional error codes ---

    #[test]
    fn stat_412_no_group_selected() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_stat("x@y");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("412 No newsgroup selected"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(412, _)))
        ));
    }

    #[test]
    fn stat_502_permission_denied() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_stat("x@y");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("502 Permission denied"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(502, _)))
        ));
    }

    // --- RFC 3977 §6.2.3: BODY additional error codes ---

    #[test]
    fn body_412_no_group_selected() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("x@y");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("412 No newsgroup selected"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(412, _)))
        ));
    }

    #[test]
    fn body_420_invalid_article_number() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("x@y");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("420 Current article number is invalid"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(420, _)))
        ));
    }

    #[test]
    fn body_502_permission_denied() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("x@y");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("502 Permission denied"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(502, _)))
        ));
    }

    // --- RFC 3977 §3.1.1: Dot-unstuffing edge cases ---

    #[test]
    fn body_empty() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        m.handle_input(Input::BodyEnd);
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::BodyEnd));
        let events = find_events(&out);
        assert!(
            !events.iter().any(|e| matches!(e, Event::BodyChunk(_))),
            "empty body should produce no BodyChunk events"
        );
    }

    #[test]
    fn body_unstuff_double_dot_to_single() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        m.handle_input(Input::BodyLine(b".."));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(b".".to_vec()));
    }

    #[test]
    fn body_unstuff_triple_dot_to_double() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        m.handle_input(Input::BodyLine(b"..."));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(b"..".to_vec()));
    }

    #[test]
    fn body_unstuff_quad_dot_to_triple() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        m.handle_input(Input::BodyLine(b"...."));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(b"...".to_vec()));
    }

    #[test]
    fn body_empty_line_preserved() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        m.handle_input(Input::BodyLine(b""));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(Vec::new()));
    }

    #[test]
    fn body_whitespace_line_unchanged() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        m.handle_input(Input::BodyLine(b"   "));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(b"   ".to_vec()));
    }

    #[test]
    fn body_line_not_starting_with_dot_unchanged() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        m.handle_input(Input::BodyLine(b"hello.world"));
        let out = drain_outputs(&mut m);
        let events = find_events(&out);
        assert_eq!(events[0], &Event::BodyChunk(b"hello.world".to_vec()));
    }

    #[test]
    fn body_dot_stuffed_content_mixed_with_regular() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);

        let lines: &[&[u8]] = &[
            b"normal line",
            b"..stuffed",
            b"another normal",
            b"...double",
        ];
        let expected: &[&[u8]] = &[b"normal line", b".stuffed", b"another normal", b"..double"];

        for (line, exp) in lines.iter().zip(expected.iter()) {
            m.handle_input(Input::BodyLine(line));
            let out = drain_outputs(&mut m);
            let events = find_events(&out);
            assert_eq!(events[0], &Event::BodyChunk(exp.to_vec()));
        }

        m.handle_input(Input::BodyEnd);
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::BodyEnd));
    }

    // --- RFC 8054: COMPRESS ---

    #[test]
    fn compress_sends_command_and_awaits_response() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_compress();
        let out = drain_outputs(&mut m);
        assert_eq!(out[0], Output::SendCommand("COMPRESS DEFLATE".to_string()));
        assert_eq!(out[1], Output::NeedResponseLine);
    }

    #[test]
    fn compress_206_emits_upgrade_to_deflate() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_compress();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("206 Compression active"));
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::UpgradeToDeflate));
        assert_eq!(find_event(&out), Some(&Event::CompressActive));
    }

    #[test]
    fn compress_403_emits_error() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_compress();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("403 Unable to activate compression"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(403, _)))
        ));
    }

    #[test]
    fn compress_malformed_response_errors() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_compress();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("garbage"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::ProtocolError(_)))
        ));
    }

    #[test]
    fn body_works_after_compress() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_compress();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("206 Compression active"));
        drain_outputs(&mut m);

        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);
        m.handle_input(Input::BodyLine(b"data"));
        drain_outputs(&mut m);
        m.handle_input(Input::BodyEnd);
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::BodyEnd));
    }

    // --- RFC 4643 §2.3: AUTHINFO edge cases ---

    #[test]
    fn auth_user_481_rejected() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_auth("user", "pass");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("481 Authentication failed"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::AuthFailed(_)))
        ));
    }

    #[test]
    fn auth_pass_481_rejected() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_auth("user", "wrong");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("381 Password required"));
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("481 Authentication failed"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::AuthFailed(_)))
        ));
    }

    #[test]
    fn auth_user_281_no_password_needed() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_auth("user", "pass");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("281 Authentication accepted"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::Authenticated));
        assert!(m.is_authenticated());
    }

    #[test]
    fn auth_user_502_unavailable() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_auth("user", "pass");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("502 Command unavailable"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::AuthFailed(_)))
        ));
    }

    // --- RFC 4642: STARTTLS error responses ---

    #[test]
    fn starttls_580_cannot_initiate() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_starttls();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("580 Can not initiate TLS negotiation"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(580, _)))
        ));
    }

    #[test]
    fn starttls_502_already_active() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_starttls();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine(
            "502 STARTTLS not allowed with active TLS layer",
        ));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(502, _)))
        ));
    }

    #[test]
    fn starttls_bad_post_upgrade_greeting() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_starttls();
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("382 Begin TLS"));
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("400 Service unavailable"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(400, _)))
        ));
    }

    // --- Idle state: unexpected input ---

    #[test]
    fn unsolicited_response_while_idle() {
        let mut m = NntpMachine::new_after_greeting();
        m.handle_input(Input::ResponseLine("200 Surprise"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::ProtocolError(_)))
        ));
    }

    // --- RFC 3977 §3.5: Pipelining ---

    #[test]
    fn pipelined_group_body_emits_two_commands_before_read() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group_body("alt.test", "msg@example");
        let out = drain_outputs(&mut m);
        assert_eq!(out[0], Output::SendCommand("GROUP alt.test".to_string()));
        assert_eq!(
            out[1],
            Output::SendCommand("BODY <msg@example>".to_string())
        );
        assert_eq!(out[2], Output::NeedResponseLine);
    }

    #[test]
    fn pipelined_group_body_happy_path() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group_body("alt.test", "msg@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("211 5 1 5 alt.test"));
        let out = drain_outputs(&mut m);
        assert_eq!(
            find_event(&out),
            Some(&Event::GroupJoined {
                group: "alt.test".to_string()
            })
        );
        assert!(out.contains(&Output::NeedResponseLine));

        m.handle_input(Input::ResponseLine("222 body follows"));
        let out = drain_outputs(&mut m);
        assert!(out.contains(&Output::NeedBodyLine));

        m.handle_input(Input::BodyLine(b"data"));
        drain_outputs(&mut m);
        m.handle_input(Input::BodyEnd);
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::BodyEnd));
    }

    #[test]
    fn pipelined_group_body_group_error_goes_done() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group_body("no.such", "msg@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("411 No such group"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::UnexpectedResponse(411, _)))
        ));
    }

    #[test]
    fn pipelined_group_body_article_not_found() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group_body("alt.test", "missing@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("211 5 1 5 alt.test"));
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("430 No Such Article"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::ArticleNotFound(_)))
        ));
    }

    #[test]
    fn pipelined_group_body_skips_group_if_already_selected() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group("alt.test");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("211 5 1 5 alt.test"));
        drain_outputs(&mut m);

        m.request_group_body("alt.test", "msg@example");
        let out = drain_outputs(&mut m);
        assert_eq!(
            out[0],
            Output::SendCommand("BODY <msg@example>".to_string())
        );
        assert_eq!(out[1], Output::NeedResponseLine);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn pipelined_group_body_auth_required_on_body() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_group_body("alt.test", "test@example");
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("211 5 1 5 alt.test"));
        drain_outputs(&mut m);

        m.handle_input(Input::ResponseLine("480 Auth required"));
        let out = drain_outputs(&mut m);
        assert!(matches!(
            find_event(&out),
            Some(Event::Error(ProtoError::AuthRequired))
        ));
    }

    // --- Recovery: commands work after error responses ---

    #[test]
    fn stat_after_body_error_works() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("missing@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("430 No Such Article"));
        drain_outputs(&mut m);

        m.request_stat("other@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("223 0 <other@example>"));
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::StatResult { exists: true }));
    }

    #[test]
    fn body_after_auth_error_works() {
        let mut m = NntpMachine::new_after_greeting();
        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("480 Auth required"));
        drain_outputs(&mut m);

        m.request_body("test@example");
        drain_outputs(&mut m);
        m.handle_input(Input::ResponseLine("222 body follows"));
        drain_outputs(&mut m);
        m.handle_input(Input::BodyLine(b"data"));
        drain_outputs(&mut m);
        m.handle_input(Input::BodyEnd);
        let out = drain_outputs(&mut m);
        assert_eq!(find_event(&out), Some(&Event::BodyEnd));
    }
}
