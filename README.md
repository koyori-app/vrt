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

#### 撮る story を絞る（`vrt plan`）

`screenshots` モードでは撮影そのものが CI のテストランナーの仕事であり、VRT は
レンダリングしない。
そのため CLI は撮影を代行せず、`vrt plan` で「撮るべき story の集合」だけを
JSON で出力する。
選択の中身は `storybook` モードの `--only-changed` と同じ依存グラフ解析
（TurboSnap 相当）を使う。

```bash
set -euo pipefail
export VRT_URL=https://vrt.example.com
export VRT_TOKEN=...                              # write:build
export VRT_PROJECT=<tenant-slug>/<project-slug>

pnpm build-storybook --stats-json                 # preview-stats.json と index.json を出す
vrt plan --dir ./storybook-static --output plan.json

# build_id はキーがあるときだけ使う（--baseline-commit 経路では省略される）。
BUILD=$(jq -r '.build_id // empty' plan.json)

# 契約 version を確認する。未知の値なら計画を捨てて全撮影へ倒す。
PLAN_VERSION=$(jq -r '.version' plan.json)
PLAN_KIND=$(jq -r '.plan' plan.json)
if [ "$PLAN_VERSION" != "1" ]; then
  PLAN_KIND=capture_all
fi

# plan の値をセンチネルとして後段へ渡す（only+空 story_ids と capture_all の取り違え防止）。
printf '%s\n' "$PLAN_KIND" > .vrt-plan-kind

case "$PLAN_KIND" in
  only)
    # 空配列なら「撮る story 無し」。capture_all とは別物。
    jq -r '.story_ids[]?' plan.json > stories.txt
    ;;
  capture_all)
    # 後段は .vrt-plan-kind を見て全 story を撮ること（空 stories.txt だけでは判断しない）。
    ;;
  *)
    printf '%s\n' capture_all > .vrt-plan-kind
    ;;
esac
```

`vrt plan` は環境の不備では計画を出さず、終了コード 2 で落ちる。
全撮影へは倒さないので、呼び出し側で必ず失敗として扱うこと。
黙って全撮影に読み替えると、設定ミスに気づけなくなる。

絞り込み（`plan = "only"` の算出）には前提条件がある。stats / index は
worktree のファイルから読むため、worktree の内容が計画の終点コミットと
一致していなければ「別内容に対する計画」ができてしまう。次を満たさない場合、
`vrt plan` は全撮影へ倒さず終了コード 2 で落ちる。

- worktree の `HEAD` が `--commit`（省略時は `HEAD` 解決値）と一致していること
- 追跡ファイルに未コミットの変更が無いこと（未追跡ファイルは対象外）

#### 部分アップロードを finalize で宣言する（`captured_names`）

計画に従って一部の story だけを撮った場合、そのままアップロードして finalize
すると、撮らなかった名前の baseline エントリがすべて `removed` になってしまう。
サーバーは「アップロードされなかった」と「削除された」を区別できないからである。

そこで部分アップロードでは、finalize のボディで **今回撮った名前の集合** を宣言する。

```bash
# 撮った screenshots を通常どおりアップロードした後、名前を宣言して finalize する
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/finalize" \
  -H "Authorization: Bearer $VRT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"captured_names": ["home-page", "pricing"]}'
```

宣言があると、サーバーは次のように振る舞う。

- **宣言 == 実際にアップロードされた名前** を検証し、一致しなければ 400 で拒否する。
  宣言したのにアップロードが欠けた名前を黙って流用に回すと、撮影の失敗が
  「差分なし」に化けるためである（逆方向の過剰アップロードも計画とのずれとして拒否）
- 宣言に無い名前の baseline エントリは `removed` にせず、前回 baseline の画像を
  このビルドのスクリーンショットとして流用する（比較は `unchanged` になり、
  承認しても baseline から消えない）。`storybook` モードの `only_story_ids` と
  同じ帰結になる
- `captured_names: []`（何も撮らない）は全エントリ流用の宣言として有効

このため部分アップロードでは story の削除は検出されない。story を削除した
ときは全撮影（宣言なしの finalize）で流し、`removed` をレビューで承認すること。

