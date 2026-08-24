# baseline 選定を git の系譜に変える設計

本書の記述基準は 2026-08-18 時点の commit `7fe200b` であり、以後の差分は `git log 7fe200b..main` で確認できる。
背景と実例は Issue #38 とそのコメントに記録済みであり、本書は選定規則・API・移行手順を実装可能な粒度で確定させる。
本書は設計のみを扱い、実装は後続の cmd で行う。

## 目的

比較相手の baseline を「作成時刻の降順」ではなく「git の祖先関係」で選ぶ。

現行の `apps/backend/crates/service/src/baselines.rs` の `latest_for` は次の順で選んでいる。

1. 同一ブランチの最新 baseline（`created_at` 降順）
2. 無ければプロジェクトのデフォルトブランチの最新 baseline
3. それも無ければ `None`（初回ビルド扱い）

第 2 段が git の履歴を見ないため、枝の派生後にデフォルトブランチが進むと、枝が持っていない変更を枝自身の差分として報告する。
Issue #38 のビルド 652 では、この経路で 35 件の story が `removed` と誤報された。

## 選定規則

> HEAD から git の祖先を遡り、最初に見つかった「受け入れ済みのビルド」が指す baseline を比較相手とする。

### 「受け入れ済み」の定義

次のいずれかを満たすビルドを受け入れ済みとする。

| 状態 | baseline の引き先 | 根拠 |
| --- | --- | --- |
| `approved` | そのビルドを `source_build_id` に持つ baseline | 承認が baseline を生む（`builds.rs` の `approve_build`） |
| `passed` かつ `baseline_id` が非 NULL | ビルドが記録した `baseline_id` の baseline | 差分ゼロは「その baseline と一致した」という事実。見た目はその baseline そのもの |

`passed` を数えない場合、より遠い祖先まで遡ることになり、間に入った承認済みの変化を再び差分として出してしまう。
`passed` のビルドは比較ジョブが使った baseline を `builds.baseline_id` に記録済みであるため（`compare_build.rs` の `apply_counts`）、baseline 行の新設もマイグレーションも要らない。

`passed` かつ `baseline_id` が NULL のビルドは受け入れ済みに数えない。
これは比較相手が無いまま完走した初回ビルドであり、辿る先の baseline を持たないためである。
`rejected` / `failed` / 未完走のビルドも数えない。

### 「派生時点の直近」を規則にしない理由

派生地点の baseline を規則にすると、枝が自分の承認を失う。
枝上に承認済みビルド B1 があれば B1 を使うべきであり、派生地点は「枝上に何も無いときに祖先遡りが自然と行き着く先」にすぎない。
規則は祖先遡りの一本で書き、派生地点を特別扱いしない。

## サーバーは git リポジトリを持たない

祖先関係を知っているのは CLI（CI 環境の checkout）だけである。
よって役割を次のように分ける。

- **CLI**: `git rev-list` で HEAD から祖先を歩き、候補 SHA 群をサーバーへ問い合わせ、受け入れ済みビルドを特定する
- **サーバー**: 「この SHA 群のうち受け入れ済みのビルドを持つものはどれか」に答える問い合わせ口を提供する

### 新設 API: 受け入れ済みビルドの照会

```
POST /v1/ci/projects/{tenant}/{project}/builds/accepted-query
Authorization: Bearer <CI token>

リクエスト:
{ "commit_shas": ["<sha>", ...] }        // 上限 200 件/回

レスポンス:
{
  "accepted": [
    { "commit_sha": "<sha>", "build_number": 123, "baseline_id": "<uuid>" }
  ],
  "last_accepted_on_branch": {           // クエリパラメータ branch= を添えたときだけ
    "commit_sha": "<sha>", "build_number": 120, "baseline_id": "<uuid>"
  } | null
}
```

サーバー側の実装は既存部品の組み合わせで足りる。

