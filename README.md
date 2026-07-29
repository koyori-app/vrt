# VRT

Visual regression testing as a service. CI が撮ったスクリーンショットを受け取り、
承認済みの baseline と 1 枚ずつ突き合わせ、差分を人間がレビューして承認すると
その結果が次の baseline になる — という一周を回すための SaaS。

- **マルチテナント**: テナント（組織）> プロジェクト > ビルド > スクリーンショット
- **CI からは PAT ひとつ**: ビルド作成 → PNG アップロード → finalize → ポーリング
- **レビュー UI**: 差分のサイドバイサイド / オーバーレイ表示、キーボード操作
- **GitHub App 連携**: PR にレビュー結果を commit status として返す（任意）

設計の詳細は [docs/architecture.md](docs/architecture.md)。
GitHub App の設定は [docs/github-app.md](docs/github-app.md)。

## 構成

| ディレクトリ    | 中身                                                            |
| --------------- | --------------------------------------------------------------- |
| `apps/backend`  | Rust / axum / SeaORM / apalis。クレート分割は architecture.md 参照 |
| `apps/frontend` | TanStack Start (React 19) + Tailwind。SSR + `/api/*` プロキシ     |
| `e2e`           | Playwright。独立した pnpm ルート                                  |
| `docs`          | アーキテクチャと GitHub App セットアップ                          |

データストアは Postgres（本体 + ジョブキュー）と Valkey（セッション / OAuth state）。
スクリーンショットの実体はローカルディレクトリか S3 互換ストレージ。

## クイックスタート

```bash
docker compose up --build -d
```

- frontend: <http://localhost:3000>
- backend: <http://localhost:3500>（OpenAPI ビューアは `/scalar`）

マイグレーションは `migration` サービスが one-shot で適用する（冪等）。
止めるときは `docker compose down`、DB ごと捨てるなら `docker compose down -v`。

> compose の環境変数はすべて開発用のダミー。**本番では使わないこと。**
> OAuth ログインを実際に試すには、自分の GitHub / GitLab OAuth アプリの
> クライアント ID / シークレットを `docker-compose.yml` に入れる。

## 開発

Postgres と Valkey だけ compose から借りて、アプリはホストで動かすのが速い。

```bash
docker compose up -d db redis migration
```

### backend

```bash
cd apps/backend
cargo run --bin backend          # :3400 で待ち受け
cargo test --workspace           # 統合テストは testcontainers で DB を自前起動
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

`.env` かシェルの環境変数で `DATABASE_URL` / `REDIS_URL` などを渡す。
必須項目は `crates/common/src/settings.rs` にすべて書いてある（不足していれば起動時に落ちる）。

Linux では `.cargo/config.toml` が mold リンカを要求するので `mold` を入れておく。

storybook モード（サーバーサイドレンダリング）を動かすには Chromium が要る。
`CHROMIUM_PATH` に実行ファイルのパスを渡す（未設定なら storybook モードだけが
無効になり、起動時に warn が出る）。`cargo test` のレンダリング系テストは
`CHROMIUM_PATH` → `PATH` 上の chromium/chrome → Playwright のキャッシュ
（`~/.cache/ms-playwright`。e2e の `pnpm exec playwright install chromium` が入れる）
の順に探し、どこにも無ければスキップする。

### frontend

```bash
cd apps/frontend
pnpm install
pnpm openapi     # backend から openapi.json を書き出し → api.d.ts を生成
pnpm dev         # :3000。/api は vite プロキシで backend へ
pnpm typecheck
pnpm build
```

`src/generated/api.d.ts` は gitignore。**`openapi.json` はコミットする** —
これが frontend にとって唯一の契約ファイルで、CI の
`openapi-drift-check` が backend からの再エクスポートと突き合わせて守っている。

バックエンドの API を変えたら:

```bash
cd apps/frontend && pnpm openapi && git add openapi.json
```

### e2e

```bash
docker compose up -d db redis
cd e2e && pnpm install
pnpm exec playwright install chromium --with-deps   # 初回のみ
pnpm exec playwright test
```

backend と frontend は Playwright の `webServer` が自前で起動する。
詳細と、なぜテスト専用ログイン口が必要なのかは [e2e/README.md](e2e/README.md)。

## CI からスクリーンショットを送る

ビルドには 2 つのモードがある。

| mode | 誰が撮るか | CI が送るもの |
| --- | --- | --- |
| `screenshots`（既定） | CI | PNG を 1 枚ずつ |
| `storybook` | **VRT のサーバー** | ビルド済み Storybook の zip 1 本 |

どちらも finalize 以降（比較・レビュー・baseline 昇格）は同じ経路を通る。

### screenshots モード（既定）

1. UI の **Settings → Personal access tokens** で `write:build` と `read:build`
   を持つ PAT を発行する
2. `VRT_TOKEN` として CI のシークレットに入れる
3. ジョブの最後で 4 リクエスト投げる

```bash
export VRT_URL=https://vrt.example.com
export VRT_TOKEN=...        # CI のシークレットから

