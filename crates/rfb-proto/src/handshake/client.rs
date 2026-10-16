use alloc::{string::String, vec, vec::Vec};

use super::{read_string, try_read};
use crate::{
    PixelFormat, ProtoError, ProtocolVersion,
    consts::{limits, security, vencrypt},
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
    /// `VeNCrypt` subtypes we accept, most preferred first. Only the
    /// certificate-validated ones (`X509*`) can be offered: anonymous TLS
    /// (`TLS*`) cannot be told from a man in the middle, and `Plain` sends the
    /// password unencrypted.
    pub vencrypt: Vec<u32>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            max_version: ProtocolVersion::V3_8,
            shared: true,
            // TLS first: a server that offers it gets a protected session.
            // TLS first, then RSA-AES (which needs a user name and password
            // and a pinned key; the caller removes what it cannot use).
            security: vec![
                security::VENCRYPT,
                security::RA2_256,
                security::RA2,
                security::VNC_AUTH,
                security::NONE,
            ],
            vencrypt: vec![
                vencrypt::X509_NONE,
                vencrypt::X509_VNC,
                vencrypt::X509_PLAIN,
            ],
        }
    }
}

/// What the caller must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// VNC Auth: answer with [`ClientHandshake::vnc_auth_reply`].
    VncAuthChallenge([u8; 16]),
    /// `VeNCrypt` agreed on a TLS subtype: upgrade the transport to TLS now,
    /// then call [`ClientHandshake::tls_established`]. Nothing may be sent or
    /// read in the clear after this point.
    StartTls { subtype: u32 },
    /// An RSA-AES security type was chosen (`security` is 5, 6, 129 or 130):
    /// run its handshake over the transport, then call
    /// [`ClientHandshake::ra2_established`]. The server's public key is the
    /// first thing to arrive; nothing is buffered before it.
    StartRa2 { security: u8 },
    /// `X509Plain`: answer with [`ClientHandshake::plain_credentials`].
    PlainCredentialsNeeded,
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
    VencryptVersion,
    VencryptAck,
    VencryptSubtypes,
    VencryptSelectAck,
    /// Waiting for the caller to finish the TLS handshake.
    AwaitTls,
    /// An RSA-AES type was chosen: report it to the caller.
    Ra2Start,
    /// Waiting for the caller to finish the RSA-AES handshake.
    AwaitRa2,
    /// Waiting for the caller to supply a username and password.
    AwaitCredentials,
    Done,
    Failed,
}

