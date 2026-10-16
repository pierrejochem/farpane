use alloc::{vec, vec::Vec};

use super::{try_read, write_string};
use crate::{
    PixelFormat, ProtoError, ProtocolVersion,
    consts::{limits, security, vencrypt},
};

/// Server handshake settings.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Highest version to offer.
    pub max_version: ProtocolVersion,
    /// Security types to offer, most preferred first. None, VNC Auth and
    /// `VeNCrypt` are handled here; 3.3 can use only the first entry that is
    /// None or VNC Auth.
    pub security: Vec<u8>,
    /// `VeNCrypt` subtypes to offer: any of `X509_NONE`, `X509_VNC` and
    /// `X509_PLAIN`.
    pub vencrypt: Vec<u32>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_version: ProtocolVersion::V3_8,
            security: vec![security::VNC_AUTH],
            vencrypt: vec![vencrypt::X509_VNC],
        }
    }
}

/// What the caller must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// VNC Auth selected: send a random challenge with
    /// [`ServerHandshake::send_challenge`].
    VncAuthSelected,
    /// The client's DES response. Verify it, then call
    /// [`ServerHandshake::security_result`].
    VncAuthResponse([u8; 16]),
    /// `VeNCrypt`: the client picked a TLS subtype. Run the TLS handshake as
    /// the server, then call [`ServerHandshake::tls_established`].
    StartTls { subtype: u32 },
    /// `X509Plain` credentials from the client, received over TLS. Verify
    /// them, then call [`ServerHandshake::security_result`].
    PlainCredentials {
        username: Vec<u8>,
        password: Vec<u8>,
    },
    /// Security passed and the client sent `ClientInit`. Answer with
    /// [`ServerHandshake::server_init`].
    ClientInit { shared: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitVersion,
    AwaitChoice,
    AwaitChallengeSend,
    AwaitResponse,
    AwaitResultSend,
    VencryptVersion,
    VencryptSubtype,
    AwaitTls,
    /// TLS is up for `X509Vnc`: announce the VNC Auth step.
    TlsDoneVnc,
    AwaitCredentials,
    AwaitClientInit,
    AwaitServerInitSend,
    Done,
    /// We sent a failed `SecurityResult`; the caller closes the connection.
    Rejected,
    Failed,
}

/// Server side of the handshake.
#[derive(Debug)]
pub struct ServerHandshake {
    cfg: ServerConfig,
    state: State,
    version: ProtocolVersion,
    subtype: Option<u32>,
}

enum Progress {
    Idle,
    Consumed(usize),
    Event(usize, ServerEvent),
}

impl ServerHandshake {
    /// Start a handshake; the server speaks first, so this writes the
    /// `ProtocolVersion` message to `out`.
    #[must_use]
    pub fn new(cfg: ServerConfig, out: &mut Vec<u8>) -> Self {
        out.extend_from_slice(&cfg.max_version.to_bytes());
        let version = cfg.max_version;
        Self {
            cfg,
            state: State::AwaitVersion,
            version,
            subtype: None,
        }
    }

