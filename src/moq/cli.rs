//! MOQT (Media over QUIC Transport) モードの CLI
//!
//! `momo moq publish` / `momo moq subscribe` のサブコマンドを提供する。
//! 現時点では MOQT セッションの確立と制御メッセージの処理までを実装している。

use std::time::Duration;

use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::SessionEvent;
use tracing::info;

use crate::error::BoxError;
use crate::moq::{ControlStream, MoqConfig, establish_session};

/// セッション確立後に制御メッセージを処理し続ける時間
///
/// トラックの publish / subscribe を実装するまでの暫定値。
const OBSERVE_DURATION: Duration = Duration::from_secs(5);

/// `momo moq` サブコマンドを実行する
///
/// 第 1 サブコマンドで役割 (publish / subscribe) を選ぶ。
pub async fn run(mut args: noargs::RawArgs) -> noargs::Result<()> {
    noargs::HELP_FLAG.take_help(&mut args);

    if noargs::cmd("subscribe")
        .doc("Subscribe to MOQT tracks and play them")
        .take(&mut args)
        .is_present()
    {
        run_role(args, Role::Subscriber).await
    } else if noargs::cmd("publish")
        .doc("Publish MOQT tracks")
        .take(&mut args)
        .is_present()
    {
        run_role(args, Role::Publisher).await
    } else if let Some(help) = args.finish()? {
        print!("{}", help);
        Ok(())
    } else {
        Err(noargs::Error::other(
            &noargs::raw_args(),
            "moq mode requires a subcommand: publish or subscribe",
        ))
    }
}

/// MOQT のロール
#[derive(Debug, Clone, Copy)]
enum Role {
    /// トラックを publish する側
    Publisher,
    /// トラックを subscribe する側
    Subscriber,
}

impl Role {
    /// ログに出す名前を返す
    fn as_str(self) -> &'static str {
        match self {
            Role::Publisher => "publisher",
            Role::Subscriber => "subscriber",
        }
    }
}

/// `publish` / `subscribe` の共通処理を実行する
async fn run_role(mut args: noargs::RawArgs, role: Role) -> noargs::Result<()> {
    noargs::HELP_FLAG.take_help(&mut args);
    let config = parse_common_options(&mut args)?;

    if let Some(help) = args.finish()? {
        print!("{}", help);
        return Ok(());
    }

    if let Err(e) = run_session(config, role).await {
        return Err(noargs::Error::other(&noargs::raw_args(), format!("{e}")));
    }
    Ok(())
}

/// `publish` / `subscribe` で共通のオプションを解析する
fn parse_common_options(args: &mut noargs::RawArgs) -> noargs::Result<MoqConfig> {
    let url: String = noargs::opt("url")
        .ty("URL")
        .doc("MOQT relay URL (moqt://host:port/path)")
        .take(args)
        .then(|o| o.value().parse())?;

    let ca_cert: Option<String> = noargs::opt("ca-cert")
        .ty("PATH")
        .doc("CA certificate file for verifying the relay")
        .take(args)
        .present()
        .map(|o| o.value().to_owned());

    Ok(MoqConfig {
        url,
        insecure: ca_cert.is_none(),
        ca_cert,
    })
}

/// MOQT セッションを確立して制御メッセージを処理する
///
/// 現時点ではトラックの publish / subscribe を行わないため、確立後に制御メッセージを
/// 処理しながら一定時間待機する。
async fn run_session(config: MoqConfig, role: Role) -> Result<(), BoxError> {
    let (mut session, mut control) = establish_session(&config).await?;
    info!(
        target: "moq",
        role = role.as_str(),
        url = %config.url,
        state = ?session.state(),
        "MOQT session is ready"
    );

    observe_session(&mut session, &mut control).await
}

/// 制御メッセージを処理しながら一定時間待機する
///
/// 相手から届いた制御メッセージは [`Session::recv_control`] に渡し、
/// 処理の結果として生じたイベントを [`Session::poll_event`] で取り出す。
async fn observe_session(
    session: &mut Session,
    control: &mut ControlStream,
) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + OBSERVE_DURATION;
    loop {
        while let Some(event) = session.poll_event() {
            match event {
                SessionEvent::CloseSession(reason) => {
                    info!(target: "moq", ?reason, "session closed by peer");
                    return Ok(());
                }
                SessionEvent::Established => {}
                // Session が生成した制御メッセージを送出する
                SessionEvent::SendControl(message) => {
                    control.send(&message).await?;
                }
                other => {
                    info!(target: "moq", ?other, "session event");
                }
            }
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            info!(target: "moq", "observation finished");
            return Ok(());
        }

        // 制御メッセージの到着を待つ。時間切れの場合はループ先頭に戻って終了判定する
        let Some(message) = control.reader.read_message(remaining).await? else {
            continue;
        };
        session
            .recv_control(message)
            .map_err(|e| format!("failed to handle control message: {e}"))?;
    }
}
