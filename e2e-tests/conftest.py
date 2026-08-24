import itertools
import os
import time
import uuid
from dataclasses import dataclass
from pathlib import Path

import jwt
import pytest
from dotenv import load_dotenv

# e2e-tests/.env を読み込む（pytest の cwd がリポジトリルートでも動作する）
load_dotenv(Path(__file__).parent / ".env")


@dataclass
class SoraSettings:
    """Sora モード用の設定"""

    signaling_urls: str
    channel_id_prefix: str
    secret_key: str
    channel_id: str
    metadata: dict


# Sora モードの E2E テストに必要な環境変数
SORA_ENV_VARS = (
    "TEST_SORA_MODE_SIGNALING_URLS",
    "TEST_SORA_MODE_CHANNEL_ID_PREFIX",
    "TEST_SORA_MODE_SECRET_KEY",
)


def _sora_env() -> dict[str, str]:
    """Sora モードの E2E に必要な環境変数をまとめて取得する。

    未取得のものがあればテストをスキップする。認証情報を持たない環境
    (フォークや開発中のクローン) で E2E を走らせたときに、
    失敗ではなくスキップとして扱うため。
    """
    values = {name: os.environ.get(name, "") for name in SORA_ENV_VARS}
    missing = [name for name in SORA_ENV_VARS if not values[name]]
    if missing:
        pytest.skip(f"Sora モードの E2E に必要な環境変数が未設定: {', '.join(missing)}")
    return values


@pytest.fixture
def sora_settings():
    """Sora モード用の設定を提供するフィクスチャ（各テストごとに新しいchannel_idを生成）"""
    env = _sora_env()
    # カンマ区切りをスペース区切りに変換
    signaling_urls = env["TEST_SORA_MODE_SIGNALING_URLS"].replace(",", " ")
    channel_id_prefix = env["TEST_SORA_MODE_CHANNEL_ID_PREFIX"]
    secret_key = env["TEST_SORA_MODE_SECRET_KEY"]

    # チャンネルIDを生成
    channel_id = f"{channel_id_prefix}{uuid.uuid4().hex[:8]}"

    # メタデータを生成
    payload = {
        "channel_id": channel_id,
        "exp": int(time.time()) + 300,
    }
    access_token = jwt.encode(payload, secret_key, algorithm="HS256")
    metadata = {"access_token": access_token}

    return SoraSettings(
        signaling_urls=signaling_urls,
        channel_id_prefix=channel_id_prefix,
        secret_key=secret_key,
        channel_id=channel_id,
        metadata=metadata,
    )


@pytest.fixture(scope="session")
def port_allocator():
    """セッション全体で共有されるポート番号アロケーター

    エフェメラルポート開始の 55000 から始まるポート番号を順番に生成します。
    複数のテストが並列実行されても、各テストに一意のポート番号が割り当てられます。
    """
    return itertools.count(56000)


@pytest.fixture
def free_port(port_allocator):
    """利用可能なポート番号を提供するフィクスチャ

    各テスト関数で使用すると、自動的に一意のポート番号が割り当てられます。
    """
    return next(port_allocator)
