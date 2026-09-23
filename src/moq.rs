//! MOQT (Media over QUIC Transport) モード
//!
//! `draft-ietf-moq-transport-21` に基づくクライアント機能を提供する。
//! Sans-I/O な `shiguredo_moqt` の `Session` と `s2n-quic` の I/O は
//! [`client::MoqtClient`] が繋ぐ。draft 由来の仕様のため、将来の改版で挙動が
//! 変わる可能性がある。

pub mod catalog;
pub mod cli;
pub mod client;
pub mod config;
pub mod error;
pub mod h264;
pub mod media;
pub mod opus;
pub mod publisher;
pub mod quic;
pub mod subscriber;
pub mod transport;
pub mod url;
