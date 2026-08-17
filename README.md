# VRT

Visual regression testing as a service. CI が撮ったスクリーンショットを受け取り、
承認済みの baseline と 1 枚ずつ突き合わせ、差分を人間がレビューして承認すると
その結果が次の baseline になる — という一周を回すための SaaS。

- **マルチテナント**: テナント（組織）> プロジェクト > ビルド > スクリーンショット
- **CI からは PAT ひとつ**: ビルド作成 → PNG アップロード → finalize → ポーリング
- **レビュー UI**: 差分のサイドバイサイド / オーバーレイ表示、キーボード操作
- **GitHub App 連携**: PR にレビュー結果を commit status とビルドリンクのコメントで返す（任意）

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

#### 配布ソース（`.git` 無し）での検証

`vrt-cli` のテストは、`.git` の無い配布ソース（`git archive` の展開結果）でも
自己完結で通ることを保証する。CI は git checkout（`.git` あり）で走るため、
この性質は CI の緑だけでは確認できない。確認は archive 展開下での実測で行う。

```bash
tmp=$(mktemp -d)
git archive HEAD | tar -x -C "$tmp"
cd "$tmp/apps/backend"
cargo test -p vrt-cli
```

テストを追加するときは、開発リポジトリの HEAD や `.git` のメタデータに
依存させず、`vrt_cli::test_support::init_test_repo` の一時リポジトリで
自己完結させること。

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

# build は vrt が実行する。前後の HEAD を vrt 自身が観測して成果物を
# 生成元コミットへ束縛する（stamp 単独の後追い実行は無い）
vrt stamp --dir ./storybook-static -- pnpm build-storybook --stats-json
vrt plan --dir ./storybook-static --output plan.json

# build_id はキーがあるときだけ入る（--baseline-commit 経路では省略される）。
# 撮影後のアップロード先・finalize 先のビルド ID として後段で使う
# （API を直接叩く場合の例は下の capture plan 節の $BUILD）。
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

サーバー解決経路で `plan = "only"` になった場合、`vrt plan` は計画 JSON を
出力する前に、その選択集合と現行 index の全 story ID をビルドへ固定する
（`POST /v1/ci/builds/{build_id}/plan`）。以降のアップロードと finalize は
この保存済み計画と突き合わせて検証される（詳細は次節）。固定に失敗した場合は
計画を出力せず終了コード 2 で落ちる——束縛の無い部分撮影を始めさせないためである。
なお部分アップロードのスクリーンショット名には計画の story ID をそのまま使うこと。

絞り込み（`plan = "only"` の算出）には前提条件がある。stats / index は
worktree のファイルから読むため、worktree の内容が計画の終点コミットと
一致していなければ「別内容に対する計画」ができてしまう。次を満たさない場合、
`vrt plan` は全撮影へ倒さず終了コード 2 で落ちる。

- worktree の `HEAD` が `--commit`（省略時は `HEAD` 解決値）と一致していること
- 追跡ファイルに未コミットの変更が無いこと（未追跡ファイルは対象外）

##### 成果物を生成元コミットへ束縛する（`vrt stamp -- <build command>`）

上の worktree 検査は**追跡ファイルしか見ない**。ところが絞り込みの実入力である
`preview-stats.json` / `index.json` は通常 untracked なので、worktree 検査だけでは
「別コミットでビルドされた成果物」（古い CI キャッシュ、rebase 前のビルドなど）を
掴んだまま絞り込んでしまう。その場合、変更の影響を受けた story が選別から漏れて
偽 PASS になりうる。

そこで storybook build は `vrt stamp` に**実行させる**。

```bash
vrt stamp --dir ./storybook-static -- pnpm build-storybook --stats-json
```

`--` の後は argv としてそのまま起動される（シェル展開はしない。シェル機能が
要るなら `sh -c '...'` を渡す）。vrt は次の順で動く。

1. build 開始前に HEAD を解決し、worktree が clean（追跡ファイルに未コミット
   変更なし）であることを検査する
2. **旧成果物を無効化する**——`<dir>/vrt-provenance.json` と stats / index を
   削除する。「コマンドが成功した」ことと「成果物がその build で生成された」
   ことは別であり（何も生成しない命令でも成功はする）、build 前の成果物が
   build を経ずに生き残る経路をここで断つ。**`--stats-json` / `--index-json` で
   指定したパスはその解決値のまま build 前に削除される**——タイポで無関係な
   ファイルを指すと、そのファイルが消える。カスタムパスを渡すときは
   指定先をよく確かめること
3. build コマンドを実行する。失敗したら何も stamp しない。旧 provenance は
   手順 2 で既に消えているため、**失敗した stamp が古い証明を残すこともない**
