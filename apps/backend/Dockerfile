# syntax=docker/dockerfile:1
# backend イメージ。用途ごとに最終ステージを使い分ける。
#   runtime    … 開発・動作確認用（debug ビルド）。docker-compose.yml が使う
#   production … 配布用（release ビルド）
# ステージを省略すると最後の production になるので、開発用は target を明示すること。
#
# ── 起動手順（配布用イメージでも同じ）─────────────────────────────
#   1. /app/run-migration.sh   … migration up
#   2. /app/backend            … サーバー起動（既定の CMD）
# ─────────────────────────────────────────────────
#
# Rust のバージョンはリポジトリルートの rust-toolchain.toml と合わせること。
# このイメージのビルドコンテキストは apps/backend なので rust-toolchain.toml はコンテキストに
# 含まれず、ここでの指定が唯一の版指定になる（rust-toolchain.toml を上げても自動追従しない）。

########## builder 共通 ##########
FROM rust:1.95-bookworm AS builder-base
# .cargo/config.toml が Linux ビルドで mold リンカ（-fuse-ld=mold）を要求する
RUN apt-get update \
    && apt-get install -y --no-install-recommends mold git \
    && rm -rf /var/lib/apt/lists/*
# auth-core は git 依存（public リポジトリなので認証情報は不要）。cargo 内蔵の git
# 実装ではなく git CLI を使わせる方が、プロキシ環境などで安定して取得できる。
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
WORKDIR /build

# ワークスペース一式（crates/ + migration/）をコピー
COPY . .

########## builder: debug ##########
# cargo のレジストリと target をキャッシュマウントに載せ、再ビルド時の依存クレート
# 再取得・再コンパイルを避ける。キャッシュマウントの中身は次のステージへ COPY できないため、
# 成果物のバイナリだけ /out に取り出す。
FROM builder-base AS builder-debug
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    mkdir -p /out \
    && cargo build --bin backend \
    && cp target/debug/backend /out/backend

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/migration/target \
    cargo build --manifest-path migration/Cargo.toml \
    && cp migration/target/debug/migration /out/migration

########## builder: release ##########
FROM builder-base AS builder-release
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    mkdir -p /out \
    && cargo build --release --bin backend \
    && cp target/release/backend /out/backend

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/migration/target \
    cargo build --release --manifest-path migration/Cargo.toml \
    && cp migration/target/release/migration /out/migration

########## runtime 共通 ##########
FROM debian:bookworm-slim AS runtime-base
# chromium は storybook モード（サーバーサイドレンダリング）にだけ必要。
#
# ⚠️ イメージサイズへの影響: chromium とその依存（フォント含む）で
#    展開後 +400〜500MB 程度になる（slim ベースの ~80MB から ~550MB 前後）。
#    storybook モードを使わない配布形態では、この apt 行と ENV CHROMIUM_PATH を
#    落としたバリアントを作ればサイズは元に戻る（CHROMIUM_PATH 未設定なら
#    mode=storybook のビルド作成が 400 で拒否されるだけで、他機能は影響を受けない）。
#
# fonts-liberation は「フォントが 1 つも無いと文字が豆腐になる」のを避ける最小限。
# レンダリング結果の決定性のため、これ以上フォントは足さないこと
# （増やすとフォント選択が変わって既存 baseline が全部差分になる）。
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates postgresql-client chromium fonts-liberation \
    && rm -rf /var/lib/apt/lists/*

# service::render はこのパスの実行ファイルを起動する。
# 未設定だと storybook モードが無効になる（起動時に warn を出す）。
ENV CHROMIUM_PATH=/usr/bin/chromium

# root では動かさない
RUN useradd --system --create-home --uid 10001 app
WORKDIR /app

COPY --chown=app:app --chmod=755 docker/run-migration.sh /app/run-migration.sh

########## runtime: 開発・動作確認用 ##########
FROM runtime-base AS runtime
COPY --from=builder-debug --chown=app:app /out/backend /app/backend
COPY --from=builder-debug --chown=app:app /out/migration /app/migration

USER app
EXPOSE 3400
CMD ["/app/backend"]

########## production: 配布用 ##########
FROM runtime-base AS production
COPY --from=builder-release --chown=app:app /out/backend /app/backend
COPY --from=builder-release --chown=app:app /out/migration /app/migration

USER app
EXPOSE 3400
CMD ["/app/backend"]
