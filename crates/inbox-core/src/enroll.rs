//! Device enrollment: OPAQUE login with the shared device password, then an
//! exchange of static keys authenticated by the OPAQUE session key.
//!
//! ```text
//! device                                   server
//!   Start{credential_request}          ->
//!                                      <-  Challenge{credential_response}
//!   Finish{finalization, name, pk, mac} ->      (verifies OPAQUE then mac)
//!                                      <-  Accepted{device_id, server_pk, mac}
//! ```
//!
//! A passive observer learns nothing usable. An active attacker without the
//! credential record cannot complete OPAQUE, so it can neither substitute its own
//! server key nor register a device.

use zeroize::Zeroizing;

use crate::PROTOCOL_VERSION;
use crate::error::{Error, Result};
use crate::login::{self, LoginClient, LoginServer, SessionKey};
use crate::noise;
use crate::proto::{EnrollClientMsg, EnrollServerMsg};
use crate::rng::Rng;
use crate::server_key::ServerKey;
use crate::suite::Role;

const CLIENT_MAC: &str = "enroll/client-mac";
const SERVER_MAC: &str = "enroll/server-mac";

/// Upper bound on the device name, in bytes.
pub const MAX_DEVICE_NAME: usize = 64;

/// What the device keeps after a successful enrollment.
pub struct EnrolledDevice {
    pub device_id: String,
    pub server_name: String,
    pub device_private_key: Zeroizing<Vec<u8>>,
    pub device_public_key: Vec<u8>,
    pub server_public_key: Vec<u8>,
}

/// Next action for the device side.
pub enum ClientStep {
    Send(String),
    Done(EnrolledDevice),
}

enum ClientState {
    AwaitChallenge(Box<LoginClient>),
    AwaitAccepted { key: SessionKey, device: noise::Keypair },
    Finished,
}

/// Device side of enrollment.
pub struct EnrollClient {
    device_name: String,
    state: ClientState,
    rng: Rng,
}

pub fn validate_device_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_DEVICE_NAME || name.chars().any(char::is_control) {
        return Err(Error::Malformed("device name"));
    }
    Ok(name.to_string())
}

fn encode<T: serde::Serialize>(msg: &T) -> String {
    serde_json::to_string(msg).expect("protocol messages always serialize")
}

impl EnrollClient {
    /// Returns the client and the first message to send.
    pub fn start(
        password: &[u8],
        device_name: &str,
        mut rng: Rng,
    ) -> Result<(EnrollClient, String)> {
        let device_name = validate_device_name(device_name)?;
        let (login, request) = LoginClient::start(Role::Device, password, &mut rng)?;
        let msg = encode(&EnrollClientMsg::Start {
            protocol: PROTOCOL_VERSION,
            credential_request: request,
        });
        Ok((
            EnrollClient { device_name, state: ClientState::AwaitChallenge(Box::new(login)), rng },
            msg,
        ))
    }

    /// Feeds one server message.
    pub fn on_message(&mut self, text: &str) -> Result<ClientStep> {
        let msg: EnrollServerMsg =
            serde_json::from_str(text).map_err(|_| Error::Malformed("enrollment message"))?;
        if let EnrollServerMsg::Rejected { code, message } = msg {
            self.state = ClientState::Finished;
            return Err(Error::Rejected { code, message });
        }
        match (std::mem::replace(&mut self.state, ClientState::Finished), msg) {
            (
                ClientState::AwaitChallenge(login),
                EnrollServerMsg::Challenge { credential_response },
            ) => {
                let (finalization, key) = login.finish(&credential_response, &mut self.rng)?;
                let device = noise::generate_keypair()?;
                let mac = login::mac(
                    &key.derive(CLIENT_MAC),
                    &[self.device_name.as_bytes(), &device.public],
                );
                let reply = encode(&EnrollClientMsg::Finish {
                    credential_finalization: finalization,
                    device_name: self.device_name.clone(),
                    device_public_key: device.public.clone(),
                    mac: mac.to_vec(),
                });
                self.state = ClientState::AwaitAccepted { key, device };
                Ok(ClientStep::Send(reply))
            }
            (
                ClientState::AwaitAccepted { key, device },
                EnrollServerMsg::Accepted { device_id, server_name, server_public_key, mac },
            ) => {
                login::verify_mac(
                    &key.derive(SERVER_MAC),
                    &[
                        device_id.as_bytes(),
                        server_name.as_bytes(),
                        &server_public_key,
                        &device.public,
                    ],
                    &mac,
                )?;
                if server_public_key.len() != 32 {
                    return Err(Error::Malformed("server key length"));
                }
                Ok(ClientStep::Done(EnrolledDevice {
                    device_id,
                    server_name,
                    device_private_key: device.private,
                    device_public_key: device.public,
                    server_public_key,
                }))
            }
            _ => Err(Error::UnexpectedMessage("enrollment order")),
        }
    }
}

/// Server side of enrollment. One instance per WebSocket connection.
pub struct EnrollServer<'a> {
    key: &'a ServerKey,
    record: &'a opaque_ke::ServerRegistration<crate::suite::Suite>,
    login: Option<LoginServer>,
    session: Option<SessionKey>,
    device_public_key: Option<Vec<u8>>,
}

/// A device that proved knowledge of the password, waiting for an id.
pub struct VerifiedDevice {
    pub name: String,
    pub public_key: Vec<u8>,
}