- 受け入れ済み判定: `builds` を `project_id` と `commit_sha IN (...)` で引き、`approved` は `baselines.source_build_id` の逆引き、`passed` は自身の `baseline_id` を返す
- 枝名検索: 既存 `latest_on_branch` の結果から `baseline_source_commit_sha` で SHA を引く

同一 SHA に複数ビルドがある場合（再実行など）は、受け入れ済みのうち `number` 最大のものを返す。

### CLI の歩き方

chromatic-cli の `getParentCommits.ts` と同じ形を採る。

1. `git rev-list HEAD --not <既知の受け入れ済み SHA 群>` を件数上限つきで実行する
2. 得た SHA 群を `accepted-query` に問い合わせる
3. 見つかった受け入れ済み SHA は次回の `--not` に加えて遮蔽し、同じ経路ではより子孫側のビルドだけが残るようにする
4. 新しい受け入れ済みビルドが見つからなくなるか、遡行上限（初期値 1000 commit）に達したら打ち切る
5. 件数上限は 10 → 40 → 160 → … と段階的に広げ、直近に受け入れ済みビルドがある通常ケースを 1〜2 往復で終わらせる

`--not` による遮蔽が chromatic-cli の `maximallyDescendentCommits`（A が B の祖先なら B だけを採る）に相当し、結果は「互いに祖先関係に無い受け入れ済みビルドの集合」になる。

### shallow clone の扱い

CI の checkout が shallow だと rev-list が途中で切れ、祖先に受け入れ済みビルドがあっても届かないことがある。

- 遡行が shallow 境界（`.git/shallow` に載る grafted commit）で打ち切られ、かつ候補が一つも見つからなかった場合、CLI は警告を出して後述のフォールバック順位に落とす
- README / CI テンプレートに `fetch-depth: 0` を推奨として明記する
- chromatic-cli が行う自動 unshallow（追加 fetch）は行わない。CI の権限と帯域への副作用が大きく、まず明示設定で足りるためである

## 返すのは集合か単一か

**API とデータの形は最初から集合、比較の実装は第 1 段では単一で始める。**

merge commit では baseline が 1 つに定まらない。
main を取り込んだ merge commit M から遡ると、枝側の承認 B と main 側の承認 X の両方に届き、story によって正しい比較相手が異なる。
chromatic-cli が集合を返し「どれか 1 つに一致すれば通す」造りにしているのはこのためであり、単一前提の規則は merge で破綻する。

一方で、比較ジョブ（`compare_build.rs`）を集合対応にするには、エントリ突き合わせ・carry-forward・承認整合（`approval.rs` の `baseline_is_current`）のすべてに手が入り、一度に変えるには大きすぎる。

よって次のように段を切る。

- **形**: CLI → サーバーの受け渡しは `Vec`（後述の `ancestor_candidates`）とし、API の互換を壊さずに第 2 段へ進めるようにする
- **第 1 段**: 集合の先頭（後述の優先順位で決めた 1 件）だけを比較に使う。merge commit 直後のビルドでは main 側から来た差分が誤検知として出るが、これは現行でも起きている誤りであり、悪化はしない
- **第 2 段**: 比較を「候補 baseline のいずれかに一致すれば `unchanged`」の集合突き合わせに拡張する。エントリ名ごとに、より子孫側の候補を優先して比較する

第 2 段は本設計の範囲に含めるが、実装の順序として第 1 段の後に置く。

## rebase / force-push の扱い

載せ直すと枝の古い commit は HEAD の祖先から消えるため、祖先遡りだけでは枝自身の承認済みビルドに辿り着けない。

**現行の枝名検索（`latest_on_branch` の第 1 段）は rebase 用の補助として残す。**
chromatic-cli も祖先探索とは別に「同じ枝名の最後のビルド」を候補に加えており（`--ignore-last-build-on-branch` はそれを切る旗）、枝名検索そのものは誤りではない。
Issue #38 / #581 の事故は、補助しか無い状態で補助が本筋の代役を務めた結果である。