# 1. ビルドを作る（PAT に write:build が必要）
BUILD=$(curl -sS -X POST \
  "$VRT_URL/v1/ci/projects/<tenant-slug>/<project-slug>/builds" \
  -H "Authorization: Bearer $VRT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"branch":"'"$GIT_BRANCH"'","commit_sha":"'"$GIT_SHA"'"}' | jq -r .id)

# 2. スクリーンショットを 1 枚ずつ上げる（1 枚 25MB まで）
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/screenshots" \
  -H "Authorization: Bearer $VRT_TOKEN" \
  -F "name=home-page" \
  -F "file=@./screenshots/home-page.png"

# 3. finalize。ここで比較ジョブが積まれる
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/finalize" \
  -H "Authorization: Bearer $VRT_TOKEN"

# 4. processing を抜けるまでポーリングする
curl -sS "$VRT_URL/v1/ci/builds/$BUILD" -H "Authorization: Bearer $VRT_TOKEN"
```

最終ステータスは `passed`（差分なし）か `changes_detected`（要レビュー）か
`failed`。`changes_detected` で CI を落としたいなら 4 のレスポンスの
`status` を見て終了コードを決める。同じスニペットはプロジェクト画面の
**CI usage** タブにもテナント / プロジェクトの slug 入りで出る。

### storybook モード（サーバーサイドレンダリング）

CI 側にブラウザを用意せず、**ビルド済み Storybook の zip を投げるだけ**にする形。
VRT がバンドルを展開してローカルに配信し、ヘッドレス Chromium で全ストーリーを
`iframe.html?id=<storyId>&viewMode=story` から撮る。スクリーンショット名は
`{title}/{name}`（例 `Components/Button/Primary`）で、`index.json` の `docs`
エントリは撮らない。

#### `vrt` CLI で 1 コマンド（推奨）

同梱の CLI（`apps/backend/crates/cli`、バイナリ名 `vrt`）を使うと、ビルド作成 →
zip 化 → アップロード → finalize までを 1 コマンドで済ませられる。branch / commit
は git から自動で拾う。

```bash
cargo build --release -p vrt-cli   # target/release/vrt が生成される

export VRT_URL=https://vrt.example.com
export VRT_TOKEN=...                # write:build（--wait を使うなら read:build も）
export VRT_PROJECT=<tenant-slug>/<project-slug>   # CI usage タブに出る値

# 全ストーリーを撮る（storybook-static を丸ごと送る）
pnpm build-storybook
vrt upload --dir ./storybook-static

# 変更されたストーリーだけ撮り直す（TurboSnap 相当）
pnpm build-storybook --stats-json          # preview-stats.json を出す
vrt upload --dir ./storybook-static --only-changed --wait
```

`--wait` を付けるとビルドが決着するまでポーリングし、終了コードで結果を返す
（`passed`=0 / `changes_detected`=1 / `failed`=2）。CI のジョブをそのまま
落とせる。`--url` / `--token` / `--project` はフラグでも環境変数
（`VRT_URL` / `VRT_TOKEN` / `VRT_PROJECT`）でも渡せる。トークンはログに出さない。

`--only-changed` の前提:

- **stats-json が必要**: `storybook build --stats-json` で `preview-stats.json`
  を出す（既定の探索先は `<dir>/preview-stats.json`、`--stats-json` で変更可）。
  無い場合は警告して全撮影にフォールバックする
- **git 履歴が baseline コミットまで必要**: 差分は
  `git diff <baseline> HEAD` で取る。shallow clone で baseline が手元に無いと
  全撮影に倒れる。CI では `fetch-depth: 0`（または baseline に届く深さ）で clone する
- **自動で全撮影に倒れるケース**: baseline がまだ無い（初回・新規ブランチ）、
  `package.json` / lockfile（`pnpm-lock.yaml` / `yarn.lock` / `package-lock.json`）
  の変更、`.storybook/` 配下の変更、依存グラフに載っていない変更ファイル
  （拾い漏れを避けるため安全側に倒す）。`*.md` などレンダリングに無関係な
  グラフ外ファイルは無視する

#### 低レベル API（curl で直接叩く）

CLI を使わず生の API を叩く場合はこちら。

```bash
# 1. mode を指定してビルドを作る
BUILD=$(curl -sS -X POST \
  "$VRT_URL/v1/ci/projects/<tenant-slug>/<project-slug>/builds" \
  -H "Authorization: Bearer $VRT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"branch":"'"$GIT_BRANCH"'","commit_sha":"'"$GIT_SHA"'","mode":"storybook"}' | jq -r .id)

# 2. storybook-static を固めて 1 本だけ上げる（zip は 200MB まで）
pnpm build-storybook
(cd storybook-static && zip -qr ../storybook-static.zip .)
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/storybook" \
  -H "Authorization: Bearer $VRT_TOKEN" \
  -F "file=@./storybook-static.zip"

# 3. finalize。ここでレンダリングジョブが積まれる
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/finalize" \
  -H "Authorization: Bearer $VRT_TOKEN"
# 撮り直しを絞りたいときは only_story_ids を渡す（下の補足を参照）

# 4. rendering / processing を抜けるまでポーリングする
curl -sS "$VRT_URL/v1/ci/builds/$BUILD" -H "Authorization: Bearer $VRT_TOKEN"
```

補足:

- finalize のボディは任意。`{"only_story_ids": ["button--primary", …]}` を渡すと、
  そのストーリー ID だけを撮影し、残りは baseline のスクリーンショットを流用する
  （TurboSnap 相当。baseline に無い新規ストーリーは指定に無くても撮影される）。
  ボディ無し・空・`only_story_ids: null` は従来どおり全撮影。`screenshots`
  モードで渡すと 400。どのストーリーを渡すべきかを決める CLI は後続で用意する
- `storybook` モードのビルドに `POST .../screenshots` すると 409。バンドルは
  1 ビルドにつき 1 本だけで、2 回目のアップロードも 409
- finalize 後は `pending → rendering → processing → …` と進む。
  `rendering` 中に 1 ストーリーでも撮れなければビルドは `failed` になり、
  `error_message` にどのストーリーで落ちたかが入る
- 撮影サイズはプロジェクト設定の **Storybook viewport**（既定 1280x720）。
  UI の **Settings** タブか `PATCH /v1/projects/{id}` の
  `viewport_width` / `viewport_height` で変えられる
- サーバー側に Chromium が必要（`CHROMIUM_PATH`）。未設定のサーバーでは
  `mode=storybook` のビルド作成が 400 で拒否される。同梱の Docker イメージには
  Chromium が入っている

GitHub App を設定してあれば、承認 / 却下の結果は PR の commit status に返る
（[docs/github-app.md](docs/github-app.md)）。
