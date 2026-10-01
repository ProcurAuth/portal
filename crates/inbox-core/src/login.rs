//! OPAQUE password login, used by device enrollment.

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, Identifiers, RegistrationRequest, RegistrationUpload, ServerLogin,
    ServerLoginParameters, ServerRegistration, ServerSetup,
};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, Result};
use crate::rng::Rng;
use crate::suite::{Role, Suite};

/// The 64-byte key both sides share after a successful OPAQUE login.
/// Only ever used through [`SessionKey::derive`].
pub struct SessionKey(Zeroizing<Vec<u8>>);

impl SessionKey {
    /// Derives an independent 32-byte key for `label` (HKDF-SHA256).
    pub fn derive(&self, label: &str) -> Zeroizing<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(Some(b"inbox/v1"), &self.0);
        let mut out = Zeroizing::new([0u8; 32]);
        hk.expand(label.as_bytes(), out.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }
}

/// HMAC-SHA256 of the length-prefixed `parts` under `key`.
pub fn mac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    for part in parts {
        m.update(&(part.len() as u64).to_be_bytes());
        m.update(part);
    }
    m.finalize().into_bytes().into()
}

/// Constant-time verification of [`mac`].
pub fn verify_mac(key: &[u8; 32], parts: &[&[u8]], tag: &[u8]) -> Result<()> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    for part in parts {
        m.update(&(part.len() as u64).to_be_bytes());
        m.update(part);
    }
    m.verify_slice(tag).map_err(|_| Error::AuthenticationFailed)
}

/// Builds a credential record offline: both OPAQUE registration sides run locally,
/// so the password never reaches the running server.
pub fn register(
    setup: &ServerSetup<Suite>,
    role: Role,
    password: &[u8],
    rng: &mut Rng,
) -> Result<Vec<u8>> {
    let start = ClientRegistration::<Suite>::start(rng, password)
        .map_err(|_| Error::Internal("registration start"))?;
    let request = RegistrationRequest::<Suite>::deserialize(&start.message.serialize())
        .map_err(|_| Error::Internal("registration request"))?;
    let server = ServerRegistration::<Suite>::start(setup, request, role.credential_identifier())
        .map_err(|_| Error::Internal("server registration start"))?;
    let finish = start
        .state
        .finish(
            rng,
            password,
            server.message,
            ClientRegistrationFinishParameters::new(Identifiers::default(), None),
        )
        .map_err(|_| Error::Internal("registration finish"))?;
    let upload = RegistrationUpload::<Suite>::deserialize(&finish.message.serialize())
        .map_err(|_| Error::Internal("registration upload"))?;
    Ok(ServerRegistration::finish(upload).serialize().to_vec())
}

/// Validates a credential record read from disk.
pub fn parse_record(bytes: &[u8]) -> Result<ServerRegistration<Suite>> {
    ServerRegistration::<Suite>::deserialize(bytes)
        .map_err(|_| Error::Malformed("credential record"))
}

/// Client side of a login.
pub struct LoginClient {
    role: Role,
    password: Zeroizing<Vec<u8>>,
    state: ClientLogin<Suite>,
}

impl LoginClient {
    /// Returns the client and the serialized `CredentialRequest` to send.
    pub fn start(role: Role, password: &[u8], rng: &mut Rng) -> Result<(LoginClient, Vec<u8>)> {
        let start = ClientLogin::<Suite>::start(rng, password)
            .map_err(|_| Error::Internal("login start"))?;
        let request = start.message.serialize().to_vec();
        let client =
            LoginClient { role, password: Zeroizing::new(password.to_vec()), state: start.state };
        Ok((client, request))
    }

