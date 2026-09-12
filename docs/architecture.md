# アーキテクチャ

`README.md` が「何ができるか」なら、この文書は「どう組まれているか」。

## 全体像

```
ブラウザ ──▶ frontend (TanStack Start SSR, :3000)
                 │  /api/* を丸ごと転送（src/routes/api.$.ts）
                 ▼
             backend (axum, :3400) ──▶ Postgres
                 │                └──▶ Valkey（セッション / OAuth state）
                 ├─ apalis ワーカー（既定。JOB_WORKERS_ENABLED=false で vrt-worker へ委ねる）
                 └─ ストレージ（local ディレクトリ or S3 互換）

             vrt-worker ──▶ Postgres の compare_build / github_status / github_webhook キュー
                 ├─ Valkey（インストールトークンのキャッシュ）
                 └─ backend と同じストレージ

             vrt-runner ──▶ Postgres の render_build キュー
                 ├─ ヘッドレス Chromium（CHROMIUM_PATH）
                 └─ backend と同じストレージ

CI ──────────▶ backend /v1/ci/*（PAT の Bearer 認証。ブラウザを経由しない）
```

ブラウザから見える origin は frontend ひとつだけ。セッション Cookie が
first-party のまま済むので、CORS も `SameSite=None` も要らない。CI だけは
backend を直接叩く（PAT はブラウザの資格情報ではないので CSRF の対象外）。

## モノレポ構成

```
apps/backend      Rust ワークスペース（下記のクレート群 + migration）
apps/frontend     TanStack Start + React 19 + Tailwind
e2e               Playwright（独立した pnpm ルート）
docs              この文書と docs/github-app.md
docker-compose.yml  db / redis / migration / backend / runner / frontend
```

## backend のクレート依存グラフ

各クレートは「1 つ下の層しか知らない」ように切ってある。矢印は依存の向き。

```
              ┌──────────┐
              │  entity  │  SeaORM モデル + ドメイン列挙（状態機械はここ）
              └────▲─────┘
                   │
              ┌────┴─────┐
              │  common  │  設定 / エラー型 / DB・Redis 接続 / バリデーション
              └────▲─────┘
                   │
        ┌──────────┴───────────┐
        │                      │
   ┌────┴─────┐          ┌─────┴─────┐
   │ payload  │          │  service  │  ビジネスロジック（DB トランザクション）
   │ (DTO)    │◀─────────┤           │
   └────▲─────┘          └─────▲─────┘
        │                      │
        │                ┌─────┴─────┐
        │                │    job    │  apalis ワーカー（レンダリング・比較・PR ステータス・webhook）
        │                └─────▲─────┘
        │                      │
   ┌────┴──────────────────────┴─────┐
   │            handler              │  axum ルータ / ハンドラ / ミドルウェア / OpenAPI
   └─────────────────▲───────────────┘
                     │
              ┌──────┴──────┐
              │   backend   │  bin: サーバー起動・ワーカー spawn・OpenAPI export
              └─────────────┘
```

実際の `path` 依存:

| クレート  | 依存                              |
| --------- | --------------------------------- |
| `entity`  | （なし）                          |
| `common`  | `entity`                          |
| `payload` | `entity`, `common`                |
| `service` | `entity`, `common`, `payload`     |
| `job`     | `entity`, `common`, `service`     |
| `handler` | 上記すべて                        |
| `backend` | `handler`（+ 起動に必要なもの）   |

守っている取り決め:

- **`handler` は DB を直接触らない。** 認可の判定に必要な行の取得も
  `service::tenants::require_role` 等を経由する。
- **`job` は `handler` を知らない。** ジョブの投入は handler 側で
  `job::*::enqueue_best_effort` を呼ぶ形にして、依存を一方向に保つ。
- **`entity` に I/O は無い。** 状態機械（`can_transition_to` / `needs_review`）は
  純粋関数で、単体テストが DB 無しで回る。

