# GitHub App 連携（PR コミットステータス・PR コメント）

VRT は GitHub App として PR のコミットステータス（`context: vrt`）を書き込み、
PR に紐付くビルドにはレビュー UI へのリンクをコメントとして掲示する。
未設定でも VRT のコアフローはそのまま動く（連携部分だけが無効になる）。

## 1. GitHub App を作る

GitHub の **Settings → Developer settings → GitHub Apps → New GitHub App**。

| 項目 | 値 |
|---|---|
| Homepage URL | `https://<APP_URL>` |
| Webhook URL | `https://<APP_URL>/api/v1/github/webhook` |
| Setup URL | `https://<APP_URL>/github/setup` |
| Webhook secret | 任意のランダム文字列（`GITHUB_WEBHOOK_SECRET` に入れる） |
| Repository permissions → **Commit statuses** | **Read and write** |
| Repository permissions → **Pull requests** | **Read and write**（PR コメントに必要） |
| Repository permissions → Metadata | Read-only（自動で付く） |
| Subscribe to events | **Installation target** / **Installation**（`installation` イベント） |
| Where can this GitHub App be installed? | 運用に合わせて |

作成後の画面で:

1. **App ID** を控える → `GITHUB_APP_ID`
2. App の公開ページにあるインストール URL を控える → `GITHUB_APP_INSTALL_URL`
3. **Generate a private key** で `.pem` をダウンロード → `GITHUB_APP_PRIVATE_KEY_PEM`

## 2. バックエンドの環境変数

| 変数 | 必須 | 説明 |
|---|---|---|
| `GITHUB_APP_ID` | ○ | App ID（数値） |
| `GITHUB_APP_PRIVATE_KEY_PEM` | ○ | 秘密鍵の PEM。環境変数に入れるときは改行を `\n` にエスケープしてよい（起動時に実改行へ戻す） |
| `GITHUB_APP_INSTALL_URL` | UI 導線に必須 | `https://github.com/apps/<app-slug>/installations/new`。プロジェクト設定のインストールボタンに使う |
| `GITHUB_WEBHOOK_SECRET` | ○ | Webhook secret。未設定だと `POST /v1/github/webhook` は 400 を返す |
| `GITHUB_API_BASE_URL` | – | 既定 `https://api.github.com`。GitHub Enterprise のときだけ設定する |

`GITHUB_APP_ID` と `GITHUB_APP_PRIVATE_KEY_PEM` の**両方**が揃って初めて連携が有効になる。
片方だけの場合は「未設定」と同じ扱い（ジョブは警告ログを出して何もしない）。

## 3. インストールとテナントへの紐付け

1. プロジェクトの **Settings → GitHub → Install GitHub App** を押す
2. GitHub で対象の Organization / ユーザーとアクセス対象 repository を選ぶ
3. Setup URL から元のプロジェクト設定へ戻り、installation がテナントへ自動 claim される
4. GitHub が `installation.created` を webhook で配信 → `github_installations` に
   **未 claim（`tenant_id = NULL`）** の行ができる
5. API を直接使う場合は `GET /v1/github/installations/unclaimed` で候補を探し、
   `POST /v1/github/installations/{installation_id}/claim`（body: `{"tenant_id": "..."}`）で
   自分のテナントに紐付ける（テナントの **admin 以上**が必要）
6. UI で Organization / アカウントを選ぶと GitHub API から repository 一覧が読み込まれる。
   検索して選択すると `PATCH /v1/projects/{project_id}/github`
   （body: `{"installation_id": 123, "github_repo": "owner/name"}`）でプロジェクトと
   リポジトリを結び付ける。解除は両方を `null` にする

> **注意（MVP の割り切り）**
> `GET /v1/github/installations/unclaimed` はログイン済みユーザーなら誰でも呼べる。
> つまり「まだどのテナントにも紐付いていない installation のアカウント名」が
> 全ログインユーザーから見える。claim 自体は先着 1 テナントで、以降は 409 になる。
> 将来は setup_url + 短命トークンで「インストールした本人だけが見える」形に絞る。

## 4. ステータスの対応表

`context` は常に `vrt`、`target_url` はレビュー UI のビルド詳細
（`{APP_URL}/t/{tenant_slug}/p/{project_slug}/builds/{number}`）を指す。

| ビルドの状態 | commit status | description |
|---|---|---|
| `processing`（finalize 直後） | `pending` | Comparing screenshots against baseline |
| `passed` | `success` | Visual tests passed |
| `changes_detected` | `pending` | N changes detected, awaiting review |
| `approved` | `success` | Visual changes approved |
| `rejected` | `failure` | Visual changes rejected |
| `failed` | `error` | Visual test run failed |

差分検出（`changes_detected`）を `failure` にしないのは、差分そのものは失敗ではなく
**人間のレビュー待ち**だから。PR は「保留」のまま止まり、承認 / 却下で確定する。

### PR コメント

ビルドが PR に紐付いている（CLI が `pull_request_number` を送っている）場合は、
ステータスと同じ内容 + ビルド詳細へのリンクを PR のコメントとしても掲示する。
コメントは状態遷移ごとに積まず、不可視マーカー（`<!-- vrt:{project_id} -->`）で
自分のコメントを見つけて**上書き更新**する（1 PR × 1 プロジェクトにつき 1 件）。

> **既存インストールへの注意**
> Pull requests 権限を後から App に追加した場合、既存の installation では
> オーナーが新しい権限を承認するまでコメントは 403 で失敗する
> （警告ログのみ。コミットステータスはそのまま動く）。

## 5. アンインストール・サスペンド

- `installation.deleted` → 行を論理削除（`deleted_at`）し、claim を外し、
  そのインストールに紐付いていた**全プロジェクトの連携を解除**する
- `installation.suspend` / `unsuspend` → `suspended_at` を出し入れする

## 6. 動作確認

```bash
# webhook の疎通（署名が合わないと 401）
BODY='{"action":"created","installation":{"id":1,"account":{"login":"acme","type":"Organization"}}}'
SIG="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$GITHUB_WEBHOOK_SECRET" -r | cut -d' ' -f1)"
curl -i -X POST "$APP_URL/api/v1/github/webhook" \
  -H "X-GitHub-Event: installation" \
  -H "X-Hub-Signature-256: $SIG" \
  -H 'Content-Type: application/json' \
  -d "$BODY"    # → 202 Accepted
```

そのあと CI からビルドを 1 本流し、PR の Checks に `vrt` が出れば成立している。
統合テスト（`cargo test --test github_integration`）は同じ経路を wiremock 相手に通す。
