//! Liveness probes: one GET per service that declares a health URL.

use std::time::Duration;

use crate::catalog::Health;

/// Client for the health probes only. It does not follow any redirect. It does not verify
/// certificates. A probe asks "does something answer?" It sends nothing secret and reads no body.
/// An in-cluster service often has a private CA.
pub fn build_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .tls_certs_only(super::embedded_roots())
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(timeout)
        .user_agent(concat!("portal-probe/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("cannot build the probe client: {e}"))
}

/// Client for the reads that carry an identity token or give a document that decides grants: the
/// actions documents and the options. It verifies the certificate and the host name of an https
/// URL against the embedded roots. It does not follow any redirect.
pub fn build_verified_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .tls_certs_only(super::embedded_roots())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(timeout)
        .user_agent(concat!("portal/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("cannot build the client of the service documents: {e}"))
}

/// Up when the service answers with any status code below 500. A 401 or a redirect means that the
/// service runs. Down on a 5xx response, a refused connection, or a timed-out connection.
pub async fn http(client: &reqwest::Client, url: &str) -> Health {
    match client.get(url).send().await {
        Ok(response) if response.status().as_u16() < 500 => Health::Up,
        _ => Health::Down,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Serves one canned response per connection on a loopback port.
    fn serve(status_line: &'static str) -> String {
        let listener = crate::util::test_ports::listener();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 512];
                let _ = stream.read(&mut buffer);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn statuses_below_500_are_up_and_5xx_or_no_answer_is_down() {
        let client = build_client(Duration::from_secs(2)).unwrap();
        let runtime = runtime();
        for (status, expected) in [
            ("200 OK", Health::Up),
            ("401 Unauthorized", Health::Up),
            ("302 Found", Health::Up),
            ("503 Service Unavailable", Health::Down),
        ] {
            let url = serve(status);
            assert_eq!(runtime.block_on(http(&client, &url)), expected, "{status}");
        }
        // A port that no process listens on.
        let closed = { format!("http://127.0.0.1:{}/", crate::util::test_ports::free()) };
        assert_eq!(runtime.block_on(http(&client, &closed)), Health::Down);
    }
}
