use alloc::{string::String, vec, vec::Vec};

use super::{read_string, try_read};
use crate::{
    PixelFormat, ProtoError, ProtocolVersion,
    consts::{limits, security},
};

/// Client handshake settings.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Highest version to speak.
    pub max_version: ProtocolVersion,
    /// `ClientInit` shared flag: leave other clients connected.
    pub shared: bool,
    /// Allowed security types, most preferred first.
    pub security: Vec<u8>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            max_version: ProtocolVersion::V3_8,
            shared: true,
            security: vec![security::VNC_AUTH, security::NONE],
        }
    }
}

/// What the caller must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// VNC Auth: answer with [`ClientHandshake::vnc_auth_reply`].
    VncAuthChallenge([u8; 16]),
    /// Handshake complete; the connection is now in the message loop.
    ServerInit {
        width: u16,
        height: u16,
        pixel_format: PixelFormat,
        name: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitVersion,
    AwaitSecurityList,
    AwaitSecurityType33,
    AwaitReason,
    AwaitChallenge,
    AwaitReply,
    AwaitSecurityResult,
    AwaitServerInit,
    Done,
    Failed,
}

/// Client side of the handshake.
#[derive(Debug)]
pub struct ClientHandshake {
    cfg: ClientConfig,
    state: State,
    version: ProtocolVersion,
}

enum Progress {
    Idle,
    Consumed(usize),
    Event(usize, ClientEvent),
}

impl ClientHandshake {
    #[must_use]
    pub fn new(cfg: ClientConfig) -> Self {
        let version = cfg.max_version;
        Self {
            cfg,
            state: State::AwaitVersion,
            version,
        }
    }