`expected_baseline_commit_sha` を併せて渡すと、計画の起点にした baseline と
ビルドに固定された baseline の一致を finalize 時点で検証できる（不一致は 400）。
baseline はビルド**作成時**に解決して固定され、比較もその固定値に対して走る。
作成後に別ビルドが承認されて最新 baseline が動いても、このビルドの比較はずれない。

計画は stdout にも必ず出るので、`--output` を使わずパイプで受けてもよい。
ログは stderr へ出すため、stdout は JSON だけになる。

出力の契約は次のとおり。

| フィールド | 意味 |
| --- | --- |
| `version` | 契約 version。未知の値なら計画を捨てて全撮影へ倒す |
| `plan` | `only`（列挙した story だけ撮る）か `capture_all`（全 story を撮る） |
| `story_ids` | 撮る story ID。`plan` が `only` のときだけ載る（空配列を含む） |
| `reason` | 全撮影へ倒した理由。`plan` が `only` では `null` |
| `baseline_commit_sha` / `head_commit_sha` | 計画が前提とした差分の起点と終点 |
| `build_id` | 計画のために作成した `screenshots` ビルド。撮影結果はこのビルドへ送る。ビルドを作らなかった場合はキーを省略する |
| `notes` | 判断の補足（レンダリングに無関係として無視したファイルなど） |

`story_ids` が空配列であることと `plan` が `capture_all` であることは別物である。
前者は「影響のある既存 story は無い」という選択結果、後者は「選択を諦めた」である。
撮らなかった story を差分なしとして扱ってはならない。

`baseline_commit_sha` と `head_commit_sha` は計画に焼き付けてある。
計画を作ってから撮るまでにどちらかが変わった場合は、計画を捨てて全 story を撮る。

`plan` が `capture_all` に倒れる条件は `--only-changed` と同じで、次を含む。

- サーバー解決経路で baseline がまだ無い（初回や新規ブランチ）
- サーバー解決経路で baseline コミットが手元に無く `git diff` が取れない（`fetch-depth: 0` で clone する）
- `preview-stats.json` が無い、または壊れている
- `index.json` が壊れている
- `package.json` や lockfile、`.storybook/` 配下の変更
- 依存グラフに載っていない変更ファイル

baseline を自分で決めている場合は `--baseline-commit <sha>` を渡せる。
その場合はビルドを作らないので、ネットワークにも触らず `build_id` キーも省略される。
**`--baseline-commit` を明示した経路では**、指定した rev が手元に無い・解決できない・
`git diff` が取れないときは `capture_all` へ倒さず **終了コード 2** で落ちる
（設定ミスを黙って全撮影に読み替えない）。

`--baseline-commit` を渡さずサーバーから baseline を解決する経路では、計画用の
`screenshots` ビルドが作成される。撮影結果をその `build_id` に送らず放置すると
ビルドは `pending` のまま残る。計画だけ欲しい場合でも、不要ならビルドの破棄運用を
別途検討するか、`--baseline-commit` でビルド作成を回避する。
根本対応（計画専用エンドポイント等）は後続 PR で検討する。

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

CLI の入手方法は 2 つ。**リリース版のバイナリを落とすのが速い**（ソースビルドは
初回 15〜20 分かかる）。

```bash
# リリースからバイナリを取得（推奨）。cli-v* タグごとに配布している。
VRT_CLI_VERSION=cli-v0.1.0
VRT_CLI_TARGET=x86_64-unknown-linux-gnu
BASE="https://github.com/koyori-app/vrt/releases/download/${VRT_CLI_VERSION}"

# チェックサムはアーカイブ名込みで記録してあるので、配布時のファイル名のまま落とす。
curl -fsSL -O "${BASE}/vrt-${VRT_CLI_TARGET}.tar.gz"
curl -fsSL -O "${BASE}/vrt-${VRT_CLI_TARGET}.tar.gz.sha256"

# 検証してから展開する（macOS は shasum、Linux は sha256sum -c でも同じ）
shasum -a 256 -c "vrt-${VRT_CLI_TARGET}.tar.gz.sha256"

tar xzf "vrt-${VRT_CLI_TARGET}.tar.gz" && chmod +x vrt
```

