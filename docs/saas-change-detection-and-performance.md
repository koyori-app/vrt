# SaaS 変更検出と比較性能の現状、および最適化方針

本書の記述基準は 2026-07-30 時点の commit `eeae652b` であり、以後の差分は `git log eeae652b..main` で確認できる。

## 目的

本書は、VRT SaaS の変更検出と画像比較について、現在の実装と今後の最適化を区別して記録する。

最適化は、①撮らない、②送らない、③比較しない、の順で検討する。
上流で除外できる画像を下流へ運ばないため、撮影、転送、比較のすべてを減らせる①を最優先とする。

変更検出は検査量を安全に減らすための最適化である。
判断できない入力は①では全撮影、②では全送信、③では全比較へ戻し、スキップを「差分なし」として扱わない。

## 現在の構成

VRT には `screenshots` と `storybook` の二つのビルドモードがある。

| モード | 撮影場所 | CI から送るもの | 現在の変更検出 |
| --- | --- | --- | --- |
| `screenshots` | CI | PNG を一枚ずつ | 全画像を撮影して送信する |
| `storybook` | VRT サーバー | Storybook の zip | `vrt upload --only-changed` で影響 story だけを撮影できる |

`storybook` モードの `--only-changed` は、公式 CLI が git 差分、webpack の `preview-stats.json`、Storybook の `index.json` を組み合わせて影響 story ID を算出する。
算出結果は `only_story_ids` として finalize API へ渡され、サーバーは対象 story だけを撮影する。
baseline に存在しない新規 story は、指定集合に含まれなくてもサーバーが撮影する。

`screenshots` モードでは撮影を CI 側のテストランナーへ任せている。
現在は CLI から対象 story 集合を受け取る経路がないため、依存グラフ上で影響がない story も撮影、送信、比較の各段階を通る。

## 実装済み

### 変更検出

`apps/backend/crates/cli/src/turbosnap.rs` は、副作用のない変更検出ロジックを提供する。
webpack stats から逆依存グラフを構築し、git の変更ファイルから到達する story ID を選ぶ。
成果物のアップロード、git の実行、API 呼び出しは `main.rs`、`git.rs`、`api.rs`、`bundle.rs` に分離されている。

選択結果は `Plan::CaptureAll(reason)` と `Plan::Only(Vec<String>)` で表される。
空の `Only` と全撮影を型で区別しているため、空集合を暗黙の全撮影や「差分なし」へ読み替えない。

次の条件では全 story の撮影へ戻る。

- branch の baseline commit がない。
- baseline commit との差分を git で取得できない。
- `preview-stats.json` がない。
- `package.json`、lockfile、または `.storybook/` が変更された。
- 変更ファイルが依存グラフに存在せず、レンダリングと無関係なファイルとしても判定できない。

依存グラフ外の Markdown など、実装が明示的にレンダリング非関連と判定するファイルだけは無視する。
stats や index の読み取り、JSON の解析に失敗した場合は CLI 自体が失敗するため、不完全な選択集合で finalize しない。

### 比較とプロダクト機能

比較ジョブは screenshot 名による完全外部結合を行い、片側だけにある画像を `added` または `removed` として記録する。
両側にある画像は PNG を読み込み、blocking pool 上で pixel diff と diff PNG の encode を実行する。
比較ペアは現在一件ずつ処理され、十件ごとの進捗と完了時の内訳が build log に残る。

次の機能も main に実装済みである。

- CLI の build progress log 追尾。
- baseline と current をドラッグ境界で比較する Swipe 表示。
- build の commit SHA から GitHub commit へのリンク。
- プロジェクト単位の build 保持数と古い build の自動削除。
- AGPL-3.0 の LICENSE。

### 旧設計項目との対応