4. build 成功後、HEAD が動いていないこと・worktree が依然 clean であることを
   再検査する。build を跨いで HEAD が動いた・追跡ファイルが汚れたままの
   checkout はここで検出され、stamp は行われない（観測は build の前後 2 点
   だけなので、途中で往復して元に戻る操作までは検出しない——下の
   「保証しないこと」参照）
5. stats / index が存在することを確認し、その HEAD で
   `<dir>/vrt-provenance.json` を書く。両ファイルは手順 2 で削除済みなので、
   ここで存在する＝**build の実行中に再生成された**ことの証明になる。
   no-op な命令を渡すと stats 不在でここが失敗する

storybook には HEAD に束縛された信頼できる build-time marker が無いため、
「キャッシュ命中で成果物を書き直さない build」を証明付きで受け入れる形は
取らず、実入力 2 ファイルの再生成を build に強制する。実際の storybook build
は常に stats / index を書き出すので、この強制で失敗するのは生成しない命令だけ
である。なお stats / index を git 追跡下に置く構成では手順 2 の削除が worktree
を汚し手順 4 で拒否されるが、build 出力の追跡はそもそも本証明と両立しない。

記録されるのは次の 3 つである。

- build の前後で vrt が観測した worktree の HEAD commit OID
- `preview-stats.json` / `index.json`（`--stats-json` / `--index-json` で
  パスを変えている場合はその解決後ファイル）の SHA-256
- vrt が実行した build コマンド（argv）

build 後の後追い stamp（build コマンド無しの形）は提供しない。stamp と build の
間に checkout が挟まると「その commit でビルドした」証明にならないためで、
証明は生成時点の観測にしか作れない。

`vrt plan` / `vrt upload --only-changed` は絞り込みの前にこれを検証する。

- **provenance が無い** → 絞り込まず**全撮影へ倒す**（理由に stamp の導入手順を残す）。
  stamp 未導入の既存パイプラインはこの移行経路でそのまま動き続ける——絞り込みが
  効かなくなるだけで、全撮影は撮り逃しを作らないため安全側である
- **provenance が version 1（build 所有なしの旧形式）** → 絞り込まず
  **全撮影へ倒す**。v1 は「stamp 時点の HEAD」しか証明せず、build と stamp の
  間の checkout を検出できない。コミットとハッシュが一致して正しく見えても
  採用しない（**移行期の扱い**: 旧 CLI で stamp 済みの CI キャッシュはこの
  経路で無害化される。絞り込みを取り戻すには build を
  `vrt stamp -- <build command>` に載せ替えて成果物を作り直す）
- **provenance があるのにコミットが一致しない／stats・index の内容ハッシュが
  一致しない／壊れている／version 不明** → 全撮影へ倒さず**終了コード 2 で
  落ちる**。別コミットの成果物や stamp 後の差し替えは設定ミスの積極的な証拠で
  あり、黙って全撮影に読み替えると誤設定に永久に気づけない（worktree 不一致を
  エラーにするのと同じ方針）

いずれの場合も「生成元を検証できないまま絞り込む」ことはない。

##### provenance が保証すること・しないこと

保証するのは次の範囲である。

- stats / index が **vrt が実行して成功した build の実行中に生成された**こと
  （build 開始前に両ファイルと旧 provenance は削除されるため、成功後に存在する
  こと自体が再生成の証明になる。build 前から残っていた別ビルドの成果物に
  provenance が付くことはない）
- stats / index のバイト列が、その build の直後に成果物ディレクトリへ
  存在した内容と一致していること
- その build の開始前と成功後の両方で、worktree が同一 HEAD の clean な
  checkout だったこと
- build が失敗した stamp は何も stamp せず、**古い provenance も残さない**こと
  （失敗後の成果物ディレクトリは provenance 不在＝全撮影へ倒れる状態になる）
- 別コミットの成果物（キャッシュ復元・rebase 前のビルド）での絞り込みが
  エラーか全撮影へ倒れること

次は**保証しない**。運用側の緩和策とあわせて明記する。

- **build コマンドの意味論**: vrt が証明するのは「渡された argv が clean な
  HEAD で走って成功し、stats / index をその実行中に書いた」ことまで。argv が
  本物の storybook build であるかは検証しない——build 前の無効化により
  「何も生成しない命令」は失敗するようになったが、**命令自身が古い内容を
  書き戻す**（退避先からのコピー等）場合は、形式上有効な provenance ができて
  しまう。緩和として実行コマンドを provenance に記録し監査可能にしてある。
  build コマンドには実際に storybook build を行うものを渡すこと
- **build プロセス内部の忠実性**: storybook / webpack の incremental cache が
  古い内容の stats を出せば、正しい worktree でビルドしても stats の中身が
  古いことはありうる。絞り込みを使う CI job では storybook のキャッシュを
  復元しない（キャッシュ無しでビルドする）ことを推奨する
