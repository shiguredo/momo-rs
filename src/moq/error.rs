//! MOQT モードのエラー型

use std::fmt;

/// MOQT モードで発生するエラー
#[derive(Debug)]
pub enum MoqError {
    /// URL の解析に失敗した
    Url(String),
    /// TLS の設定に失敗した
    Tls(String),
    /// QUIC 接続またはストリーム操作に失敗した
    Quic(String),
    /// MOQT のセッションまたはプロトコル処理に失敗した
    Session(String),
}

impl fmt::Display for MoqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoqError::Url(message) => write!(f, "URL error: {message}"),
            MoqError::Tls(message) => write!(f, "TLS error: {message}"),
            MoqError::Quic(message) => write!(f, "QUIC error: {message}"),
            MoqError::Session(message) => write!(f, "MOQT session error: {message}"),
        }
    }
}

impl std::error::Error for MoqError {}