| 項目 | 区分 | 現在の解き方 |
| --- | --- | --- |
| (a) 変更検出方式 | 実装済み | CLI が git 差分と bundle の依存情報を併用して算出し、SaaS は `only_story_ids` を適用する |
| (b) 複数検出器の和集合 | 未着手 | 現在の選択器は CLI の一系統であり、複数検出器を統合していない |
| (c) version 付き入力契約 | 実装済み | `/v1` finalize API の `only_story_ids` が選択集合の入力契約である |
| (d) fail-closed | 実装済み | baseline、git、stats、設定・依存変更、グラフ外変更を安全側へ処理する |
| (e) 空集合の専用結果型 | 実装済み | `Plan::Only([])` と `Plan::CaptureAll(reason)` を区別する |
| (f) 段階導入と shadow mode | 未着手 | 現在は CLI 選択器を直接利用し、shadow 評価は行わない |
| (g) bounded parallelism | 最適化の余地 | CPU バウンド処理は blocking pool へ移すが、比較ペアの走査は逐次である |
| (h) 性能 gate | 未着手 | 5,000 比較を public 4 CPU で測る再現可能な gate はまだ置かれていない |

## 最適化の順序

| 順序 | 段階 | 現状 | 効果 | 実装コストの中心 |
| --- | --- | --- | --- | --- |
| ① | 撮らない | `storybook` では実装済み、`screenshots` では最適化の余地 | 撮影、転送、保存、比較をすべて削減する | CLI の選択計画出力、CI ランナー連携、選択証跡 |
| ② | 送らない | 最適化の余地 | 転送量と新規オブジェクト保存量を削減する | content hash 契約、baseline hash の取得、再利用証跡 |
| ③ | 比較しない | 最適化の余地 | decode、pixel diff、diff encode の計算を削減する | hash 一致の検証、比較スキップ状態、集計との整合 |

①は三段のうち唯一、撮影そのものをなくせる。
したがって、②や③を先に広げるのではなく、まず `screenshots` モードへ①を適用する。

## 最優先の最適化: `screenshots` モードで撮らない

### `turbosnap.rs` の再利用可否

既存の算出ロジックは再利用できる。
`compute_affected_stories` はファイルシステム、git、HTTP に依存しない純関数であり、入力も git 変更ファイル、webpack stats、Storybook index と明確である。

一方、現在の `vrt upload` コマンド全体は `storybook` モード専用である。
build 作成時に mode を `storybook` へ固定し、bundle upload と finalize までを一続きに実行するため、そのまま `screenshots` の CI ランナーからは利用できない。

再利用単位は `upload` コマンドではなく、`turbosnap.rs` の解析器と選択器にする。
CLI に選択計画だけを JSON へ出力する経路を追加し、撮影は既存の CI ランナーへ任せる。

### CLI から CI への集合受け渡し

CLI は `preview-stats.json` と Storybook の `index.json` を読み、baseline commit から HEAD までの git 差分を取得する。
その後、既存の選択器で計画を算出し、機械可読な JSON を標準出力または指定ファイルへ出す。

出力契約には少なくとも次を含める。

```json
{
  "version": 1,
  "plan": "only",
  "baseline_commit_sha": "<sha>",
  "head_commit_sha": "<sha>",
  "story_ids": ["button--primary"],
  "reason": null
}
```

全撮影へ戻る場合は `plan` を `capture_all` とし、`story_ids` を省略して理由を残す。
空の `story_ids` は「影響がある既存 story はない」という選択結果であり、`capture_all` とは区別する。

CI ランナーは `plan=only` のときだけ、出力された story ID を自身のテスト選択形式へ変換して撮影する。
`plan=capture_all`、未知の `version`、JSON の解析失敗、ID 変換失敗、baseline と HEAD の不一致では全 story を撮影する。

CLI が計画した集合と CI が実際に撮影を試みた集合は build へ送る。
サーバーは、撮らなかった story を `not_captured_by_plan`、撮影を試みたが画像を得られなかった story を `capture_failed` として区別する。
いずれも `unchanged` には含めない。

### baseline 情報

