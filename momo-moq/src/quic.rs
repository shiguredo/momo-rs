//! MOQT の QUIC 接続確立
//!
//! TLS / ALPN / transport parameter を構築し、`tobi-quic` で接続を確立する。
//! QUIC 実装への依存はこのファイルに閉じ込め、他は `transport.rs` 越しに扱う。

use std::net::SocketAddr;
use std::sync::Arc;

// PEM 形式の証明書を読み込むためのトレイト
use rustls_pki_types::pem::PemObject;
// システムのルート証明書を使う TLS 設定を構築するためのトレイト
use rustls_platform_verifier::BuilderVerifierExt;
use tobi_quic::core::connection::ConnectionConfig;
use tobi_quic::{Client, ClientTlsConfig, Connect, Connection, DatagramConfig, IoConfig};

use crate::error::MoqError;
use crate::url::ServerUrl;

/// MOQT の ALPN プロトコル識別子
///
/// `draft-ietf-moq-transport-21` §6.2 (Session establishment) が ALPN を `moqt-21` と定める。
/// ALPN は draft 版ごとに変わるため、接続先の実装と同じ版に合わせる必要がある。
const MOQT_ALPN: &[u8] = b"moqt-21";

/// ピアが開ける単方向ストリームの累積上限
///
/// relay は audio を 20ms ごと、video を 1 frame ごとに 1 本の subgroup stream で
/// 送るため、stream の消費速度が上限の回復速度を上回り得る。s2n-quic の既定
/// (100 本) では購読が数秒から数十秒で止まるため、余裕を持たせる。
const INITIAL_MAX_STREAMS_UNI: u64 = 10_000;

/// ピアが開ける双方向ストリームの累積上限
///
/// 双方向ストリームは SUBSCRIBE / PUBLISH / FETCH などの request stream に使う。
/// 同時に処理する request 数より十分大きい値にする。
const INITIAL_MAX_STREAMS_BIDI: u64 = 1_000;

/// 接続レベルで受け入れを許す総バイト数
const INITIAL_MAX_DATA: u64 = 64 * 1024 * 1024;

/// ストリーム 1 本あたりの受信ウィンドウ
///
/// 1 本の subgroup stream に 1 group 分の media が載るため、group を
/// 読み切れるだけの窓を確保する。
const INITIAL_MAX_STREAM_DATA: u64 = 16 * 1024 * 1024;

/// アイドルタイムアウト (ミリ秒)
///
/// tobi-quic の既定は 0 (無効) であり、そのままでは応答しない peer との接続が
/// 残り続ける。s2n-quic の既定 (30 秒) に合わせる。
const MAX_IDLE_TIMEOUT_MS: u64 = 30_000;

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

    // ホスト名は DNS 解決する。IPv4 / IPv6 リテラルも lookup_host で解決できる。
    // 解決結果の順序は環境依存で IPv6 が先に来ることがあり、IPv6 が通らない環境では
    // 接続がタイムアウトするため、IPv4 を優先して選ぶ
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((server.host(), server.port()))
        .await
        .map_err(|e| MoqError::Quic(format!("failed to resolve '{}': {e}", server.host())))?
        .collect();
    let socket_addr = addresses
        .iter()
        .find(|addr| addr.is_ipv4())
        .or_else(|| addresses.first())
        .copied()
        .ok_or_else(|| MoqError::Quic(format!("no address found for '{}'", server.host())))?;

    // ローカルの待ち受けアドレスは接続先と同じアドレスファミリにする。
    // IPv6 の接続先に IPv4 のソケットで送信しようとすると失敗するため
    let bind_addr = if socket_addr.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };

    let client = Client::builder()
        .with_tls(ClientTlsConfig::new(Arc::new(tls)))
        .with_io(IoConfig::new(bind_addr))
        .with_transport(build_transport_config())
        // MOQT の Object Datagram は QUIC DATAGRAM 拡張 (RFC 9221) を使うため、
        // DATAGRAM の受信を広告する。momo-moq 自身は datagram を送らないが、
        // relay が datagram での配信可否を判断できるようにしておく
        .with_datagram(DatagramConfig::default())
        // s2n-quic の endpoint はアプリとは独立したタスクで常時ソケットを駆動していた。
        // tobi-quic の既定はコマンド駆動であり、アプリが API を呼んでいない間は受信と
        // タイマーを処理しないため、同じ挙動になるよう接続生成時からバックグラウンド
        // 駆動を有効にする。無効のままだと ACK や再送が止まる
        .with_background_driver(true)
        .start()
        .map_err(|e| MoqError::Quic(format!("failed to start QUIC client: {e}")))?;

    let connection = client
        .connect(Connect::new(socket_addr, server.host()))
        .await
        .map_err(|e| MoqError::Quic(format!("failed to connect to {socket_addr}: {e}")))?;

    tracing::info!(target: "moq", addr = %socket_addr, "connected to MOQT server");
    Ok(connection)
}

/// 接続で広告する transport parameter を構築する
///
/// MOQT は 1 セッションで多数の subgroup stream と request stream を同時に使う。
/// tobi-quic の既定 (単方向 100 本 / 双方向 100 本 / 接続 1 MiB / ストリーム 64 KiB)
/// では受信が追いつかず flow control で止まるため、上限とウィンドウを明示する。
fn build_transport_config() -> ConnectionConfig {
    let mut config = ConnectionConfig::default();
    config.transport_params.initial_max_streams_uni = INITIAL_MAX_STREAMS_UNI;
    config.transport_params.initial_max_streams_bidi = INITIAL_MAX_STREAMS_BIDI;
    config.transport_params.initial_max_data = INITIAL_MAX_DATA;
    config.transport_params.initial_max_stream_data_uni = INITIAL_MAX_STREAM_DATA;
    config.transport_params.initial_max_stream_data_bidi_local = INITIAL_MAX_STREAM_DATA;
    config.transport_params.initial_max_stream_data_bidi_remote = INITIAL_MAX_STREAM_DATA;
    // multipath は tobi-quic の既定 (initial_max_path_id = 4) のまま有効にしておく
    config.transport_params.max_idle_timeout = MAX_IDLE_TIMEOUT_MS;
    config
}

/// rustls の TLS クライアントを構築する
///
/// ALPN に [`MOQT_ALPN`] を設定する。TLS は QUIC で必須のため TLS 1.3 のみを使う。
fn build_tls_client(
    insecure: bool,
    ca_cert: Option<&str>,
) -> Result<rustls::ClientConfig, MoqError> {
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
    Ok(config)
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
