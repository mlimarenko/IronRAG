use std::{fmt, sync::Arc};

use anyhow::Context as _;
use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tokio_rustls::TlsConnector;
use url::Url;

#[derive(Clone)]
pub(super) struct TlsPinProxies {
    _shutdown: Arc<watch::Sender<()>>,
}

#[derive(Clone, Copy)]
pub(super) enum UpstreamProtocol {
    Postgres,
    Redis,
}

#[derive(Debug)]
struct ExactLeafVerifier {
    expected_sha256: [u8; 32],
    chain_and_hostname: Arc<WebPkiServerVerifier>,
}

impl ServerCertVerifier for ExactLeafVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        verify_exact_leaf(end_entity.as_ref(), self.expected_sha256)?;
        self.chain_and_hostname.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.chain_and_hostname.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.chain_and_hostname.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.chain_and_hostname.supported_verify_schemes()
    }
}

fn verify_exact_leaf(peer_der: &[u8], expected_sha256: [u8; 32]) -> Result<(), rustls::Error> {
    let actual: [u8; 32] = Sha256::digest(peer_der).into();
    if actual == expected_sha256 {
        Ok(())
    } else {
        Err(rustls::Error::General(
            "dependency TLS peer leaf does not match the configured DER pin".into(),
        ))
    }
}

impl fmt::Debug for TlsPinProxies {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TlsPinProxies").finish_non_exhaustive()
    }
}

pub(super) async fn start_tls_pin_proxy(
    upstream_url: &str,
    protocol: UpstreamProtocol,
    pinned_der: Vec<u8>,
) -> anyhow::Result<(Url, TlsPinProxies)> {
    let mut local_url = Url::parse(upstream_url).context("invalid pinned dependency URL")?;
    let upstream_host =
        local_url.host_str().context("pinned dependency URL has no hostname")?.to_owned();
    let upstream_port =
        local_url.port_or_known_default().context("pinned dependency URL has no port")?;
    let server_name = ServerName::try_from(upstream_host.clone())
        .map_err(|error| anyhow::anyhow!("invalid pinned dependency TLS hostname: {error}"))?;

    let certificate = CertificateDer::from(pinned_der.clone());
    let mut roots = RootCertStore::empty();
    roots.add(certificate).context("configured dependency leaf is not a valid trust anchor")?;
    let chain_and_hostname = WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .context("cannot construct dependency TLS verifier")?;
    let verifier = ExactLeafVerifier {
        expected_sha256: Sha256::digest(&pinned_der).into(),
        chain_and_hostname,
    };
    let mut client_config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    client_config.enable_sni = true;
    let connector = TlsConnector::from(Arc::new(client_config));

    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let local_port = listener.local_addr()?.port();
    local_url.set_host(Some("127.0.0.1"))?;
    local_url.set_port(Some(local_port)).map_err(|()| anyhow::anyhow!("cannot set proxy port"))?;
    if matches!(protocol, UpstreamProtocol::Redis) {
        local_url.set_scheme("redis").map_err(|()| anyhow::anyhow!("cannot set Redis scheme"))?;
    }

    let (shutdown, mut shutdown_rx) = watch::channel(());
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                result = shutdown_rx.changed() => {
                    if result.is_err() {
                        break;
                    }
                    continue;
                }
            };
            let Ok((downstream, _)) = accepted else { break };
            let connector = connector.clone();
            let upstream_host = upstream_host.clone();
            let server_name = server_name.clone();
            tokio::spawn(async move {
                if let Err(error) = proxy_connection(
                    downstream,
                    &upstream_host,
                    upstream_port,
                    server_name,
                    connector,
                    protocol,
                )
                .await
                {
                    tracing::warn!(%error, "pinned dependency TLS connection failed");
                }
            });
        }
    });

    Ok((local_url, TlsPinProxies { _shutdown: Arc::new(shutdown) }))
}

async fn proxy_connection(
    mut downstream: TcpStream,
    upstream_host: &str,
    upstream_port: u16,
    server_name: ServerName<'static>,
    connector: TlsConnector,
    protocol: UpstreamProtocol,
) -> anyhow::Result<()> {
    let mut upstream = TcpStream::connect((upstream_host, upstream_port)).await?;
    if matches!(protocol, UpstreamProtocol::Postgres) {
        upstream.write_all(&[0, 0, 0, 8, 4, 210, 22, 47]).await?;
        let response = upstream.read_u8().await?;
        anyhow::ensure!(response == b'S', "PostgreSQL dependency refused TLS");
    }
    let mut secured = connector.connect(server_name, upstream).await?;
    tokio::io::copy_bidirectional(&mut downstream, &mut secured).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_leaf_verifier_accepts_only_the_pinned_der() {
        let pinned = b"pinned leaf DER";
        let expected = Sha256::digest(pinned).into();

        assert!(verify_exact_leaf(pinned, expected).is_ok());
        assert!(verify_exact_leaf(b"different leaf DER", expected).is_err());
    }
}
