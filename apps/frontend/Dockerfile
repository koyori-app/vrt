# syntax=docker/dockerfile:1
# frontend イメージ（TanStack Start SSR + srvx）。
# pnpm workspace のルートに package.json / pnpm-workspace.yaml があるので、
# ビルドコンテキストはリポジトリルート（`docker compose` の context: . と対応）。
#
# API 型（src/generated/api.d.ts）は gitignore されているので、コミット済みの
# openapi.json からビルド時に生成する（cargo は不要）。
#
# vite build の成果物は開発でも配布でも同じなので、runtime と production は同一内容。
# backend 側の Dockerfile と target 名を揃えるためにステージ名だけ分けてある。
#
# Node のバージョンは .github/workflows/frontend-build.yml の node-version と合わせること。

########## builder ##########
FROM node:24-bookworm-slim AS builder
# pnpm の版はルート package.json の packageManager フィールドに従わせる（二重管理を避ける）
RUN corepack enable
WORKDIR /repo

# ワークスペース一式をコピー（node_modules・dist 等は .dockerignore で除外済み）
COPY . .

# pnpm の store をキャッシュマウントに載せ、再ビルド時のパッケージ再取得を避ける。
# ルートの devDependencies（husky 等）は不要なので frontend だけに絞る。
# HUSKY=0: .git は .dockerignore で除外されているので、フックの導入は無意味。
RUN --mount=type=cache,target=/pnpm-store \
    HUSKY=0 pnpm install --frozen-lockfile --store-dir /pnpm-store --filter frontend...

WORKDIR /repo/apps/frontend
# openapi.json（コミット済み）→ 型生成 → ビルド
RUN pnpm run openapi:generate
RUN pnpm run build

########## runtime ##########
# server.mjs（srvx）は Node 実行前提。dist/client は同じプロセスが静的配信する。
FROM node:24-bookworm-slim AS runtime
WORKDIR /repo/apps/frontend

# pnpm の node_modules は .pnpm ストアへの相対 symlink 構成なので、ディレクトリごと持ってくる。
# devDependencies も混ざるが、prune のために install をもう一度回すより速く、
# この構成は開発・動作確認用なので許容する。
COPY --from=builder --chown=node:node /repo/apps/frontend/node_modules ./node_modules
COPY --from=builder --chown=node:node /repo/apps/frontend/dist ./dist
COPY --from=builder --chown=node:node /repo/apps/frontend/server.mjs ./server.mjs
COPY --from=builder --chown=node:node /repo/apps/frontend/package.json ./package.json

# node イメージに同梱の非 root ユーザー
USER node
ENV NODE_ENV=production
ENV PORT=3000
ENV HOST=0.0.0.0
EXPOSE 3000
CMD ["node", "./server.mjs"]

########## production: 配布用 ##########
# runtime と同一内容。backend の Dockerfile と同じ target 名で参照できるようにするためのエイリアス。
FROM runtime AS production
