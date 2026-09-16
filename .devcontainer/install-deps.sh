#!/usr/bin/env bash
set -euo pipefail

# クロスコンパイルに必要な OS パッケージをインストールする。
#
# コンテナイメージのビルド (.devcontainer/Dockerfile) と CI
# (.github/workflows/ci.yml) の両方から呼び出す。パッケージ一覧を 1 箇所に
# まとめて、ローカルと CI でビルド環境がずれないようにするため。
#
# 使い方:
#   ./install-deps.sh              # Raspberry Pi 以外向け
#   ./install-deps.sh raspberrypi  # Raspberry Pi 向け (keyring を追加する)
#
# root 以外で実行した場合は sudo を使う。

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

# --- ビルドツールと aarch64 クロスコンパイラ ---
# build-essential: ホスト側の C/C++ コンパイラ
# dpkg-dev: dpkg アーキテクチャの追加
# libclang-dev: bindgen が libclang を必要とする
$SUDO apt-get update
$SUDO apt-get install -y \
  build-essential \
  ca-certificates \
  curl \
  dpkg-dev \
  git \
  gcc-aarch64-linux-gnu \
  g++-aarch64-linux-gnu \
  libclang-dev \
  make \
  pkg-config

# --- Debian の keyring ---
# sysroot の生成元が Debian のリポジトリのため、署名検証に使う
$SUDO apt-get install -y clang debian-archive-keyring
$SUDO install -Dm644 /usr/share/keyrings/debian-archive-keyring.gpg \
  /etc/apt/trusted.gpg.d/debian-archive-keyring.gpg

# Ubuntu の keyring がある環境では同じく配置する
if [ -f /usr/share/keyrings/ubuntu-archive-keyring.gpg ]; then
  $SUDO install -Dm644 /usr/share/keyrings/ubuntu-archive-keyring.gpg \
    /etc/apt/trusted.gpg.d/ubuntu-archive-keyring.gpg
fi

# --- Raspberry Pi のアーカイブ keyring ---
# Raspberry Pi OS のリポジトリを sysroot の生成元にする場合に必要
if [ "${1:-}" = "raspberrypi" ]; then
  curl -fsSL http://archive.raspberrypi.com/debian/pool/main/r/raspberrypi-archive-keyring/raspberrypi-archive-keyring_2021.1.1+rpt1_all.deb \
    -o /tmp/rpi-keyring.deb
  $SUDO dpkg -i /tmp/rpi-keyring.deb
  rm /tmp/rpi-keyring.deb
  $SUDO install -Dm644 /usr/share/keyrings/raspberrypi-archive-keyring.gpg \
    /etc/apt/trusted.gpg.d/raspberrypi-archive-keyring.gpg
fi