    /// The version in use. Valid once the server's version was read.
    #[must_use]
    pub fn version(&self) -> ProtocolVersion {
        self.version
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Consume bytes; see the module docs for the calling convention.
    ///
    /// # Errors
    /// Any [`ProtoError`] other than `NeedMore`. All are fatal.
    pub fn poll(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(usize, Option<ClientEvent>), ProtoError> {
        let mut pos = 0;
        loop {
            match self.step(&input[pos..], out) {
                Ok(Progress::Idle) => return Ok((pos, None)),
                Ok(Progress::Consumed(n)) => pos += n,
                Ok(Progress::Event(n, ev)) => return Ok((pos + n, Some(ev))),
                Err(e) => {
                    self.state = State::Failed;
                    return Err(e);
                }
            }
        }
    }

    /// Answer a [`ClientEvent::VncAuthChallenge`] with the DES-encrypted
    /// challenge (computed by `rfb-security`).
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if no challenge is pending.
    pub fn vnc_auth_reply(
        &mut self,
        response: &[u8; 16],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        if self.state != State::AwaitReply {
            return Err(ProtoError::Malformed {
                ctx: "VNC Auth reply out of order",
            });
        }
        out.extend_from_slice(response);
        self.state = State::AwaitSecurityResult;
        Ok(())
    }

    // Closures, not method paths: `Reader<'a>` methods are not higher-ranked.
    #[allow(clippy::redundant_closure_for_method_calls, clippy::too_many_lines)]
    fn step(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<Progress, ProtoError> {
        match self.state {
            State::AwaitVersion => {
                let Some(((major, minor), n)) = try_read(input, |r| {
                    crate::ProtocolVersion::parse_raw(r.take(ProtocolVersion::LEN)?)
                })?
                else {
                    return Ok(Progress::Idle);
                };
                let offer = ProtocolVersion::from_announced(major, minor)?;
                self.version = ProtocolVersion::client_choice(offer, self.cfg.max_version);
                out.extend_from_slice(&self.version.to_bytes());
                self.state = if self.version == ProtocolVersion::V3_3 {
                    State::AwaitSecurityType33
                } else {
                    State::AwaitSecurityList
                };
                Ok(Progress::Consumed(n))
            }
            State::AwaitSecurityList => {
                let Some((types, n)) = try_read(input, |r| {
                    let count = r.u8()?;
                    Ok(if count == 0 {
                        None
                    } else {
                        Some(r.take(usize::from(count))?.to_vec())
                    })
                })?
                else {
                    return Ok(Progress::Idle);
                };
                match types {
                    None => self.state = State::AwaitReason,
                    Some(offered) => {
                        let chosen = self
                            .cfg
                            .security
                            .iter()
                            .copied()
                            .find(|t| offered.contains(t))
                            .ok_or(ProtoError::NoAcceptableSecurity)?;
                        out.push(chosen);
                        self.after_choice(chosen, out)?;
                    }
                }
                Ok(Progress::Consumed(n))
            }
            State::AwaitSecurityType33 => {
                let Some((t, n)) = try_read(input, |r| r.u32())? else {
                    return Ok(Progress::Idle);
                };
                match u8::try_from(t) {
                    Ok(security::INVALID) => self.state = State::AwaitReason,
                    Ok(t @ (security::NONE | security::VNC_AUTH))
                        if self.cfg.security.contains(&t) =>
                    {
                        self.after_choice(t, out)?;
                    }
                    _ => return Err(ProtoError::NoAcceptableSecurity),
                }
                Ok(Progress::Consumed(n))
            }
            State::AwaitReason => {
                let Some((reason, _)) = try_read(input, |r| {
                    read_string(r, "security reason").map(<[u8]>::to_vec)
                })?
                else {
                    return Ok(Progress::Idle);
                };
                Err(ProtoError::SecurityFailed(Some(lossy(&reason))))
            }
            State::AwaitChallenge => {
                let Some((c, n)) = try_read(input, |r| r.array::<16>())? else {
                    return Ok(Progress::Idle);
                };
                self.state = State::AwaitReply;
                Ok(Progress::Event(n, ClientEvent::VncAuthChallenge(c)))
            }
            State::AwaitSecurityResult => {
                let Some((res, n)) = try_read(input, |r| r.u32())? else {
                    return Ok(Progress::Idle);
                };
                if res == 0 {
                    self.send_client_init(out);
                } else if self.version == ProtocolVersion::V3_8 {
                    self.state = State::AwaitReason;
                } else {
                    return Err(ProtoError::SecurityFailed(None));
                }
                Ok(Progress::Consumed(n))
            }
            State::AwaitServerInit => {
                let Some((ev, n)) = try_read(input, |r| {
                    let width = r.u16()?;
                    let height = r.u16()?;
                    if width > limits::MAX_FB_DIMENSION || height > limits::MAX_FB_DIMENSION {
                        return Err(ProtoError::LimitExceeded {
                            field: "framebuffer dimension",
                            value: u64::from(width.max(height)),
                            limit: u64::from(limits::MAX_FB_DIMENSION),
                        });
                    }
                    let pixel_format = PixelFormat::read(r)?;
                    let name = read_string(r, "desktop name")?.to_vec();
                    Ok(ClientEvent::ServerInit {
                        width,
                        height,
                        pixel_format,
                        name,
                    })
                })?
                else {
                    return Ok(Progress::Idle);
                };
                self.state = State::Done;
                Ok(Progress::Event(n, ev))
            }
            State::AwaitReply | State::Done => Ok(Progress::Idle),
            State::Failed => Err(ProtoError::Malformed {
                ctx: "handshake already failed",
            }),
        }
    }

    /// State change after a security type was settled. `None` skips the
    /// `SecurityResult` on 3.3 and 3.7, so `ClientInit` goes out right away.
    fn after_choice(&mut self, chosen: u8, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        match chosen {
            security::NONE if self.version == ProtocolVersion::V3_8 => {
                self.state = State::AwaitSecurityResult;
            }
            security::NONE => self.send_client_init(out),
            security::VNC_AUTH => self.state = State::AwaitChallenge,
            _ => return Err(ProtoError::NoAcceptableSecurity),
        }
        Ok(())
    }

    fn send_client_init(&mut self, out: &mut Vec<u8>) {
        out.push(u8::from(self.cfg.shared));
        self.state = State::AwaitServerInit;
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