- **依存の状態**: `node_modules` の中身が lockfile と一致しているかは見ない。
  同じ job 内で frozen lockfile インストールを行うこと
- **untracked な build 入力**: clean 検査は追跡ファイルのみ。untracked の
  ローカルファイルが build に影響しても検出できない（CI の fresh checkout では
  実害になりにくいが、ローカル実行では注意）
- **build 中の一時的な checkout 往復**: worktree の観測は build の開始前と
  成功後の 2 点だけ。build の途中で別コミットへ checkout し、終了までに元の
  HEAD へ clean に戻す操作（A→B→A の往復）は 2 点観測では検出できない。
  build コマンドの中で checkout を行わないこと（CI の通常運用でこの形に
  なることはない——事故ではなく意図的な操作でしか作れない状態である）
- **改竄への防御**: provenance は無署名で、手で書けば偽装できる。脅威モデルは
  「事故（キャッシュ復元・rebase・手順ミス）」であり、悪意ある CI ランナーへの
  防御ではない

#### 部分アップロードは撮影前に計画を固定する（capture plan）

計画に従って一部の story だけを撮った場合、そのままアップロードして finalize
すると、撮らなかった名前の baseline エントリがすべて `removed` になってしまう。
サーバーは「アップロードされなかった」と「削除された」を区別できないからである。

そこで部分アップロードでは、**撮影を始める前に** 計画をビルドへ固定する。
`vrt plan` のサーバー解決経路（`build_id` が出る経路）は、`plan = "only"` の
計画を出力する前にこれを自動で行う。API を直接叩く場合は
`POST /v1/ci/builds/{build_id}/plan` を呼ぶ。

```bash
# vrt plan を使わず API を直接叩く場合のみ必要（vrt plan は自動で添付する）
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/plan" \
  -H "Authorization: Bearer $VRT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
        "selected_names": ["home-page", "pricing"],
        "manifest_names": ["home-page", "pricing", "about"],
        "baseline_commit_sha": "<ビルド作成レスポンスの baseline_commit_sha>"
      }'
```

- `selected_names` は今回撮ってアップロードする名前、`manifest_names` は
  現時点で存在する**全**名前（現行 story index の写し）。部分アップロードでは
  スクリーンショット名として計画の story ID をそのまま使うこと（サーバーは
  名前でしか突き合わせられない）
- 計画の起点 `baseline_commit_sha` が現在の baseline と一致しなければ 409。
  計画を作り直す（ずれたまま撮ると、比較対象が計画と別物になる）
- 計画は**スクリーンショットのアップロード前**にしか添付できない（409）。
  撮れた結果から後出しで計画を作る抜け道を断つためである
- **新規の名前（manifest にあり baseline に無いもの）は必ず `selected_names` に
  含める**（漏れると 400）。新規の名前には流用元が無いため、選択から漏れると
  アップロードも流用もされず、`added` として報告されないまま比較から消える
  ——選択ロジックのバグを黙って通さないための検査である。既存の名前の
  絞り込み（selected から外して流用に回す）は従来どおり自由
- スクリーンショット名の規則は全経路（計画・アップロード・finalize の
  `captured_names`、そして `storybook` モードのレンダリング——サーバーが
  `{title}/{name}` から生成する名前）で共通: 空でなく、前後に空白が無く、
  制御文字（NUL・エスケープ・改行等）を含まず、**NFC 正規化後**に **255 バイト**
  （文字数ではない。UTF-8 バイト長）以内。規則に合わない名前は計画の時点で
  400 になる。計画だけが緩いと「計画には載るのにアップロードできない名前」が
  でき、finalize は計画とアップロードの完全一致を要求するため、そのビルドは
  永久に finalize できなくなる。空白付きの名前は trim されず拒否される——
  サーバーが名前を書き換えると、計画に載せた名前と保存された名前がずれて
  突き合わせが成立しない。非 ASCII の名前は受理時に Unicode NFC へ正規化して
  保存・比較する——OS によって同じ見た目の名前が NFC / NFD で割れ（macOS の
  APFS は NFD を返す）、バイト一致の突き合わせが removed + added に化けるため
  である
- baseline が非空で `manifest_names` も非空なのに、baseline のエントリ名と
  `manifest_names` が 1 件も重ならない計画は 400 で拒否される。これは story の
  全削除ではなく命名規則の変更（例: PNG のパスから導出した `mobile/home` 形の
  名前で育てた baseline に、story ID の manifest を当てた）とみなされる。
  素通しすると baseline の全エントリが `removed` になり、比較自体は成功扱いで
  進むため命名のずれに気づけない。**既存 baseline を別の命名で育てていた場合は、
  計画を使わず一度全撮影で baseline を作り直してから部分アップロードへ移行する
  こと**（story を本当に全部削除した場合は `manifest_names` が空になるので、
  このガードには当たらない）