## OpenAPI パイプライン

契約は Rust 側が単一の出所。

```
utoipa の #[utoipa::path] 注釈
        │  cargo run --bin export_openapi
        ▼
apps/frontend/openapi.json   ← コミットされる唯一の契約ファイル
        │  openapi-typescript
        ▼
apps/frontend/src/generated/api.d.ts   ← gitignore（ビルド時に生成）
        │  openapi-fetch + openapi-react-query
        ▼
$api.useQuery("get", "/v1/...")        ← パスもレスポンス型も型検査される
```

- 開発時は `pnpm openapi`（export + generate）を一発で回す。
- CI の `openapi-drift-check.yml` が backend から export し直して
  `openapi.json` との差分で落とす。バックエンドを変えて再生成を忘れると赤くなる。
- frontend のビルド系ワークフローと Dockerfile は cargo を持たないので、
  **コミット済みの `openapi.json` から型だけ生成**する。

## 認証

ログイン手段は OAuth のみ（GitHub / GitLab / セルフホスト GitLab）。
PKCE・state・トークン交換・ユーザー情報取得は外部クレート
[`auth-core`](https://github.com/koyori-app/auth-core) が担い、
このリポジトリは HTTP ハンドラとセッション発行だけを組み立てる。

```
GET /v1/auth/{provider}/login
  ├─ auth-core: PKCE ペア + state を生成
  ├─ state を Valkey に保存（TTL 付き）＆ セッションにも控える
  └─ 307 → プロバイダーの認可 URL

GET /v1/auth/{provider}/callback
  ├─ Valkey から state を GETDEL（使い捨て）
  ├─ セッションに控えた state と突き合わせ（セッション固定対策）
  ├─ auth-core: code → アクセストークン → ユーザー情報
  ├─ users / oauth_connections を upsert（アクセストークンは AES-256-GCM で暗号化）
  ├─ session.renew()（セッション ID を張り替え）
  └─ 307 → フロントエンド
```

資格情報は 2 系統:

| 経路               | 主体            | CSRF                            | スコープ           |
| ------------------ | --------------- | ------------------------------- | ------------------ |
| セッション Cookie  | ブラウザの人間  | Origin 検査ミドルウェアが担当   | 全権（ロール検査） |
| PAT (`Bearer`)     | CI              | 対象外（ヘッダは偽装できない）  | `read:project` / `read:build` / `write:build` |

テナント内のロールは `member < admin < owner`。非メンバーには
「存在しない」と「権限がない」を区別させないため、一律 403 を返す。

> **テスト専用ログイン**: `TEST_LOGIN_ENABLED=true` のときだけ
> `POST /v1/auth/test-login` が開く（e2e 用）。既定では 404 で、release
> ビルドではこのフラグが立っていると起動そのものを拒否する。
> 本番では絶対に有効にしないこと。

## VRT の状態機械

### ビルド

```
   pending ── finalize ──▶ queued ── worker (screenshots) ──▶ processing
                            │                                   ▲
                            └── worker (storybook) ──▶ rendering┘
                                                         │ 撮影失敗
                                                         ▼
              ┌───────────────────┼───────────────────┐
              │                   │                   │
              ▼                   ▼                   ▼
           passed          changes_detected         failed
              │                   │                (終端)
              │ 明示承認          ├── approve ──▶ approved (終端)
              └───────────────────┤
                                  └── reject  ──▶ rejected (終端)

   failed ── retry ──▶ queued
   passed | changes_detected ── recompare ──▶ queued
```

- `pending` … ビルド行を作った直後。スクリーンショット（`screenshots` モード）
  か Storybook バンドル（`storybook` モード）を受け付けている。
- `queued` … finalize または再実行でパイプライン先頭のジョブを投入済み。
  worker が取得すると storybook は `rendering`、screenshots は `processing` に進む。
- `rendering` … `render_build` worker がジョブを取得後、
  ヘッドレス Chromium でストーリーを撮っている。撮り終えると `processing` に
  自動で繋がる。`screenshots` モードでは通らない。
- `processing` … `compare_build` ジョブが走っている。
- `passed` … 差分ゼロ。レビュー不要。ただし baseline 昇格のために明示承認はできる。
- `changes_detected` … 差分あり。人間のレビュー待ち。
- `failed` … 比較そのものが失敗（画像が壊れている等）。終端だが唯一の例外として
  再実行できる（`POST /v1/builds/{build_id}/retry`、admin 以上）。storybook
  モードはアップロード済みバンドルの再レンダリングから、`screenshots` モードは
  比較からやり直す。どちらもまず `queued` に入り、`error_message` /
  `completed_at` / 差分カウントはクリアされ、途中結果（screenshots /
  comparisons）は各ジョブが開始時に捨てる。
- `approved` … 承認済み。**このビルドの全スクリーンショットが
  `(project, branch)` の新しい baseline になる。**
- `rejected` … 却下。baseline は更新されず、未レビューの比較は `rejected` になる。

比較が済んだビルド（`passed` / `changes_detected`）は、撮影し直さずに現行
baseline と比べ直せる（`POST /v1/builds/{build_id}/recompare`、admin 以上）。
レビューを待っている間に別のビルドが承認されて baseline が動くと、そのビルドの
承認は「baseline moved」で止まる。スクリーンショットは残っているので、比較だけ
やり直せば救える——という経路である。

- 比較結果は作り直されるため、**そのビルドに記録済みのレビューは失われる**
  （baseline が動いた時点でその判断は無効になっている）
- baseline が動いていないビルドは 409。やり直しても結果は同じで、レビューだけが
  消えるため
- 部分撮影のビルドは 409。撮らなかった story が旧 baseline の PNG の複製で
  埋まっており、別の baseline と比べると撮ってもいない story に差分が出る。
  そのまま承認すれば旧 baseline の絵が新しい baseline へ焼き付く
- `approved` / `rejected` は 409。確定したレビュー結果は書き換えない

再比較の間、GitHub の commit status は `pending` に戻り、比較結果が出た時点で
書き直される（同じコミットに後続のビルドがある場合は、そちらが優先されて更新
されない）。必須チェックにしている場合、古いビルドの再比較で成功していた
チェックが失敗に転じうる。

遷移は `service::builds::transition` に一本化されていて、表に無い遷移は
すべて 409。実装は `entity::builds::BuildStatus::can_transition_to`。

承認はプロジェクト行を `SELECT ... FOR UPDATE` で直列化してから baseline を
作るので、同一プロジェクトの並行承認でも baseline が競合しない。

### 比較（スクリーンショット 1 枚ごと）

比較結果の状態（`compare_build` ジョブが算出する事実）:

```
pending ──▶ processing ──┬──▶ unchanged   baseline と一致（しきい値以内）
                         ├──▶ changed     差分あり
                         ├──▶ added       baseline に無い新規
                         ├──▶ removed     baseline にあるが今回無い
                         └──▶ failed      比較自体が失敗
```

レビュー状態はこれと直交する別の軸（人間の判断）:

```
pending ──┬──▶ approved
          └──▶ rejected
```

`unchanged` は差分が無いので自動的にレビュー済み扱い
（`ComparisonStatus::needs_review` が `false`）。残り 4 つは人間待ちで、
未レビューが残ったままビルドを承認しようとすると 409 になる。
まとめて通したいときだけ `approve` に `force: true` を渡す。

### 初回ビルド

baseline がまだ無いプロジェクトでは、全スクリーンショットが `added` になり、
ビルドは `changes_detected` に入る。そこで承認すると初回 baseline ができ、
次のビルドからが本来の比較になる。

## 比較ジョブ

`finalize` はビルドを `queued` にして `compare_build` ジョブを積むだけで、
HTTP リクエストはそこで完了する。worker が取得した時点で `processing` に進み、
実際の比較を行う:

1. `(project, branch)` の最新 baseline を引く（無ければ全件 `added`）
2. baseline とビルドのスクリーンショットを名前で突き合わせる
3. 両方にある組はデコードして pixelmatch で差分ピクセル数を数え、
   プロジェクトの `diff_threshold` / `diff_ratio_fail` と突き合わせる
4. 差分画像を保存し、`comparisons` 行を書く
5. 集計してビルドを `passed` か `changes_detected`（失敗時は `failed`）へ遷移
6. GitHub App が設定されていれば `github_status` ジョブで PR にステータスを返す

キューは Postgres（apalis-postgres）。Redis を落としてもジョブは消えない。

## レンダリングジョブ（storybook モード）

`mode = storybook` のビルドは、CI が撮った PNG ではなく **ビルド済み Storybook
の zip** を受け取り、Render worker で撮る（Chromatic 方式）。`finalize` はビルドを
`queued` にして `render_build` ジョブを積むだけで、worker が取得した時点で
`rendering` に進み実処理を行う:

1. `builds.storybook_key` の zip をストレージから読み、一時ディレクトリに展開する。
   展開は `service::render::bundle` が担当し、**zip-slip（`..` / 絶対パス /
   `\` 区切り）・シンボリックリンク・エントリ数（20,000）・展開後サイズ（500MB）**
   を検査して弾く。`index.json` はアーカイブ直下と 1 階層下まで探す
2. `index.json`（v4/v5 の `entries`、6 系の `stories` も可）からストーリー一覧を作る。
   `type: "docs"` のエントリは撮らない
3. 展開先を `127.0.0.1:0`（OS 任せの空きポート）でループバック配信する
4. `CHROMIUM_PATH` の Chromium を 1 プロセス起動し、独立した page で
   ストーリーを**最大 2 件ずつ並列**に
   `iframe.html?id=<storyId>&viewMode=story` で開く。`#storybook-root`（6 系は
   `#root`）に中身が入るまでポーリングし、settle 待ち・`document.fonts.ready` の
   条件待ち（到達可能な同一オリジン iframe の中まで再帰する）・静止・
   撮影直前の再確認（検証と撮影の間に document が入れ替わっていないか——
   story が自分を reload / `document.open()` する場合は検証を描画完了待ちから
   やり直す）のあと
   ビューポートを PNG で撮る。読み込みに**失敗**したフォントは story を
   失敗にせず、代替字形のまま撮って警告をビルドログ（warn 行）に残す——「同じビルドから
   同じ絵」の保証が成り立つのは**同じビルド、かつ外部依存（外部 CDN の
   フォント等）の応答が同じ場合**である。
   ビューポートはプロジェクトの `viewport_width` / `viewport_height`（既定 1280x720）
5. `{title}/{name}` の名前で `screenshots` 行に保存する
   （`metadata` に `story_id` / `title` を残す）
6. `rendering → processing` に遷移し、`compare_build` ジョブを積んで
   既存の比較経路に合流する

ジョブ開始時にそのビルドの `screenshots` 行を全削除するので、途中で落ちて
リトライされても `(build_id, name)` の UNIQUE にぶつからない。
1 ストーリーでも撮れなければビルドは `failed` になり、`error_message` に
どのストーリーで落ちたかが入る（Chromium が無い / 起動できない場合も同じ経路で
`failed` になり、ワーカーは死なない）。ブラウザは 1 ジョブ = 1 インスタンス、
ワーカーの同時実行数は 1、ブラウザ内の story page 同時実行数は 2 に固定してある。

### 独立ワーカー

`vrt-worker` は `compare_build` / `github_status` / `github_webhook` を消費する独立
バイナリで、API に `JOB_WORKERS_ENABLED=false` を設定すると API 内のワーカーは起動しない。
分けている理由は復帰のさせ方にある。apalis のワーカーはハートビートに失敗した時点で走行
ループを抜けるため、止まったらプロセスを非ゼロ終了させて restart policy に戻してもらう。
API に同居させるとこの再起動が HTTP まで巻き添えにする。

ワーカーが読む設定は API より狭く、`DATABASE_URL`・`REDIS_URL`・`APP_URL`・ストレージ設定・
GitHub App の資格情報だけで、PAT の署名鍵や OAuth の資格情報は渡さない。
停止検知の設計は [worker-supervision.md](worker-supervision.md) に、キューごとの状態は
`GET /v1/health/queues`（認証不要・集計値のみ）で読める。

切り替えは worker サービスを起動して登録を確認してから API を `false` にする。
順序を逆にすると、その間だけ誰もキューを消費しない。

ローリング更新では **worker を API より先に入れ替える**。再比較のジョブは
「storybook ビルドを `queued` から比較へ進めてよい」という印を持つが、この印を
知らない旧 worker はそのジョブを対象外として消費してしまい、ビルドが `queued` の
まま止まる。止まった場合は同じ再比較エンドポイントで回収できる
（`queued` に入れてから 1 分後以降、ジョブだけを積み直す）。

### 独立 runner

`vrt-runner` は `render_build` キューだけを消費する独立バイナリで、API と同じ
Postgres と成果物ストレージへ接続する。API に `RENDER_WORKER_ENABLED=false` を設定すると
API 内の Render worker は起動しない。Chromium を持たない API イメージでもStorybook
ビルドを受け付ける場合は、併せて `STORYBOOK_RENDER_ENABLED=true` を設定する。

runner に必要な環境変数は `DATABASE_URL`、`CHROMIUM_PATH`、`STORAGE_BACKEND` と
選択したストレージの設定だけで、Redis・OAuth・GitHub資格情報は渡さない。Postgres
キューのclaimとheartbeatはapalisが担当し、runnerが途中で停止したジョブは孤児回収後に
再投入される。runnerは1プロセスあたり1ビルドを処理するため、Dokployでは同じイメージを
Start Command `/app/vrt-runner` で起動し、サービスのレプリカ数でビルド並列数を調整する。
runnerを別ホストへ置く場合、localディレクトリは共有できないためS3互換ストレージを使う。
デプロイ停止時は runner が新しい job の取得を止め、進行中の build の完了を待つ。同梱の
Compose は `stop_grace_period: 1h` を設定しているため、Dokploy 側でも停止猶予を同等以上に
する。猶予を超えて強制終了された job は apalis の孤児回収後に同じ task として再実行される。

## ストレージ

`STORAGE_BACKEND` で切り替える trait 実装（`service::storage`）:

- `local` … `LOCAL_UPLOAD_DIR` 配下に置き、backend 自身が配信する
- `s3` … S3互換へ保存し、private objectをbackend経由でプロキシ配信する

production Composeでlocalからs3へ切り替えると、`vrt-storage-migrate` one-shotが
`LOCAL_UPLOAD_DIR`を再帰走査し、相対パスをそのままS3 keyとしてコピーする。同じkey・sizeは
skipし、upload後のsizeも再取得して検証する。元ファイルは削除せず、移行が失敗した場合は
backend/runnerの起動を止めるため、設定を直した再デプロイで安全に再開できる。

スクリーンショットのアップロードは 1 枚 25MB まで。frontend の `/api` プロキシも
同じ上限を持つ（超過は転送せず 413）。

Storybook バンドル（`POST /v1/ci/builds/{id}/storybook`）だけは桁が違うので、
ルーターのボディ上限を分けてある（zip 1 本 200MB まで。展開後 500MB /
20,000 エントリで打ち切り）。CI が直接 backend を叩く経路なので、frontend の
プロキシは通らない。バンドルは
`tenants/{tenant}/projects/{project}/builds/{build}/storybook.zip` に置かれる。
