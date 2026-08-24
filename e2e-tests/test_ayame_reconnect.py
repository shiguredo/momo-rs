"""Ayame モードの自動再接続の E2E テスト

到達できないシグナリング URL を使い、認証情報なしで再接続の挙動を検証する。
接続に失敗してもプロセスを終了させず、momo (C++) と同じ間隔 (10 秒刻みで上限 30 秒)
で再試行することを確認する。
"""

import time
import uuid

import pytest
from momo import Momo, MomoMode, read_log


def test_ayame_reconnects_when_signaling_unreachable(free_port, tmp_path):
    """シグナリングサーバーに接続できなくてもプロセスを終了せず再接続を続けることを確認する

    再接続を実装していなければ、接続失敗時に momo はプロセスを終了させる。
    """
    log_path = tmp_path / "momo-stderr.log"

    with Momo(
        mode=MomoMode.AYAME,
        ayame_signaling_url="ws://127.0.0.1:1/signaling",
        room_id="e2e-reconnect",
        metrics_port=free_port,
        fake_capture_device=True,
        log_level="info",
        startup_timeout=40,
        log_path=str(log_path),
    ) as m:
        # 2 回目の再接続待ち (10 秒) までログが進むことを待つ
        deadline = time.time() + 30
        while time.time() < deadline:
            if "retry_seconds=10" in read_log(log_path):
                break
            time.sleep(1)
        else:
            pytest.fail("再接続待ちのログが出ない = 再接続ループに入っていない")

        log = read_log(log_path)
        assert "connection failed" in log, "接続失敗がログに出るはず"

        # プロセスは終了せず、メトリクス応答も継続している
        assert m.process is not None and m.process.poll() is None, "momo は終了していないはず"
        assert m.get_metrics() is not None, "再接続中でもメトリクスが取得できるはず"


def test_ayame_reconnects_after_peer_leaves(free_port, port_allocator, tmp_path):
    """相手が退出して bye を受信中も、プロセスを終了せず再接続してルームに戻っていることを確認する

    ayame-labo を使い、相手役の momo を先に終了させてサーバーから bye を送らせる。
    momo (C++) は bye でも終了しないため、同じ挙動になっていることを見る。
    """
    signaling_url = "wss://ayame-labo.shiguredo.app/signaling"
    room_id = f"e2e-ayame-bye-{uuid.uuid4().hex[:8]}"
    log_path = tmp_path / "momo-stderr.log"

    with Momo(
        mode=MomoMode.AYAME,
        ayame_signaling_url=signaling_url,
        room_id=room_id,
        client_id="peer-b",
        metrics_port=free_port,
        fake_capture_device=True,
        log_level="info",
        log_path=str(log_path),
    ) as peer_b:
        with Momo(
            mode=MomoMode.AYAME,
            ayame_signaling_url=signaling_url,
            room_id=room_id,
            client_id="peer-a",
            metrics_port=next(port_allocator),
            fake_capture_device=True,
            log_level="info",
        ):
            # 両者の接続が確立するまで待つ
            time.sleep(10)
        # ここを抜けた時点で相手役が終了し、サーバーから bye が届く

        deadline = time.time() + 60
        while time.time() < deadline:
            if "bye 受信" in read_log(log_path):
                break
            time.sleep(1)
        else:
            pytest.fail("bye を受信していない = 相手の退出が検知されていない")

        deadline = time.time() + 30
        while time.time() < deadline:
            if read_log(log_path).count("accept 受信") >= 2:
                break
            time.sleep(1)
        else:
            pytest.fail("bye 受信後に再接続できていない")

        log = read_log(log_path)
        assert "reconnecting immediately" in log, "bye 後は即再接続に入るはず"
        assert peer_b.process is not None and peer_b.process.poll() is None, (
            "momo は終了していないはず"
        )
        assert peer_b.get_metrics() is not None, "再接続後もメトリクスが取得できるはず"
