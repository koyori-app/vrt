# syntax=docker/dockerfile:1
# backend イメージ。用途ごとに最終ステージを使い分ける。
#   runtime    … 開発・動作確認用（debug ビルド）。docker-compose.yml が使う
#   production … 配布用（release ビルド）
# ステージを省略すると最後の production になるので、開発用は target を明示すること。
#
# ── 起動手順（配布用イメージでも同じ）─────────────────────────────
#   1. /app/run-migration.sh   … migration up
#   2. /app/backend            … API サーバー起動（既定の CMD）
#   3. /app/vrt-runner         … 独立 Render worker（別サービスの command で指定）
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
    && cargo build --bin backend --bin vrt-runner \
    && cp target/debug/backend /out/backend \
    && cp target/debug/vrt-runner /out/vrt-runner

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/migration/target \
    cargo build --manifest-path migration/Cargo.toml \
    && cp migration/target/debug/migration /out/migration

########## builder: release ##########
FROM builder-base AS builder-release
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    mkdir -p /out \
    && cargo build --release --bin backend --bin vrt-runner \
    && cp target/release/backend /out/backend \
    && cp target/release/vrt-runner /out/vrt-runner

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/migration/target \
    cargo build --release --manifest-path migration/Cargo.toml \
    && cp migration/target/release/migration /out/migration

########## runtime 共通 ##########
FROM debian:bookworm-slim AS runtime-base
# chromium は storybook モード（サーバーサイドレンダリング）にだけ必要。
#
# ⚠️ イメージサイズへの影響: chromium とその依存（フォント含む）で
#    展開後 +400〜500MB 程度、さらに CJK フォント（fonts-noto-cjk）で +90MB 程度。
#    合計 slim ベースの ~80MB から ~640MB 前後になる。
#    storybook モードを使わない配布形態では、この apt 行と ENV CHROMIUM_PATH を
#    落としたバリアントを作ればサイズは元に戻る（CHROMIUM_PATH 未設定なら
#    mode=storybook のビルド作成が 400 で拒否されるだけで、他機能は影響を受けない）。
#
# フォントセットはレンダリング出力を決める入力そのもの。
# fonts-liberation は「フォントが 1 つも無いと欧文が豆腐になる」のを避ける最小限、
# fonts-noto-cjk は日本語・中国語・韓国語（CJK）が豆腐/文字化けになるのを避けるため
# （Noto CJK は単一ファミリで JP/CN/KR を賄うので、フォールバック順が安定し
#  決定的にレンダリングされる。fontconfig の追加設定は不要）。
#
# ⚠️ フォントの増減はレンダリング結果を変え、既存 baseline を全滅させる（全比較が差分になる）。
#    このリストはチームが baseline を承認し始めたら凍結すること。CJK を足すなら
#    「実運用の baseline がまだ無い今」が唯一の正しいタイミング。以後は追加しない。
#
# なお fonts-noto-cjk は展開後 +90MB 前後あり、runtime イメージがそのぶん太る。
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates postgresql-client chromium fonts-liberation fonts-noto-cjk \
    && rm -rf /var/lib/apt/lists/*

# service::render はこのパスの実行ファイルを起動する。
# 未設定だと storybook モードが無効になる（起動時に warn を出す）。
ENV CHROMIUM_PATH=/usr/bin/chromium

# root では動かさない
RUN useradd --system --create-home --uid 10001 app

# ローカルストレージ（LOCAL_UPLOAD_DIR の既定マウント先）を app 所有で用意しておく。
# docker-compose.yml がここに空の named volume `uploads` をマウントする。Docker は
# 「空の named volume を、イメージ内に既存のディレクトリへ当てると、そのディレクトリの
# 所有権を volume ルートへコピーする」ため、これで uid 10001(app) が書き込める。
# このディレクトリが無いと volume ルートが root 所有になり、非 root の backend が
# create_dir_all で EACCES になってアップロードが全部失敗する。
RUN mkdir -p /home/app/uploads && chown app:app /home/app/uploads

# 「Open Storybook」がアップロード済みバンドルを展開して静的配信するためのキャッシュ。
# STORYBOOK_CACHE_DIR の既定マウント先。派生データ（元は storybook.zip）なので永続化は不要
# ＝ named volume は当てない。コンテナを作り直すと空になり、初回アクセスで再展開されるだけ。
# app 所有で用意しておかないと非 root の backend が create_dir_all で EACCES になる。
RUN mkdir -p /home/app/storybook-cache && chown app:app /home/app/storybook-cache

WORKDIR /app

COPY --chown=app:app --chmod=755 docker/run-migration.sh /app/run-migration.sh

########## runtime: 開発・動作確認用 ##########
FROM runtime-base AS runtime
COPY --from=builder-debug --chown=app:app /out/backend /app/backend
COPY --from=builder-debug --chown=app:app /out/vrt-runner /app/vrt-runner
COPY --from=builder-debug --chown=app:app /out/migration /app/migration

USER app
EXPOSE 3400
CMD ["/app/backend"]

########## production: 配布用 ##########
FROM runtime-base AS production
COPY --from=builder-release --chown=app:app /out/backend /app/backend
COPY --from=builder-release --chown=app:app /out/vrt-runner /app/vrt-runner
COPY --from=builder-release --chown=app:app /out/migration /app/migration

USER app
EXPOSE 3400
CMD ["/app/backend"]