計画が固定されたビルドでは、サーバーは次のように振る舞う。

- アップロードは `selected_names` 内の名前だけ受理する（計画外は 400）
- finalize は **計画 == 実際にアップロードされた名前** を検証し、一致しなければ
  400 で拒否する。計画したのにアップロードが欠けた名前を黙って流用に回すと、
  撮影の失敗が「差分なし」に化けるためである。**撮影が全滅して 0 枚のまま
  finalize しても通らない**——「撮る集合」は finalize 時の自己申告ではなく
  保存済み計画から来るので、空の申告で偽 PASS を作ることはできない
  （finalize の `captured_names` は保存済み計画との任意のクロスチェックで、
  計画なしのビルドに渡すと 400）
- 計画外かつ `manifest_names` に残っている baseline エントリは `removed` に
  せず、前回 baseline の画像をこのビルドのスクリーンショットとして流用する
  （比較は `unchanged` になり、承認しても baseline から消えない）。
  `storybook` モードの `only_story_ids` と同じ帰結になる
- baseline にあって `manifest_names` に無い名前（= 削除された story）は
  流用せず `removed` として報告する。**部分アップロードでも削除は隠れない**
- `selected_names: []`（何も撮らない計画）は全エントリ流用として有効。
  撮影前に固定された「変更なし」の宣言なので、偽 PASS ではない

`expected_baseline_commit_sha` を finalize に添えると、計画添付時に固定された
baseline との一致を最後にもう一度検証できる（不一致は 400）。比較は固定値に
対して走るので、計画添付後に別ビルドが承認されて最新 baseline が動いても、
このビルドの比較はずれない。なお計画を固定しない全撮影ビルドの baseline は
従来どおり比較時点の最新が使われる（作成が古くても、他ブランチの fallback を
含めて最新と比較される）。

計画の固定（前節）まで成功した場合、計画は stdout にも出るので、`--output` を
使わずパイプで受けてもよい。固定に失敗したときは計画を出力せず、stdout が
空のまま終了コード 2 で落ちる（「`vrt plan` は環境の不備では計画を出さず……」の
節を参照）。ログは stderr へ出すため、stdout に出るのは計画の JSON だけである。

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

生成された名前にはスクリーンショット名の規則（空でなく、前後に空白が無く、
制御文字を含まず、NFC 正規化後に 255 バイト以内——「部分アップロードは撮影前に
計画を固定する」の節を参照）が
そのまま適用される。規則に合わない story が 1 件でもあるとレンダリング時に
ビルドが `failed` になり、`error_message` にどの story かが入る。

> **破壊的変更**: 従来はサーバーが保存時に名前の前後空白を黙って落としていた
> ため、`title` / story 名の先頭・末尾に空白があってもビルドは通っていた。
> 今回からそうした story を含む storybook ビルドは**失敗する**。直し方は、
> CSF の `title`（`export default { title }`）と story 名（named export の
> `storyName` / `name`）から前後の空白を取り除くこと。
>
> trim して受け付ける形をやめたのは、撮る story を絞る判定が trim **前**の
> 名前で baseline を引いていたためである。trim されて保存された baseline 名
> とは永久に一致せず、空白付きの story は絞り込みが効かず毎回撮り直しに
> なっていた。名前を黙って加工する経路としない経路の不整合は同種の突き合わせ
> ずれを生み続けるので、加工はせず入口で拒否する側に一本化した。

#### 撮影の決定化（キャレット・アニメーションの静止）

レンダラは撮影の直前に、ページを時刻に依存しない静止状態へ固定してから撮る。
利用者側の Storybook や preview に設定を足す必要はない。

- 入力キャレットは `caret-color: transparent` で不可視にする
  （明滅の位相で差分が出ることがなくなる）
- 有限の CSS アニメーション・transition・Web Animations は**終端**へシークして止める
- 無限アニメーション（スピナー等）は**先頭（進行度 0）**へ巻き戻して止める

いずれも「いつ撮ったか」に依存しない一意な座標へ固定するので、
同じ story は何度撮っても同じ PNG になる。
paused にするだけでは止まる位置がタイミング依存のままなので、そうはしていない。

静止用の CSS は document だけでなく **open shadow root のそれぞれ**にも
constructed stylesheet（`new CSSStyleSheet()` + `adoptedStyleSheets`）として
注入する。`caret-color` の継承値は shadow 内で明示された宣言に負け、
`transition-duration` はそもそも継承しないので、document への注入だけでは
shadow の中に届かないためである。同一オリジンの iframe（shadow 内に
置かれたものを含む）にも root 単位で再帰し、同じ注入とアニメーション静止を
行う。CSSOM 経由の注入は CSP `style-src` の管轄外なので、
`style-src 'self'` のような厳しい CSP を持つページでもそのまま効く
（CSP 側の設定変更は不要）。

