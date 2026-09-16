FROM ubuntu:24.04

# クロスコンパイルに必要な OS パッケージをインストールする。
# パッケージ一覧は CI と共有するため install-deps.sh に置く
COPY install-deps.sh /tmp/install-deps.sh
RUN chmod +x /tmp/install-deps.sh \
    && /tmp/install-deps.sh raspberrypi \
    && rm -f /tmp/install-deps.sh \
    && rm -rf /var/lib/apt/lists/*

# Rust をインストールする
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable
ENV PATH="/root/.cargo/bin:${PATH}"

RUN rustup target add aarch64-unknown-linux-gnu

# cargo-shiguredo-sysroot をインストールする
RUN cargo install shiguredo_sysroot