候補の合成と優先順位は次のとおり。

1. 祖先集合に枝名候補（`last_accepted_on_branch`）と同じ baseline があれば重複として除く
2. 第 1 段の単一選定では **枝名候補 > 祖先候補** とする
3. 枝名候補が無ければ祖先集合の先頭（最も子孫側）を使う
4. どちらも無ければ `None`（初回ビルド扱い）

枝名候補を先に置く理由は二つある。
第一に、rebase 直後に祖先候補（新しい main の承認）を使うと、枝で一度承認した差分が全部再提示され、#581 型のレビュー地獄が再発する。
第二に、この順位は現行 `latest_for` の第 1 段と同じ振る舞いであり、既存プロジェクトの挙動変化を「第 2 段（デフォルトブランチへの落下）の置き換え」だけに絞れる。

枝名候補を使いたくない運用（枝名の再利用が多いリポジトリなど）のために、CLI に `--ignore-last-build-on-branch` 相当の旗を用意する。

なお、デフォルトブランチの最新 baseline への無条件フォールバック（現行第 2 段）は**廃止**する。
これが #38 の事故経路そのものであり、祖先遡りがその役割を正しく置き換える。

## squash merge の扱い — 先送りする

枝を squash して main に入れると、main の新 commit は枝の commit と血縁を持たず、遡っても枝の承認には届かない。
chromatic-cli はここを別建てにし、merge commit のメッセージや provider API から PR を引き当て、その head ビルドを候補に加えている。

本設計では**先送り**とする。理由は次のとおり。

- 影響が限定的である。squash 後の main のビルドは、祖先遡りで「squash 前の main の受け入れ済みビルド」に正しく届く。失われるのは「枝上で承認した差分を main で再確認せずに済む」便益だけであり、main 上で一度レビューすれば以後は積み重ならない。#38 の事故（無関係な差分が誤って出続ける）とは深刻度が違う
- 実装の依存が重い。PR と commit の紐付けには GitHub API（既存 GitHub App 連携）への問い合わせが要り、GitHub 以外の CI やトークン権限の考慮が増える
- 判定材料は既に残る。`builds.pull_request_number` を記録しているため、後日「squash merge commit → PR 番号 → その PR の head ビルド」を引く拡張は本設計の候補集合にそのまま足せる

先送りの判断は本書に記録し、必要になったら候補集合への追加として実装する。

## ビルドへの受け渡しと固定

CLI は歩いた結果をビルド作成時に添える。

```
POST /v1/ci/builds  （NewBuild に追加）
{
  ...,
  "ancestor_candidates": [        // 省略可。順序が優先順位
    { "commit_sha": "<sha>", "baseline_id": "<uuid>" }
  ]
}
```

- サーバーは先頭候補の baseline を `builds.baseline_id` に**作成時点で固定**する
- `baseline_id` の既存セマンティクス（NULL = 比較時に最新を解決）は互換のため残す。`ancestor_candidates` 省略時（旧 CLI・手動 API）は現行の `latest_for` にフォールバックする
- ビルド作成レスポンスの `baseline_commit_sha`（差分撮影 turbosnap の起点）は、固定した baseline の `baseline_source_commit_sha` を返す。これにより撮影範囲の計画と比較相手が最初から一致し、finalize 時の `expected_baseline_commit_sha` 照合はそのまま生きる
- 比較ジョブは `baseline_id` が固定済みならそれを使い、`latest_for` を呼ばない（部分撮影の固定と同じ経路）

`ancestor_candidates` の `baseline_id` はサーバーが `accepted-query` で返した値であり、CLI が捏造できる余地はサーバー側の再検証（その baseline が当該プロジェクトに属し、候補 SHA の受け入れ済みビルドと結びついているか）で塞ぐ。

### 承認整合への影響

