"""Sora モードの自動再接続の E2E テスト

到達できないシグナリング URL を使い、認証情報なしで再接続の挙動を検証する。
momo (C++) と同じく、接続に失敗してもプロセスを終了させずに再接続を続けることを
確認する。実 Sora への接続を必要とするテストは `test_sora_mode.py` 側にある。
"""

import pytest
from momo import Momo, MomoMode, read_log


def test_sora_keeps_reconnecting_when_connection_fails(free_port, tmp_path):
    """接続に失敗したあと、プロセスを終了させずに再接続を続けることを確認する

    存在しないポートをシグナリング URL として指定する。接続は必ず失敗するので、
    再接続を実装していなければ momo はプロセスを終了させる。
    """
    log_path = tmp_path / "momo-stderr.log"

    momo = Momo(
        mode=MomoMode.SORA,
        signaling_urls="wss://127.0.0.1:1/signaling",
        channel_id="e2e-reconnect",
        role="sendonly",
        metrics_port=free_port,
        fake_capture_device=True,
        log_level="info",
        # 再接続の間隔は 5 秒、15 秒と伸びるので、2 回目の再接続まで見届ける
        startup_timeout=25,
        log_path=str(log_path),
    )

    # stats が揃わないため起動待ち自体は失敗する。プロセスが終了していなければ
    # "failed to start"、先に終了していれば "exited unexpectedly" が返ってくる。
    with pytest.raises(RuntimeError, match="failed to start"), momo:
        pass

    log = read_log(log_path)
    assert "connection failed" in log, "接続失敗がログに出るはず"
    assert log.count("reconnecting to Sora") >= 2, "再接続を 2 回以上試みているはず"
    assert "retry_seconds=5" in log, "1 回目の待機は 5 秒のはず"
    assert "retry_seconds=15" in log, "2 回目の待機は 15 秒のはず"


def test_sora_reopens_capture_devices_on_each_attempt(free_port, tmp_path):
    """再接続のたびにフェイク音声・フェイク映像の生成者を準備し直すことを確認する

    接続設定は 1 回だけ検証し、デバイス相当の準備は接続ごとにやり直す設計になっている。
    """
    log_path = tmp_path / "momo-stderr.log"

    momo = Momo(
        mode=MomoMode.SORA,
        signaling_urls="wss://127.0.0.1:1/signaling",
        channel_id="e2e-reconnect-capture",
        role="sendonly",
        metrics_port=free_port,
        fake_capture_device=True,
        log_level="info",
        startup_timeout=12,
        log_path=str(log_path),
    )

    with pytest.raises(RuntimeError, match="failed to start"), momo:
        pass

    log = read_log(log_path)
    assert log.count("fake audio started") >= 2, "接続ごとにフェイク音声を準備するはず"
    assert log.count("fake video started") >= 2, "接続ごとにフェイク映像を準備するはず"