    /// The version in use. Valid once the client's version was read.
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
    ) -> Result<(usize, Option<ServerEvent>), ProtoError> {
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

    /// Send the VNC Auth challenge after [`ServerEvent::VncAuthSelected`].
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if called out of order.
    pub fn send_challenge(
        &mut self,
        challenge: &[u8; 16],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.expect(State::AwaitChallengeSend, "challenge out of order")?;
        out.extend_from_slice(challenge);
        self.state = State::AwaitResponse;
        Ok(())
    }

    /// Send the `SecurityResult` after [`ServerEvent::VncAuthResponse`]. On
    /// failure the handshake is over and the caller closes the connection.
    /// Only 3.8 carries `reason` on the wire.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if called out of order,
    /// [`ProtoError::LimitExceeded`] if `reason` is too long.
    pub fn security_result(
        &mut self,
        ok: bool,
        reason: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.expect(State::AwaitResultSend, "SecurityResult out of order")?;
        if ok {
            out.extend_from_slice(&0u32.to_be_bytes());
            self.state = State::AwaitClientInit;
        } else {
            out.extend_from_slice(&1u32.to_be_bytes());
            if self.version == ProtocolVersion::V3_8 {
                write_string(out, reason, "security reason")?;
            }
            self.state = State::Rejected;
        }
        Ok(())
    }

    /// Send `ServerInit` after [`ServerEvent::ClientInit`]; ends the handshake.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if called out of order,
    /// [`ProtoError::LimitExceeded`] for a too-large framebuffer or name.
    pub fn server_init(
        &mut self,
        width: u16,
        height: u16,
        pixel_format: &PixelFormat,
        name: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        self.expect(State::AwaitServerInitSend, "ServerInit out of order")?;
        if width > limits::MAX_FB_DIMENSION || height > limits::MAX_FB_DIMENSION {
            return Err(ProtoError::LimitExceeded {
                field: "framebuffer dimension",
                value: u64::from(width.max(height)),
                limit: u64::from(limits::MAX_FB_DIMENSION),
            });
        }
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&pixel_format.to_bytes());
        write_string(out, name, "desktop name")?;
        self.state = State::Done;
        Ok(())
    }

    /// The TLS handshake finished: continue with the subtype's authentication.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if no TLS upgrade was requested.
    pub fn tls_established(&mut self, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        self.expect(State::AwaitTls, "tls_established out of order")?;
        match self.subtype {
            Some(vencrypt::X509_NONE) => {
                // After VeNCrypt a SecurityResult always follows, on every version.
                out.extend_from_slice(&0u32.to_be_bytes());
                self.state = State::AwaitClientInit;
            }
            Some(vencrypt::X509_VNC) => self.state = State::TlsDoneVnc,
            Some(vencrypt::X509_PLAIN) => self.state = State::AwaitCredentials,
            _ => return Err(ProtoError::NoAcceptableSecurity),
        }
        Ok(())
    }

    fn expect(&self, want: State, ctx: &'static str) -> Result<(), ProtoError> {
        if self.state == want {
            Ok(())
        } else {
            Err(ProtoError::Malformed { ctx })
        }
    }

    // Closures, not method paths: `Reader<'a>` methods are not higher-ranked.
    #[allow(clippy::redundant_closure_for_method_calls, clippy::too_many_lines)]
    fn step(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<Progress, ProtoError> {
        match self.state {
            State::AwaitVersion => {
                let Some(((major, minor), n)) = try_read(input, |r| {
                    ProtocolVersion::parse_raw(r.take(ProtocolVersion::LEN)?)
                })?
                else {
                    return Ok(Progress::Idle);
                };
                let announced = ProtocolVersion::from_announced(major, minor)?;
                self.version = announced.min(self.cfg.max_version);
                if self.version == ProtocolVersion::V3_3 {
                    // 3.3: the server decides, as a U32.
                    let t = *self
                        .cfg
                        .security
                        .iter()
                        .find(|t| matches!(**t, security::NONE | security::VNC_AUTH))
                        .ok_or(ProtoError::NoAcceptableSecurity)?;
                    out.extend_from_slice(&u32::from(t).to_be_bytes());
                    if let Some(ev) = self.chosen(t, out)? {
                        return Ok(Progress::Event(n, ev));
                    }
                } else {
                    let count = u8::try_from(self.cfg.security.len())
                        .ok()
                        .filter(|&c| c > 0)
                        .ok_or(ProtoError::NoAcceptableSecurity)?;
                    out.push(count);
                    out.extend_from_slice(&self.cfg.security);
                    self.state = State::AwaitChoice;
                }
                Ok(Progress::Consumed(n))
            }
            State::AwaitChoice => {
                let Some((t, n)) = try_read(input, |r| r.u8())? else {
                    return Ok(Progress::Idle);
                };
                if !self.cfg.security.contains(&t) {
                    return Err(ProtoError::NoAcceptableSecurity);
                }
                match self.chosen(t, out)? {
                    Some(ev) => Ok(Progress::Event(n, ev)),
                    None => Ok(Progress::Consumed(n)),
                }
            }
            State::AwaitResponse => {
                let Some((resp, n)) = try_read(input, |r| r.array::<16>())? else {
                    return Ok(Progress::Idle);
                };
                self.state = State::AwaitResultSend;
                Ok(Progress::Event(n, ServerEvent::VncAuthResponse(resp)))
            }
            State::AwaitClientInit => {
                let Some((shared, n)) = try_read(input, |r| Ok(r.u8()? != 0))? else {
                    return Ok(Progress::Idle);
                };
                self.state = State::AwaitServerInitSend;
                Ok(Progress::Event(n, ServerEvent::ClientInit { shared }))
            }
            State::VencryptVersion => {
                let Some(((major, minor), n)) = try_read(input, |r| Ok((r.u8()?, r.u8()?)))? else {
                    return Ok(Progress::Idle);
                };
                if (major, minor) != (0, 2) {
                    out.push(1); // we only speak VeNCrypt 0.2
                    return Err(ProtoError::SecurityFailed(Some(
                        "client does not speak VeNCrypt 0.2".into(),
                    )));
                }
                out.push(0);
                let offered: Vec<u32> = self
                    .cfg
                    .vencrypt
                    .iter()
                    .copied()
                    .filter(|t| {
                        matches!(
                            *t,
                            vencrypt::X509_NONE | vencrypt::X509_VNC | vencrypt::X509_PLAIN
                        )
                    })
                    .collect();
                let count = u8::try_from(offered.len())
                    .ok()
                    .filter(|&c| c > 0)
                    .ok_or(ProtoError::NoAcceptableSecurity)?;
                out.push(count);
                for t in offered {
                    out.extend_from_slice(&t.to_be_bytes());
                }
                self.state = State::VencryptSubtype;
                Ok(Progress::Consumed(n))
            }
            State::VencryptSubtype => {
                let Some((t, n)) = try_read(input, |r| r.u32())? else {
                    return Ok(Progress::Idle);
                };
                let allowed = self.cfg.vencrypt.contains(&t)
                    && matches!(
                        t,
                        vencrypt::X509_NONE | vencrypt::X509_VNC | vencrypt::X509_PLAIN
                    );
                if !allowed {
                    out.push(0);
                    return Err(ProtoError::NoAcceptableSecurity);
                }
                out.push(1);
                self.subtype = Some(t);
                self.state = State::AwaitTls;
                Ok(Progress::Event(n, ServerEvent::StartTls { subtype: t }))
            }
            State::TlsDoneVnc => {
                self.state = State::AwaitChallengeSend;
                Ok(Progress::Event(0, ServerEvent::VncAuthSelected))
            }
            State::AwaitCredentials => {
                let Some(((username, password), n)) = try_read(input, |r| {
                    let (ulen, plen) = (r.u32()?, r.u32()?);
                    for (field, len) in [("username", ulen), ("password", plen)] {
                        if len > limits::MAX_CREDENTIAL_LEN {
                            return Err(ProtoError::LimitExceeded {
                                field,
                                value: u64::from(len),
                                limit: u64::from(limits::MAX_CREDENTIAL_LEN),
                            });
                        }
                    }
                    let username = r.take(ulen as usize)?.to_vec();
                    let password = r.take(plen as usize)?.to_vec();
                    Ok((username, password))
                })?
                else {
                    return Ok(Progress::Idle);
                };
                self.state = State::AwaitResultSend;
                Ok(Progress::Event(
                    n,
                    ServerEvent::PlainCredentials { username, password },
                ))
            }
            State::AwaitTls
            | State::AwaitChallengeSend
            | State::AwaitResultSend
            | State::AwaitServerInitSend
            | State::Rejected
            | State::Done => Ok(Progress::Idle),
            State::Failed => Err(ProtoError::Malformed {
                ctx: "handshake already failed",
            }),
        }
    }

    /// State change once the security type is settled. `None` has no
    /// challenge: 3.8 sends an OK `SecurityResult`, older versions go straight
    /// to `ClientInit`.
    fn chosen(&mut self, t: u8, out: &mut Vec<u8>) -> Result<Option<ServerEvent>, ProtoError> {
        match t {
            security::NONE => {
                if self.version == ProtocolVersion::V3_8 {
                    out.extend_from_slice(&0u32.to_be_bytes());
                }
                self.state = State::AwaitClientInit;
                Ok(None)
            }
            security::VNC_AUTH => {
                self.state = State::AwaitChallengeSend;
                Ok(Some(ServerEvent::VncAuthSelected))
            }
            security::VENCRYPT if self.version != ProtocolVersion::V3_3 => {
                out.extend_from_slice(&[0, 2]); // the highest version we support: 0.2
                self.state = State::VencryptVersion;
                Ok(None)
            }
            _ => Err(ProtoError::NoAcceptableSecurity),
        }
    }
}