/// Client side of the handshake.
#[derive(Debug)]
pub struct ClientHandshake {
    cfg: ClientConfig,
    state: State,
    version: ProtocolVersion,
    /// The security type chosen, once there is one.
    security: Option<u8>,
    /// The `VeNCrypt` subtype chosen, once there is one.
    subtype: Option<u32>,
    /// The RSA-AES security type chosen.
    ra2: Option<u8>,
    /// `PlainCredentialsNeeded` was reported for the current wait.
    credentials_asked: bool,
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
            security: None,
            subtype: None,
            ra2: None,
            credentials_asked: false,
        }
    }

    /// The version in use. Valid once the server's version was read.
    #[must_use]
    pub fn version(&self) -> ProtocolVersion {
        self.version
    }

    /// The security type in use, once one was chosen.
    #[must_use]
    pub fn security_type(&self) -> Option<u8> {
        self.security
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

    /// The TLS handshake finished; continue with the subtype's own
    /// authentication.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if no TLS upgrade was requested.
    pub fn tls_established(&mut self) -> Result<(), ProtoError> {
        if self.state != State::AwaitTls {
            return Err(ProtoError::Malformed {
                ctx: "tls_established out of order",
            });
        }
        self.state = match self.subtype {
            Some(vencrypt::X509_NONE) => State::AwaitSecurityResult,
            Some(vencrypt::X509_VNC) => State::AwaitChallenge,
            Some(vencrypt::X509_PLAIN) => State::AwaitCredentials,
            _ => return Err(ProtoError::NoAcceptableSecurity),
        };
        Ok(())
    }

    /// The RSA-AES handshake finished (after [`ClientEvent::StartRa2`]): the
    /// server's `SecurityResult` comes next, encrypted for 5 and 129 and in
    /// the clear for 6 and 130, which the caller's transport takes care of.
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if no RSA-AES handshake was requested.
    pub fn ra2_established(&mut self) -> Result<(), ProtoError> {
        if self.state != State::AwaitRa2 {
            return Err(ProtoError::Malformed {
                ctx: "ra2_established out of order",
            });
        }
        self.state = State::AwaitSecurityResult;
        Ok(())
    }

    /// Send the `X509Plain` credentials (over the TLS channel) after
    /// [`ClientEvent::PlainCredentialsNeeded`].
    ///
    /// # Errors
    /// [`ProtoError::Malformed`] if out of order,
    /// [`ProtoError::LimitExceeded`] for an overlong username or password.
    pub fn plain_credentials(
        &mut self,
        username: &[u8],
        password: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), ProtoError> {
        if self.state != State::AwaitCredentials {
            return Err(ProtoError::Malformed {
                ctx: "credentials out of order",
            });
        }
        let length = |field: &'static str, bytes: &[u8]| {
            u32::try_from(bytes.len())
                .ok()
                .filter(|&n| n <= limits::MAX_CREDENTIAL_LEN)
                .ok_or(ProtoError::LimitExceeded {
                    field,
                    value: bytes.len() as u64,
                    limit: u64::from(limits::MAX_CREDENTIAL_LEN),
                })
        };
        let (username_len, password_len) =
            (length("username", username)?, length("password", password)?);
        out.extend_from_slice(&username_len.to_be_bytes());
        out.extend_from_slice(&password_len.to_be_bytes());
        out.extend_from_slice(username);
        out.extend_from_slice(password);
        self.state = State::AwaitSecurityResult;
        Ok(())
    }

    /// The `VeNCrypt` subtype in use, once agreed.
    #[must_use]
    pub fn vencrypt_subtype(&self) -> Option<u32> {
        self.subtype
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
            State::VencryptVersion
            | State::VencryptAck
            | State::VencryptSubtypes
            | State::VencryptSelectAck => self.vencrypt_step(input, out),
            State::AwaitCredentials if !self.credentials_asked => {
                self.credentials_asked = true;
                Ok(Progress::Event(0, ClientEvent::PlainCredentialsNeeded))
            }
            State::Ra2Start => {
                self.state = State::AwaitRa2;
                let security = self.ra2.unwrap_or_default();
                Ok(Progress::Event(0, ClientEvent::StartRa2 { security }))
            }
            State::AwaitReply
            | State::AwaitTls
            | State::AwaitRa2
            | State::AwaitCredentials
            | State::Done => Ok(Progress::Idle),
            State::Failed => Err(ProtoError::Malformed {
                ctx: "handshake already failed",
            }),
        }
    }

    /// State change after a security type was settled. `None` skips the
    /// `SecurityResult` on 3.3 and 3.7, so `ClientInit` goes out right away.
    fn after_choice(&mut self, chosen: u8, out: &mut Vec<u8>) -> Result<(), ProtoError> {
        self.security = Some(chosen);
        match chosen {
            security::NONE if self.version == ProtocolVersion::V3_8 => {
                self.state = State::AwaitSecurityResult;
            }
            security::NONE => self.send_client_init(out),
            security::VNC_AUTH => self.state = State::AwaitChallenge,
            security::VENCRYPT if self.version != ProtocolVersion::V3_3 => {
                self.state = State::VencryptVersion;
            }
            security::RA2 | security::RA2NE | security::RA2_256 | security::RA2NE_256
                if self.version != ProtocolVersion::V3_3 =>
            {
                self.ra2 = Some(chosen);
                self.state = State::Ra2Start;
            }
            _ => return Err(ProtoError::NoAcceptableSecurity),
        }
        Ok(())
    }

    /// `VeNCrypt` 0.2 negotiation (spec, section "`VeNCrypt`").
    // Closures, not method paths: `Reader<'a>` methods are not higher-ranked.
    #[allow(clippy::redundant_closure_for_method_calls)]
    fn vencrypt_step(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<Progress, ProtoError> {
        match self.state {
            State::VencryptVersion => {
                let Some(((major, minor), n)) = try_read(input, |r| Ok((r.u8()?, r.u8()?)))? else {
                    return Ok(Progress::Idle);
                };
                // We speak 0.2 only: older servers cannot be served, newer
                // ones are told 0.2.
                if (major, minor) < (0, 2) {
                    return Err(ProtoError::SecurityFailed(Some(
                        "server only speaks VeNCrypt older than 0.2".into(),
                    )));
                }
                out.extend_from_slice(&[0, 2]);
                self.state = State::VencryptAck;
                Ok(Progress::Consumed(n))
            }
            State::VencryptAck => {
                let Some((ack, n)) = try_read(input, |r| r.u8())? else {
                    return Ok(Progress::Idle);
                };
                if ack != 0 {
                    return Err(ProtoError::SecurityFailed(Some(
                        "server did not accept our VeNCrypt version".into(),
                    )));
                }
                self.state = State::VencryptSubtypes;
                Ok(Progress::Consumed(n))
            }
            State::VencryptSubtypes => {
                let Some((offered, n)) = try_read(input, |r| {
                    let count = usize::from(r.u8()?);
                    let mut list = Vec::with_capacity(count);
                    for _ in 0..count {
                        list.push(r.u32()?);
                    }
                    Ok(list)
                })?
                else {
                    return Ok(Progress::Idle);
                };
                let chosen = self
                    .cfg
                    .vencrypt
                    .iter()
                    .copied()
                    .find(|t| {
                        offered.contains(t)
                            && matches!(
                                *t,
                                vencrypt::X509_NONE | vencrypt::X509_VNC | vencrypt::X509_PLAIN
                            )
                    })
                    .ok_or(ProtoError::NoAcceptableSecurity)?;
                out.extend_from_slice(&chosen.to_be_bytes());
                self.subtype = Some(chosen);
                self.state = State::VencryptSelectAck;
                Ok(Progress::Consumed(n))
            }
            State::VencryptSelectAck => {
                let Some((ack, n)) = try_read(input, |r| r.u8())? else {
                    return Ok(Progress::Idle);
                };
                // Note the polarity: for the subtype the server answers 1 for
                // success, unlike the version ack where 0 is success.
                if ack != 1 {
                    return Err(ProtoError::SecurityFailed(Some(
                        "server refused the VeNCrypt subtype".into(),
                    )));
                }
                self.state = State::AwaitTls;
                let subtype = self.subtype.ok_or(ProtoError::NoAcceptableSecurity)?;
                Ok(Progress::Event(n, ClientEvent::StartTls { subtype }))
            }
            _ => Ok(Progress::Idle),
        }
    }

    fn send_client_init(&mut self, out: &mut Vec<u8>) {
        out.push(u8::from(self.cfg.shared));
        self.state = State::AwaitServerInit;
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