`approval.rs` の `baseline_is_current` は「ビルドが比較した baseline がいまも最新か」を照合している。
系譜固定後は「最新」の意味が「枝名の最新」から「そのビルドの系譜上の比較相手」に変わるため、照合は「`builds.baseline_id` が承認時点でも同じ解決結果になるか」= 固定値同士の一致検査に単純化される。
承認レースの検査意図（比較後に baseline がすり替わった状態で承認させない）は固定によってむしろ強くなる。
詳細な検査条件の書き換えは実装 cmd で行う。

## 段階移行

現行方式と併存させ、差を観測してから切り替える。

### 第 0 段: 影走行（shadow）

- CLI は `ancestor_candidates` を常に計算して送るが、サーバーは**固定せず**、現行 `latest_for` の結果と並べて記録だけする
- 記録先は tracing ログ（`build_id` / `legacy_baseline_id` / `ancestry_baseline_id` / `agree: bool`）とメトリクスカウンタとし、スキーマ変更を要しない
- 不一致率と不一致の実例（#38 の再現条件で ancestry 側が正しい baseline を指すか）を確認する

### 第 1 段: 切り替え（単一選定）

- プロジェクト設定（`projects` に boolean 1 列、既定 false）で ancestry 固定を有効化する
- 有効プロジェクトでは作成時固定・`latest_for` 不使用、無効プロジェクトと旧 CLI は現行動作のまま
- koyori-app/task など実プロジェクトで有効化し、#581 型の誤検知が消えることを確認してから既定を true に倒す

### 第 2 段: 集合比較

- 比較ジョブを候補集合対応にし、merge commit 直後の誤検知を解消する
- ここで `builds.baseline_id`（単数）に加えて候補集合の永続化（`build_baseline_candidates` 中間テーブルまたは JSON 列）が要る。設計詳細は第 2 段着手時に本書へ追記する

各段はいつでも設定で現行方式へ戻せるため、切り替えに伴うデータ移行は無い。

## 実装範囲の見取り図（後続 cmd の分割案）

| 段 | 触る場所 | 内容 |
| --- | --- | --- |
| 0-a | `handler/handlers/ci.rs`・`service/builds.rs` | `accepted-query` エンドポイント |
| 0-b | `cli/git.rs`・`cli/api.rs`・`cli/main.rs` | rev-list 遡行と候補計算・`ancestor_candidates` 送信 |
| 0-c | `service/builds.rs`（`create_build`） | 影走行の記録（固定はしない） |
| 1 | `service/builds.rs`・`job/compare_build.rs`・`service/approval.rs` | 作成時固定・`latest_for` バイパス・承認整合の書き換え・プロジェクト設定 |
| 2 | `job/compare_build.rs`・entity | 集合比較 |

## 決めたことの一覧

| 論点 | 決定 | 主な理由 |
| --- | --- | --- |
| 選定規則 | HEAD から祖先を遡り最初の受け入れ済みビルド | 派生時点基準は枝の承認を失う |
| `passed` の扱い | 受け入れ済みに数え、記録済み `baseline_id` を辿る | 差分ゼロ＝baseline と同一。新規テーブル不要 |
| サーバー git 無し | CLI が rev-list を歩き、`accepted-query` で照会 | 祖先関係を知るのは CI checkout だけ |
| 集合か単一か | 形は集合、比較は第 1 段単一 → 第 2 段集合 | merge で単一は破綻するが、比較ジョブの集合化は段を分ける |
| rebase / force-push | 枝名検索を補助として残す（優先は枝名 > 祖先） | 枝の承認消失（#581 型）の再発防止・現行第 1 段と同挙動 |
| デフォルトブランチへの落下 | 廃止 | #38 の事故経路そのもの |
| squash merge | 先送り（PR 紐付け拡張の受け皿だけ確保） | 誤検知が積み重ならず深刻度が低い・依存が重い |
| 段階移行 | 影走行 → プロジェクト単位切替 → 集合比較 | 差を観測してから切り替え、いつでも戻せる |