選択計画は撮影前に必要である。
そのため、CLI が対象 branch の baseline commit SHA を取得できる読み取り API、または screenshots build 作成レスポンスから撮影前に同じ情報を得る経路が必要になる。

計画には baseline commit SHA と HEAD commit SHA を固定して含める。
計画生成後にどちらかが変わった場合は計画を破棄し、全撮影へ戻す。

## 次の最適化: 撮ったが送らない

①を適用しても、撮影対象の画像が baseline と同一である場合は残る。
CI は PNG の content hash を計算し、サーバーが保持する baseline の hash と一致した画像の本体送信を省略できる。

hash の照合方法には次の二案がある。

- **A: hash 問い合わせ API**: 画像ごとに server へ既存 hash の有無を問い合わせる。
- **B: baseline hash 一覧**: build 開始時に story ID と hash の一覧を取得し、CI 内で照合する。

多数の画像を扱う通常経路では、往復回数を抑えられる B を第一候補とする。
A は一覧が大きすぎる場合や、単発アップロードの補助経路として使える。

送信を省略するときも、story ID、content hash、参照した baseline build、再利用元 screenshot を build へ記録する。
hash algorithm や契約 version が未知、baseline が変化、一覧が不完全、または hash が一致しない場合は画像本体を送る。
送らなかった画像は `reused_by_content_hash` として扱い、「今回比較して差分がなかった」とは記録しない。

## その次の最適化: 送ったが比較しない

画像を受け取った後でも、検証済み content hash が baseline と一致する場合は decode、pixel diff、diff PNG encode を省略できる。

比較を省略するには、同一の正規化規則と hash algorithm、同じ project、story ID、viewport、レンダリング条件、baseline を確認する。
条件が一つでも判定できなければ通常の画像比較を実行する。

結果には `comparison_skipped_identical_content` と再利用元を記録する。
この状態は比較を実行した `unchanged` と集計上の結果が同じでも、処理証跡では区別する。

## 比較ペアの bounded parallelism

①から③を適用した後に残る比較ペアは、メモリ予算付きで並列処理できる。
現在の比較はペアを逐次走査し、各ペアの CPU バウンド処理だけを blocking pool へ移しているため、ペア間並列化には最適化の余地がある。

worker 数は CPU 数だけでなく、比較一件の decoded baseline、decoded current、diff buffer、encode 作業領域を含む実測メモリから決める。

```text
workers = min(
  available_parallelism,
  configured_worker_cap,
  floor(comparison_memory_budget / measured_p95_bytes_per_comparison)
)
```

並列 worker は永続化を直接行わず、story ID と比較結果を coordinator へ返す。
coordinator は story ID で安定 sort してから保存し、一件でも失敗した attempt の中間結果を公開しない。

## 性能 gate

最適化の効果は同じ fixture、同じ commit、同じ runner 条件で測る。
判定基準は 5,000 比較、public 4 CPU、per-shard 120 秒以内、peak RSS 2 GiB 以下とする。

計測は少なくとも撮影、転送、保存、download、decode、pixel diff、encode、DB 書き込みを分ける。
既存計測では decode の比重が大きいという前提を維持し、pixel comparison のマイクロ最適化だけを先に選ばない。

## 未着手

現在の main を基準にすると、次の項目は未着手である。

- `screenshots` モードへ渡す version 付き選択計画の CLI 出力。
- CI ランナーが選択された story だけを撮影し、選択集合と実行集合を build へ送る契約。
- baseline content hash の問い合わせまたは一覧取得と、画像本体の再利用。
- hash 一致時の比較スキップ状態と処理証跡。
- 複数の変更検出器を shadow 評価し、安全な集合として統合する経路。
- public 4 CPU 上で 5,000 比較を再現する性能 gate。

これらは現在の `storybook --only-changed` が成立していないことを意味しない。
既存経路はそのまま維持し、最適化を `screenshots` モードから段階的に追加する。