**移行手順**: この決定化が入った版へ更新した直後の最初のビルドでは、
アニメーションやキャレットを含む story が `changes_detected` になりうる
（baseline は静止前の絵のままだから）。これは想定どおりの一度きりの差分で、
UI でレビューして承認すれば静止後の絵が新しい baseline になり、以降は安定する。

**この版から新しい失敗クラスが増える**: 従来は「揺れる絵のまま成功」して
いた story——静止しきれないアニメーション連鎖を持つもの、静止処理を時間内に
終えられないもの、静止 CSS の適用を検証できなかったもの——が、この版からは
**エラーとして表面化する**。緑だったビルドが更新後に失敗へ転じたら、
まずビルドログの `story failed ...` 行を見ること。失敗は story 単位で報告され、
**残りの story の撮影は最後まで続行される**（storybook の title / name から
生成したスクリーンショット名が名前規則に合わない場合も、同じく story 単位で
報告される）——エラーメッセージとログには
失敗した story が一度に列挙されるので、1 ビルドごとに 1 件ずつしか
発見できない形にはならない（ただし失敗が 1 件でもあればビルド自体は
`failed` になる。撮れなかった story を欠いたまま比較へ進めると、
差分ゼロに見える偽の成功を作るためである）。

なお、静止を story 単位で無効化する opt-out の口は意図的に設けていない。
静止できない story は「揺れる絵を黙って baseline に混ぜる」のではなく
エラーとして表面化させ、Storybook 側で動きを止めた状態の story を用意するのが
正しい対処である——opt-out はその場しのぎに使われた瞬間から、差分が出ても
不思議でない絵を緑として通し続ける偽 PASS の口になる。

**届かない範囲**: 次のものは静止できない。

- closed shadow root・クロスオリジン iframe の中
- canvas / `requestAnimationFrame` など JS が毎フレーム描き直すアニメーション
  （Worker からの `OffscreenCanvas` 描画・WebGL / WebGPU を含む——メインスレッドの
  外で進むものは rAF の差し替えにも掛からない）
- **ブラウザが Web Animations のタイムラインの外で進める時間変化**。静止処理は
  running なアニメーションを `getAnimations()` で数えてシーク・pause するが、
  次のものはそこに**載らない**（Chromium 実測で `document.getAnimations()` が
  返さないことを確認済み）ため、シークも pause もできず、**残っていても検知
  できない**——静止は成功と報告されるが、撮った絵はタイミング依存でありうる:
  - アニメーション画像（GIF / APNG / アニメーション WebP・AVIF）のフレーム送り
  - `<video>` / `<audio>` の再生（自動再生の映像フレーム、ネイティブコントロールの
    進行表示を含む）
  - SVG SMIL アニメーション（`<animate>` / `<animateTransform>` /
    `<animateMotion>`）
  - 進行中の smooth scroll（`scroll-behavior: smooth` や
    `scrollTo({ behavior: 'smooth' })` によるスクロール位置の遷移）
  - `<marquee>` のスクロール
  - UA シャドウ内の組み込みアニメーション（不確定状態の `<progress>` 等——
    closed shadow root と同じく外から列挙する口が無い）
- 読み込み・描画の進行に伴う**一度きり**の見た目変化: 遅延読み込み画像
  （`loading="lazy"`）・画像のデコード完了・`content-visibility: auto` の
  遅延描画。アニメーションではないので静止の対象にならず、描画完了シグナル
  後の settle 待ちが吸収する best-effort。**Web Font の適用（FOUT / FOIT）は
  ここから外れた**——撮影前に `document.fonts.ready` を条件待ちし、期限内に
  解決しなければその story は失敗になる（fail-closed）。フォント待ちは
  到達可能な**同一オリジン iframe の中まで再帰**する（静止処理と同じ範囲。
  open shadow root の中へも潜り、`<iframe>` と `<frame>` の両方を見る。
  クロスオリジン iframe の中は `contentDocument` が読めず観測できない——
  上記「届かない範囲」と同じ契約）。検証と撮影の間に
  document が入れ替わった（story が自分を reload した・`document.open()` で
  書き換えた）場合や新たな読み込みが始まった場合は、撮影直前の再確認が
  検知して検証を**描画完了待ちから**やり直す——reload 後の document では
  story がまだ描画されていない時点で `document.fonts.ready` が解決しうる
  （Blink の `ready` は「load イベント完了＋読み込み中フォント無し」で
  解決する）ため、フォント待ちだけをやり直すと未描画の絵を撮ってしまう。
  検知できないのは、再確認から撮影までの最後の一往復に始まる変化と、
  **同一 document 内の DOM 全面置換**（`body.replaceChildren` 等——document
  自体は入れ替わらないため、新しいフォント読み込みを伴う場合にしか
  検知に掛からない）である。素の XML document（feed.xml を読む同一オリジン
  iframe 等）は検証済みの印を刻む口（`dataset`）を持たないため、その
  document の入れ替わりも印では検知できない（フォント状態の検査は行う）。
  読み込みに
  **失敗**したフォント（404・接続断・`local()` 不在等）は story を失敗に
  **しない**——代替字形のまま撮り、警告（フォント名と、外部依存の応答が
  run ごとに変われば同一ビルドから異なる絵が生じうる旨と、対処: フォント
  ファイルをビルドへ同梱するか `@font-face` 参照を外す）を**ビルドログ
  （warn 行）に残す**。失敗判定にすると
  原因の `@font-face`（preview-head 等）が project 全体で共有されるため、
  そのフォントを一切表示しない story まで全滅するからである。このため
  「同じビルドから同じ絵」の保証が成り立つのは**同じビルド、かつ外部依存
  （外部 CDN のフォント等）の応答が同じ場合**である——断続的にしか届かない
  外部フォントは run ごとに違う絵を作りうる。恒久対処は警告の示すとおり
  フォントの同梱か参照の除去。**既知の限界（費用）**: 読み込みが error へ
  確定せず `ready` が解決しないフォント（無応答の CDN 等）は、story ごとに
  最大 `story_timeout`（既定 30 秒）を丸ごと消費してから失敗し、原因の
  `@font-face` が project 全体で共有されるため費用は story 数に比例して
  累積する。フォント待ちに独立の短い上限を持たせるのは未着手である