impl<'a> EnrollServer<'a> {
    pub fn new(
        key: &'a ServerKey,
        record: &'a opaque_ke::ServerRegistration<crate::suite::Suite>,
    ) -> EnrollServer<'a> {
        EnrollServer { key, record, login: None, session: None, device_public_key: None }
    }

    pub fn parse(text: &str) -> Result<EnrollClientMsg> {
        serde_json::from_str(text).map_err(|_| Error::Malformed("enrollment message"))
    }

    /// Handles `Start`, returns the challenge to send.
    pub fn on_start(&mut self, msg: EnrollClientMsg, rng: &mut Rng) -> Result<String> {
        let EnrollClientMsg::Start { protocol, credential_request } = msg else {
            return Err(Error::UnexpectedMessage("expected start"));
        };
        if self.login.is_some() || self.session.is_some() {
            return Err(Error::UnexpectedMessage("duplicate start"));
        }
        if protocol != PROTOCOL_VERSION {
            return Err(Error::UnsupportedProtocol(protocol));
        }
        let (login, response) = LoginServer::start(
            &self.key.opaque,
            self.record,
            Role::Device,
            &credential_request,
            rng,
        )?;
        self.login = Some(login);
        Ok(encode(&EnrollServerMsg::Challenge { credential_response: response }))
    }

    /// Handles `Finish`. Fails with [`Error::AuthenticationFailed`] on a wrong password.
    pub fn on_finish(&mut self, msg: EnrollClientMsg) -> Result<VerifiedDevice> {
        let EnrollClientMsg::Finish {
            credential_finalization,
            device_name,
            device_public_key,
            mac,
        } = msg
        else {
            return Err(Error::UnexpectedMessage("expected finish"));
        };
        let login = self.login.take().ok_or(Error::UnexpectedMessage("finish before start"))?;
        let key = login.finish(&credential_finalization)?;
        login::verify_mac(
            &key.derive(CLIENT_MAC),
            &[device_name.as_bytes(), &device_public_key],
            &mac,
        )?;
        let name = validate_device_name(&device_name)?;
        if device_public_key.len() != 32 {
            return Err(Error::Malformed("device key length"));
        }
        self.session = Some(key);
        self.device_public_key = Some(device_public_key.clone());
        Ok(VerifiedDevice { name, public_key: device_public_key })
    }

    /// Builds the `Accepted` reply once the device is stored.
    pub fn accepted(&self, device_id: &str, server_name: &str) -> Result<String> {
        let key = self.session.as_ref().ok_or(Error::UnexpectedMessage("accept before finish"))?;
        let device_pk = self.device_public_key.as_ref().ok_or(Error::Internal("device key"))?;
        let mac = login::mac(
            &key.derive(SERVER_MAC),
            &[device_id.as_bytes(), server_name.as_bytes(), &self.key.noise_public, device_pk],
        );
        Ok(encode(&EnrollServerMsg::Accepted {
            device_id: device_id.to_string(),
            server_name: server_name.to_string(),
            server_public_key: self.key.noise_public.clone(),
            mac: mac.to_vec(),
        }))
    }

    pub fn rejected(code: &str, message: &str) -> String {
        encode(&EnrollServerMsg::Rejected { code: code.to_string(), message: message.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        server_pw: &[u8],
        client_pw: &[u8],
    ) -> Result<(EnrolledDevice, VerifiedDevice, Vec<u8>)> {
        let key = ServerKey::generate(&mut Rng::os()).unwrap();
        let record_bytes =
            login::register(&key.opaque, Role::Device, server_pw, &mut Rng::os()).unwrap();
        let record = login::parse_record(&record_bytes).unwrap();
        let mut server = EnrollServer::new(&key, &record);
        let (mut client, m1) = EnrollClient::start(client_pw, " Pixel de garde ", Rng::os())?;
        let m2 = server.on_start(EnrollServer::parse(&m1)?, &mut Rng::os())?;
        let ClientStep::Send(m3) = client.on_message(&m2)? else { panic!("expected send") };
        let verified = server.on_finish(EnrollServer::parse(&m3)?)?;
        let m4 = server.accepted("dev1", "Prod")?;
        let ClientStep::Done(device) = client.on_message(&m4)? else { panic!("expected done") };
        Ok((device, verified, key.noise_public.clone()))
    }

    #[test]
    fn enrollment_succeeds_and_pins_server_key() {
        let (device, verified, server_pk) = run(b"pw", b"pw").unwrap();
        assert_eq!(device.device_id, "dev1");
        assert_eq!(device.server_public_key, server_pk);
        assert_eq!(verified.public_key, device.device_public_key);
        assert_eq!(verified.name, "Pixel de garde");
    }

    #[test]
    fn wrong_password_is_rejected() {
        assert!(run(b"pw", b"other").is_err());
    }

    #[test]
    fn tampered_accept_is_rejected() {
        let key = ServerKey::generate(&mut Rng::os()).unwrap();
        let record_bytes =
            login::register(&key.opaque, Role::Device, b"pw", &mut Rng::os()).unwrap();
        let record = login::parse_record(&record_bytes).unwrap();
        let mut server = EnrollServer::new(&key, &record);
        let (mut client, m1) = EnrollClient::start(b"pw", "phone", Rng::os()).unwrap();
        let m2 = server.on_start(EnrollServer::parse(&m1).unwrap(), &mut Rng::os()).unwrap();
        let ClientStep::Send(m3) = client.on_message(&m2).unwrap() else { panic!() };
        server.on_finish(EnrollServer::parse(&m3).unwrap()).unwrap();
        let m4 = server.accepted("dev1", "Prod").unwrap();
        let forged = m4.replace("dev1", "dev2");
        assert_eq!(client.on_message(&forged).err(), Some(Error::AuthenticationFailed));
    }
}