    /// Consumes the server `CredentialResponse`. Returns the serialized
    /// `CredentialFinalization` and the session key. A wrong password, or a server
    /// that does not hold the credential record, fails here.
    pub fn finish(self, response: &[u8], rng: &mut Rng) -> Result<(Vec<u8>, SessionKey)> {
        let response = CredentialResponse::<Suite>::deserialize(response)
            .map_err(|_| Error::Malformed("credential response"))?;
        let params = ClientLoginFinishParameters::new(
            Some(self.role.context()),
            Identifiers::default(),
            None,
        );
        let finish = self
            .state
            .finish(rng, &self.password, response, params)
            .map_err(|_| Error::AuthenticationFailed)?;
        let mut key = finish.session_key.to_vec();
        let session = SessionKey(Zeroizing::new(key.clone()));
        key.zeroize();
        Ok((finish.message.serialize().to_vec(), session))
    }
}

/// Server side of a login.
pub struct LoginServer {
    role: Role,
    state: ServerLogin<Suite>,
}

impl LoginServer {
    /// Returns the server and the serialized `CredentialResponse` to send.
    pub fn start(
        setup: &ServerSetup<Suite>,
        record: &ServerRegistration<Suite>,
        role: Role,
        request: &[u8],
        rng: &mut Rng,
    ) -> Result<(LoginServer, Vec<u8>)> {
        let request = CredentialRequest::<Suite>::deserialize(request)
            .map_err(|_| Error::Malformed("credential request"))?;
        let params = ServerLoginParameters {
            context: Some(role.context()),
            identifiers: Identifiers::default(),
        };
        let start = ServerLogin::start(
            rng,
            setup,
            Some(record.clone()),
            request,
            role.credential_identifier(),
            params,
        )
        .map_err(|_| Error::Internal("server login start"))?;
        let response = start.message.serialize().to_vec();
        Ok((LoginServer { role, state: start.state }, response))
    }

    /// Consumes the client `CredentialFinalization`. Fails on a wrong password.
    pub fn finish(self, finalization: &[u8]) -> Result<SessionKey> {
        let finalization = CredentialFinalization::<Suite>::deserialize(finalization)
            .map_err(|_| Error::Malformed("credential finalization"))?;
        let params = ServerLoginParameters {
            context: Some(self.role.context()),
            identifiers: Identifiers::default(),
        };
        let finish =
            self.state.finish(finalization, params).map_err(|_| Error::AuthenticationFailed)?;
        Ok(SessionKey(Zeroizing::new(finish.session_key.to_vec())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> ServerSetup<Suite> {
        ServerSetup::<Suite>::new(&mut Rng::os())
    }

    fn login(
        setup: &ServerSetup<Suite>,
        record: &[u8],
        role: Role,
        password: &[u8],
    ) -> Result<(SessionKey, SessionKey)> {
        let mut rng = Rng::os();
        let record = parse_record(record)?;
        let (client, request) = LoginClient::start(role, password, &mut rng)?;
        let (server, response) = LoginServer::start(setup, &record, role, &request, &mut rng)?;
        let (finalization, client_key) = client.finish(&response, &mut rng)?;
        let server_key = server.finish(&finalization)?;
        Ok((client_key, server_key))
    }

    #[test]
    fn correct_password_agrees_on_keys() {
        let setup = setup();
        let record = register(&setup, Role::Device, b"correct horse", &mut Rng::os()).unwrap();
        let (c, s) = login(&setup, &record, Role::Device, b"correct horse").unwrap();
        assert_eq!(*c.derive("x"), *s.derive("x"));
        assert_ne!(*c.derive("x"), *c.derive("y"));
    }

    #[test]
    fn wrong_password_fails() {
        let setup = setup();
        let record = register(&setup, Role::Device, b"correct horse", &mut Rng::os()).unwrap();
        assert!(login(&setup, &record, Role::Device, b"battery staple").is_err());
    }

    #[test]
    fn mac_detects_tampering() {
        let key = [7u8; 32];
        let tag = mac(&key, &[b"a", b"bc"]);
        assert!(verify_mac(&key, &[b"a", b"bc"], &tag).is_ok());
        assert!(verify_mac(&key, &[b"ab", b"c"], &tag).is_err());
    }
}