- 利用者側スタイルが要素セレクタ等で `!important` 明示した `caret-color` /
  `transition`（注入 CSS も `!important` だが `*` セレクタ＝specificity 0
  なので、`!important` 同士の比較では specificity の高い利用者側の宣言が勝つ。
  これは best-effort であり、個別要素の上書きまでは検出しない）
- 静止処理より後から JS で生成される shadow root や iframe

逆に、`@property` で登録したカスタムプロパティの animation / transition と、
同一 document の View Transitions（`::view-transition-*` 擬似要素の
`CSSAnimation`）は `getAnimations()` に**載る**（Chromium 実測）ので、
通常のアニメーションと同じく静止の対象になる。

そうした story は動きを止めた状態を Storybook 側で用意すること
（動画はポスターフレーム、GIF は静止画への差し替え、SMIL は
`<svg>` 側で `animate` を外した story を使う、等）。

**静止に失敗した story はエラーになる**: 静止処理は最終巡回後に running な
animation が残っていないことを検証する。残っていれば、そのストーリーの
スクリーンショットは撮らず、`freeze failed: N animation(s) still running
after M sweep(s): [...]` の形式でエラーを返す。これは既存の `storyErrored` /
`storyThrewException` と同じエラー経路（`RenderError::Story`）を通るので、
ビルドの結果には「この story は静止できなかった」という理由が表示される。
「なぜか差分が出続ける」よりも原因に辿り着きやすい。静止処理は安定状態まで
反復し（上限 10 巡）、有限の `animationend` 連鎖であれば収束して成功する。
収束の判定は running な animation が**ゼロになったか**で行う。ゼロになれば
即座に抜け、残っていれば上限まで反復を続ける。identity（animation 名と
対象要素の proxy）による早期停止は行わない——proxy では要素の同一性を表せず、
同一 keyframes が異なる要素へ順移動する連鎖（el1→el2→el3→el4）を
誤判定するためである。
無限に連鎖し続けるアニメーションは上限を超えて失敗になるので、Storybook 側で
止めた状態の story を用意すること。progress-based timeline
（`animation-timeline: scroll()` 等）の `endTime` は `CSSNumericValue`
（percent）であり、型を保って `currentTime` に渡すことで正しく静止する。
これは無限（`infinite`）の progress-based アニメーションでも同じで、
巻き戻し先の進行度 0 も `CSSUnitValue(0, "percent")` として型を保って渡す
（数値 0 の代入は `TypeError` になるため）。

**静止処理そのものにも時間上限がある**: 静止は `requestAnimationFrame` を
待って収束を確かめるため、rAF が発火しないページ（差し替え・停止した
レンダリングパイプライン等）では静止処理が終わらない。この場合も
ハングはしない。描画完了待ちと静止処理は **story ごとの時間予算
（`story_timeout`）を分け合う**——静止処理には予算の残り時間だけが渡され、
1 story の所要は最悪でもおよそ `story_timeout` に収まる。予算を使い切ると
`did not complete within ...: the freeze did not finish ...` の形式の
タイムアウトエラーとして返る（描画完了待ちの時間切れと同じ分類）。

