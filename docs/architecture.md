# アーキテクチャ

`README.md` が「何ができるか」なら、この文書は「どう組まれているか」。

## 全体像

```
ブラウザ ──▶ frontend (TanStack Start SSR, :3000)
                 │  /api/* を丸ごと転送（src/routes/api.$.ts）
                 ▼
             backend (axum, :3400) ──▶ Postgres
                 │                └──▶ Valkey（セッション / OAuth state）
                 │
                 ├─ apalis ワーカー（同一プロセス、Postgres がキュー）
                 │    compare_build / github_status / github_webhook
                 └─ ストレージ（local ディレクトリ or S3 互換）

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
docker-compose.yml  db / redis / migration / backend / frontend
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
        │                │    job    │  apalis ワーカー（比較・PR ステータス・webhook）
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
                  finalize
   pending ──────────────────▶ processing
                                  │
              ┌───────────────────┼───────────────────┐
              │                   │                   │
              ▼                   ▼                   ▼
           passed          changes_detected         failed
              │                   │                (終端)
              │ 明示承認          ├── approve ──▶ approved (終端)
              └───────────────────┤
                                  └── reject  ──▶ rejected (終端)
```

- `pending` … ビルド行を作った直後。スクリーンショットを受け付けている。
- `processing` … `finalize` 後、`compare_build` ジョブが走っている。
- `passed` … 差分ゼロ。レビュー不要。ただし baseline 昇格のために明示承認はできる。
- `changes_detected` … 差分あり。人間のレビュー待ち。
- `failed` … 比較そのものが失敗（画像が壊れている等）。
- `approved` … 承認済み。**このビルドの全スクリーンショットが
  `(project, branch)` の新しい baseline になる。**
- `rejected` … 却下。baseline は更新されず、未レビューの比較は `rejected` になる。

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

`finalize` はビルドを `processing` にして `compare_build` ジョブを積むだけで、
HTTP リクエストはそこで完了する。実際の比較は apalis ワーカーが行う:

1. `(project, branch)` の最新 baseline を引く（無ければ全件 `added`）
2. baseline とビルドのスクリーンショットを名前で突き合わせる
3. 両方にある組はデコードして pixelmatch で差分ピクセル数を数え、
   プロジェクトの `diff_threshold` / `diff_ratio_fail` と突き合わせる
4. 差分画像を保存し、`comparisons` 行を書く
5. 集計してビルドを `passed` か `changes_detected`（失敗時は `failed`）へ遷移
6. GitHub App が設定されていれば `github_status` ジョブで PR にステータスを返す

キューは Postgres（apalis-postgres）。Redis を落としてもジョブは消えない。

## ストレージ

`STORAGE_BACKEND` で切り替える trait 実装（`service::storage`）:

- `local` … `LOCAL_UPLOAD_DIR` 配下に置き、backend 自身が配信する
- `s3` … S3 互換にアップロードし、`S3_PUBLIC_BASE_URL` の URL を返す

アップロードは 1 枚 25MB まで。frontend の `/api` プロキシも同じ上限を持つ
（超過は転送せず 413）。
