//! MOQT の QUIC 接続確立
//!
//! TLS / ALPN / datagram provider を構築し、`s2n-quic` で接続を確立する。
//! QUIC 実装への依存はこのファイルに閉じ込め、他は `transport.rs` 越しに扱う。

use std::sync::Arc;

use s2n_quic::Client;
use s2n_quic::client::Connect;
use s2n_quic::connection::Connection;
use s2n_quic::provider::tls::rustls as s2n_rustls;
// PEM 形式の証明書を読み込むためのトレイト
use rustls_pki_types::pem::PemObject;
// システムのルート証明書を使う TLS 設定を構築するためのトレイト
use rustls_platform_verifier::BuilderVerifierExt;

use crate::moq::error::MoqError;
use crate::moq::url::ServerUrl;

/// MOQT の ALPN プロトコル識別子
///
/// `draft-ietf-moq-transport-21` §6.2 (Session establishment) が ALPN を `moqt-21` と定める。
/// ALPN は draft 版ごとに変わるため、接続先の実装と同じ版に合わせる必要がある。
const MOQT_ALPN: &[u8] = b"moqt-21";

/// datagram provider の受信バッファ容量
const DATAGRAM_RECV_CAPACITY: usize = 64;

/// QUIC 接続を確立する
///
/// `insecure` が真の場合は証明書検証をスキップし、`ca_cert` (PEM の内容) が指定された
/// 場合はその証明書で検証する。両方指定された場合は `insecure` を優先する。
/// どちらも指定されない場合はシステムのルート証明書で検証する。
pub async fn connect(
    server: &ServerUrl,
    insecure: bool,
    ca_cert: Option<&str>,
) -> Result<Connection, MoqError> {
    let tls = build_tls_client(insecure, ca_cert)?;

    // MOQT の Object Datagram は QUIC DATAGRAM 拡張 (RFC 9221) を使う。
    // s2n-quic では datagram provider を明示的に設定しないと datagram を送受信できない
    let datagram_endpoint = s2n_quic::provider::datagram::default::Endpoint::builder()
        .with_recv_capacity(DATAGRAM_RECV_CAPACITY)
        .map_err(|e| MoqError::Quic(format!("failed to set datagram recv capacity: {e}")))?
        .build()
        .expect("datagram endpoint build must succeed after recv capacity is set");

    let client = Client::builder()
        .with_tls(tls)
        .map_err(|e| MoqError::Tls(format!("failed to configure TLS: {e}")))?
        .with_io("0.0.0.0:0")
        .map_err(|e| MoqError::Quic(format!("failed to bind local address: {e}")))?
        .with_datagram(datagram_endpoint)
        .map_err(|e| MoqError::Quic(format!("failed to configure datagram provider: {e}")))?
        .start()
        .map_err(|e| MoqError::Quic(format!("failed to start QUIC client: {e}")))?;

    // ホスト名は DNS 解決する。IPv4 / IPv6 リテラルも lookup_host で解決できる。
    // 解決結果の順序は環境依存で IPv6 が先に来ることがあり、IPv6 が通らない環境では
    // 接続がタイムアウトするため、IPv4 を優先して選ぶ
    let addresses: Vec<std::net::SocketAddr> =
        tokio::net::lookup_host((server.host(), server.port()))
            .await
            .map_err(|e| MoqError::Quic(format!("failed to resolve '{}': {e}", server.host())))?
            .collect();
    let socket_addr = addresses
        .iter()
        .find(|addr| addr.is_ipv4())
        .or_else(|| addresses.first())
        .copied()
        .ok_or_else(|| MoqError::Quic(format!("no address found for '{}'", server.host())))?;

    let connect = Connect::new(socket_addr).with_server_name(server.host());
    let connection = client
        .connect(connect)
        .await
        .map_err(|e| MoqError::Quic(format!("failed to connect to {socket_addr}: {e}")))?;

    tracing::info!(target: "moq", addr = %socket_addr, "connected to MOQT server");
    Ok(connection)
}

/// rustls の TLS クライアントを構築する
///
/// ALPN に [`MOQT_ALPN`] を設定する。TLS は QUIC で必須のため TLS 1.3 のみを使う。
fn build_tls_client(insecure: bool, ca_cert: Option<&str>) -> Result<s2n_rustls::Client, MoqError> {
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| MoqError::Tls(format!("failed to set TLS versions: {e}")))?;

    let mut config = if insecure {
        // 検証をスキップする (--cacert より優先する)
        tracing::warn!(target: "moq", "TLS certificate verification is disabled");
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth()
    } else if let Some(pem) = ca_cert {
        // 指定した CA 証明書で検証する
        let mut roots = rustls::RootCertStore::empty();
        for certificate in rustls_pki_types::CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let certificate = certificate
                .map_err(|e| MoqError::Tls(format!("failed to parse certificate: {e}")))?;
            roots
                .add(certificate)
                .map_err(|e| MoqError::Tls(format!("failed to add root certificate: {e}")))?;
        }
        if roots.is_empty() {
            return Err(MoqError::Tls("no certificate in --cacert".to_owned()));
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        // システムのルート証明書で検証する
        builder
            .with_platform_verifier()
            .map_err(|e| MoqError::Tls(format!("failed to use the platform verifier: {e}")))?
            .with_no_client_auth()
    };

    config.alpn_protocols = vec![MOQT_ALPN.to_vec()];
    Ok(s2n_rustls::Client::from(config))
}

/// 証明書検証をスキップする Verifier (`--insecure` 用)
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