**注入した CSS の適用も検証する**: 注入は constructed stylesheet で行うため
CSP に拒否されることはないが、「adopt の操作が成功した」ことと「CSS が
実際にカスケードへ入った」ことは別である。レンダラは `getComputedStyle` で
静止 CSS 専用のプローブ（`--vrt-frozen`）が root ごとに効いていることを
実測し、効いていなければ fail-closed で失敗を返す。プローブは既定値を
持たないので、既定値との偶然の一致で検証が素通りすることもない。
検証の対象はあくまで「注入 sheet が root に効いているか」であり、利用者側の
`!important` による個別要素の上書き（上記「届かない範囲」）は検証では
落とさない——それを落とすと best-effort の契約と食い違うためである。
CSP を `Page.setBypassCSP` で迂回すると本番と異なる絵を撮ることになるため、
迂回は行わない（CSSOM 経由の注入は CSP の管轄外なので迂回する理由もない）。

**検証層自身の失敗も fail-closed である**: `getComputedStyle` が throw する
（ページ側スクリプトによる差し替え・切り離された document 等）など、静止処理の
どの層でも内部エラーが 1 件でもあれば、撮影せず失敗を返す。「静止できたと
確かめられていない」状態で撮った絵を baseline に混ぜないためである。
これは**セキュリティ境界ではない**——脅威モデルは一貫して「事故」であり、
悪意あるページへの防御ではない（成功の応答そのものを偽造するページは
原理的に検出できない）。main world の組み込み関数（`JSON.stringify` /
`getComputedStyle` / `getAnimations` 等）の差し替え・欠落を失敗として扱うのは、
悪意の検出のためではなく、polyfill や計測コードの事故・壊れた環境を
沈黙させないためである（root 側の `getAnimations` が無いと擬似要素の
アニメーションを数える口が無く、「残っていない」と「数えられなかった」を
区別できないまま撮ることになる）。
各層の失敗経路の一覧はレンダラのモジュールコメント
（`crates/service/src/render/browser.rs` の「層ごとの失敗経路」）を参照。

**静止後に始まる transition のイベントも届かない**: 静止は
`transition-duration: 0s` / `transition-delay: 0s` の注入で行う。CSS Transitions
の仕様では duration と delay の合計（combined duration）が 0s より大きいときに
だけ transition が開始されるため、静止後にスタイルが変わっても transition は
そもそも生成されず、プロパティ値だけが即座に終値へ変わる。生成されない以上
`transitionrun` / `transitionstart` / `transitionend` はどれも発火しない。
静止の時点で**すでに走っていた** transition は別で、終端へシークされて完了し、
その `transitionend` は発火する（どちらの向きもレンダラのテストで実測して
固定してある）。ゆえに **静止よりあとに始まる transition の `transitionend`
を合図に見た目を変えるコンポーネント——たとえば transition の完了を待って
クラスを付け替える、要素を取り除く、次の状態へ進めるもの——は、その更新が
起きる前の絵で撮られる**。transition を連鎖させるコンポーネントなら、静止時に
走っていた 1 本ぶんの完了イベントまでは届き、そこから先の transition は
始まらない。自分の story がこの形かどうかで、撮れる絵が変わる。厳密に最終状態を撮りたい場合は、
イベント待ちを経ずに最終状態を直接描く story を用意すること。

これは意図した割り切りである。理由は二つ。

1. このシステムの目的は決定的な静止画であり、ページは静止の直後に撮られて
   それ以降の相互作用は無い。イベントを合図に進む先の状態は撮影対象ではない
2. 仮に transition を生成させて `transitionend` を発火させると（たとえば
   Web Animations API で終端までシークして完了させる形）、それに依存する
   状態更新がいつ描画へ反映されるかが撮影タイミングに依存してしまい、
   「時刻に依存しない絵を撮る」という目的そのものに反する

イベント互換性より撮影の決定性を優先している。

なお、これが効くのは **storybook モードだけ**である。`screenshots` モード
（既定）では撮影は CI 側のテストランナーの仕事であり、VRT は受け取った PNG を
比較するだけなので、キャレット隠蔽やアニメーション静止は撮影側で行うこと
（Playwright なら `caret: 'hide'` / `animations: 'disabled'` に相当）。

#### `prefers-reduced-motion` のエミュレーション（プロジェクト単位・既定 OFF）

プロジェクト設定の `emulate_reduced_motion`（`PATCH /v1/projects/{id}`）を
有効にすると、storybook モードの撮影を `prefers-reduced-motion: reduce` を
エミュレートした状態で行う。ページのナビゲーション前に
`Emulation.setEmulatedMedia` で一度設定するので、CSS メディアクエリにも、
初期化時に `matchMedia` を読む JS にも、実利用者が OS で reduce を
設定したときと同じ条件で見える。切り替えは Web UI のプロジェクト設定
（Settings タブのチェックボックス）からも行える——同じ `PATCH` を叩く。