配布ターゲットは `x86_64` / `aarch64` の Linux（`-unknown-linux-gnu`）と macOS
（`-apple-darwin`）。

```bash
# ソースからビルドする場合（Cargo ワークスペースは apps/backend、Linux は mold が要る）
cargo build --release -p vrt-cli   # target/release/vrt が生成される
```

```bash
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

`--json` を付けると、build ID・build 番号・slug・最終ステータス・終了コードを
stdout へ JSON で 1 行だけ出す（例
`{"build_id":"…","build_number":123,"tenant_slug":"koyori","project_slug":"task","status":"changes_detected","exit_code":1}`）。
GitHub Action など呼び出し元がパースしやすいよう、このときログはすべて stderr に回る。

ただし **JSON が出るのはビルドを作成して finalize まで到達した場合だけ**。それ以前の
失敗（設定不備、ネットワークエラー、stats JSON の解析失敗など）では stdout は空のまま
stderr にエラーを出して終了コード 2 で終わる。呼び出し元は「stdout が空で非ゼロ終了」を
必ず処理すること。

`--wait` 併用時、finalize には成功したがその後のポーリングが一時的な通信失敗や
タイムアウト（既定 30 分）で失敗した場合も **JSON は 1 行出る**。このときの JSON は
finalize 時点の既知情報（`build_id` / `build_number` / `tenant_slug` /
`project_slug` / finalize 直後の `status`）に `exit_code: 2` と失敗理由の `error`
フィールド（例
`{"build_id":"…","build_number":123,"tenant_slug":"koyori","project_slug":"task","status":"processing","exit_code":2,"error":"timed out after 1800s …"}`）
を添えて出す。`error` は失敗時のみ現れ、成功時の JSON 形状は変わらない。これにより
呼び出し元は少なくとも `build_id` を取り出して後続処理（結果 URL の組み立てや
再ポーリング）に使える。

`error` が現れるのはポーリング失敗時だけではない。`--wait` でポーリングが終端まで
到達し、ビルド自体が `failed` / `rejected` で終わってサーバーが失敗理由
（`error_message`）を返した場合も、その理由が `exit_code: 2` とともに `error` に載る
（どのストーリーで落ちたか等）。この場合も成功時（`error_message` が無いとき）は
`error` キーが出ない契約は変わらない。

`--only-changed` の前提:

- **stats-json が必要**: `storybook build --stats-json` で `preview-stats.json`
  を出す（既定の探索先は `<dir>/preview-stats.json`、`--stats-json` で変更可）。
  無い場合は警告して全撮影にフォールバックする
- **git 履歴が baseline コミットまで必要**: 差分は
  `git diff <baseline> <commit>` で取る（`<commit>` は `--commit` 解決値、
  未指定なら worktree の `HEAD`）。shallow clone で baseline が手元に無いと
  全撮影に倒れる。CI では `fetch-depth: 0`（または baseline に届く深さ）で clone する
- **自動で全撮影に倒れるケース**: サーバーから返った baseline がまだ無い（初回・新規ブランチ）、
  `package.json` / lockfile（`pnpm-lock.yaml` / `yarn.lock` / `package-lock.json`）
  の変更、`.storybook/` 配下の変更、依存グラフに載っていない変更ファイル
  （拾い漏れを避けるため安全側に倒す）。`*.md` などレンダリングに無関係な
  グラフ外ファイルは無視する
- **`--commit`**: ビルド記録と差分終点の両方に使う。絞り込み時は worktree が
  この SHA と一致していること（`HEAD` 一致・追跡ファイル clean）が前提条件で、
  満たさなければ全撮影へ倒さずエラー（終了コード 2）で落ちる。stats / index は
  worktree から読むため、worktree が別コミットだと選別が別内容に対する計画になる

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
  モードで渡すと 400（サーバーがレンダリングしないため、ストーリー ID を
  スクリーンショット名へ写像できない。部分アップロードは名前ベースの
  `captured_names` を使う）。どのストーリーを渡すべきかは `vrt upload
  --only-changed` が自動で決める
- 流用の起点になる baseline はビルド**作成時**に固定される。
  `{"expected_baseline_commit_sha": "<sha>"}` を finalize に添えると、
  計画の起点との一致をサーバーが検証する（不一致は 400）
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