**効く範囲は狭い。** 上の静止機構が `getAnimations()` に載るもの
（CSS animation / transition / Web Animations）を実装の行儀に依存せず
止めるのに対し、reduced-motion が動きを止められるのは
**canvas / `requestAnimationFrame` など JS が毎フレーム描き直す実装のうち、
`prefers-reduced-motion` を自分で尊重するものだけ**である。

- **効くもの**: rAF / canvas 駆動で、かつメディアクエリ（CSS）や
  `matchMedia`（JS）を見て動きを抑える実装
- **効かないもの**:
  - メディアクエリを見ない rAF / canvas 実装（尊重しない実装に
    ブラウザ側から強制する手段は無い）
  - アニメーション画像（GIF / APNG）・`<video>`・SMIL——ブラウザ自身が
    進めるもので、そもそも「行儀」の概念が無い
  - `getAnimations()` に載るアニメーション——こちらは reduce の尊重に
    関係なく上の静止機構が既に止めている

つまり**これを入れても flaky が消えるわけではない**。静止機構が原理的に
届かない領域（rAF / canvas）のうち、行儀の良い実装との狭い交差にだけ効く
追加の手である——自前のコンポーネントに `prefers-reduced-motion` の尊重を
規約として課せる場合に最も価値がある。

**既定が OFF なのは、有効化すると撮る絵が変わるから**である。reduce を
尊重する story の絵が変わり、**そのプロジェクトの baseline が一度
入れ替わる**——有効化後の最初のビルドで出る差分をレビューして承認すること
（静止機構の「移行手順」と同じ一度きりの差分）。

**fail-closed である**: 有効にした project では、撮影直前に「エミュレーションが
実際に効いているか」を CSS カスケード（constructed stylesheet の
`@media` プローブ）と `matchMedia` の両面で実測し、**効いていると確かめられ
なければその story は撮らずにエラーにする**（`reduced-motion emulation was
requested but could not be verified as applied`）。`matchMedia` を reduce を
返さないモックへ差し替えるページ（polyfill やテストダブルの事故）はここで
落ちる。ページが両方の観測を偽装する場合は原理的に検出できない——脅威モデルが
事故であり悪意でないのは静止機構の検証と同じである。失敗経路の全数は
`crates/service/src/render/browser.rs` の「層ごとの失敗経路」を参照。

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
# build は vrt stamp に実行させる（後追いの stamp は受け付けない。
# --stats-json 付きの build が preview-stats.json も出す）
vrt stamp --dir ./storybook-static -- pnpm build-storybook --stats-json
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
- **provenance（`vrt stamp -- <build command>`）が必要**: build を vrt に
  実行させ、成果物を生成元コミットへ束縛する（詳細は `vrt plan` の節を参照）。
  無い場合と version 1（build 所有なしの旧形式）の場合は警告して全撮影に
  フォールバックする（移行期）。**あるのにコミットや内容ハッシュが
  合わない場合はエラー（終了コード 2）**——別コミットの成果物での絞り込みは
  偽 PASS を作るため、黙って読み替えない
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
  スクリーンショット名へ写像できない。部分アップロードは capture plan——
  `POST /v1/ci/builds/{id}/plan`——を使う）。どのストーリーを渡すべきかは
  `vrt upload --only-changed` が自動で決める
- `only_story_ids` を渡すときは `expected_baseline_commit_sha`（差分計画の
  起点にした baseline のコミット SHA。ビルド作成レスポンスの
  `baseline_commit_sha`）が**必須**。サーバーは現在の baseline と照合してから
  流用と比較の対象として固定する。ずれていれば 409 で拒否されるので再計画する。
  全撮影ビルドは固定されず、比較時点の最新 baseline と比較される
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

### content hash による比較省略の安全境界

同じ名前の current と baseline で `sha256:<64 hex>` が完全一致した場合、PNG の
decode と pixel diff を省略できる。ただし hash が証明するのは、その hash を計算した
時点で受け取った**バイト列の一致だけ**であり、保存実体の存在・不変性・PNG としての
健全性を単独では証明しない。

そのため baseline 昇格時に保存実体を再読し、SHA-256 の再照合と full decode に成功した
hash だけを `verified_content_hash` marker として記録する。marker が無い旧 baseline、
形式不正、`content_hash` と marker の不一致は従来どおり full decode + pixel diff へ倒す。
marker は昇格時点の健全性しか証明しないため、fast path の直前にも baseline と current の
両実体を再読して hash を照合する。欠損・後発破損・DB hash との不一致は比較失敗となり、
`passed` にはしない。upload 時は寸法検査だけに留め、全 upload へ full decode の費用を
二重に課さない。

GitHub App を設定してあれば、承認 / 却下の結果は PR の commit status に返る
（[docs/github-app.md](docs/github-app.md)）。
