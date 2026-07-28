# SaaS 変更検出と比較性能の設計

## 目的

本書は、汎用 VRT SaaS が撮影対象を安全に減らし、撮影後の画像比較を一定の資源内で処理するための設計を定める。
特定クライアントの selector や manifest 形式は前提にしない。

変更検出は速度のための最適化であり、検査範囲を暗黙に狭める許可ではない。
判断材料が不足する場合は全撮影へ戻し、部分撮影を「差分なし」として通過させない。

## 用語

- **Story inventory**: アップロードされた Storybook 成果物から SaaS が列挙した撮影可能な story の全集合。
- **選択計画**: 今回撮影する story の集合と、各 story を選択または除外した理由を記録したもの。
- **クライアントヒント**: クライアントが自身の依存グラフなどから算出し、SaaS へ送る候補集合。
- **撮影証跡**: 選択計画、実行した撮影、保存できた画像を対応づける永続レコード。
- **未撮影**: 選択計画によって撮影対象外になった状態。
  baseline と一致したことを示す「差分ゼロ」とは異なる。

## 変更検出方式の比較

### (a) クライアントが選択集合を算出する

クライアントはソースコード、Storybook の索引、依存グラフ、変更ファイルなど、SaaS から見えない情報を利用できる。
既存 CI へ段階的に導入しやすく、SaaS の計算負荷も小さい。

一方、クライアント実装ごとに精度が異なり、古いロジックや改変された入力を SaaS がそのまま信頼すると偽陰性が生じる。
クライアントヒントだけで build を `passed` にしてはならない。

### (b) SaaS が選択集合を算出する

SaaS はアルゴリズム、監査記録、段階的な更新を一元管理できる。
利用者が専用 selector を導入しなくても最適化を適用できる。

ただし、完成済み Storybook だけでは、ソースファイルから story までの依存関係を復元できない場合がある。
依存メタデータを build artifact に含める方式や、前回 artifact との比較方式を別途定義する必要がある。
算出根拠が不足した build は全撮影へ戻さなければならない。

### (c) 両方を許す

本設計は (c) を採用する。
クライアントヒントは撮影を減らす候補として受け取り、SaaS は Story inventory と入力の結び付きを検証して最終的な選択計画を確定する。
将来 SaaS 側の変更検出器を追加した場合も、同じ選択計画と撮影証跡へ出力する。

方式を併用しても、二つの判定を無条件に交差させてはならない。
両方が有効な場合の既定値は和集合とし、一方が不明または不正なら全撮影へ戻す。
これにより、精度の異なる検出器を追加しても検査範囲が意図せず縮まらない。

## SaaS の入力契約

変更検出入力は、特定リポジトリのファイル形式ではなく、version 付き API 契約として受け取る。
契約は最低限、次の情報を持つ。

- 契約 version
- project、build、commit の識別子
- Storybook artifact の digest
- 選択された story ID の集合
- 算出器の識別子と version
- 比較元と比較先の参照
- 入力集合の digest
- story ごとの選択理由

SaaS はアップロード完了後に artifact の digest を再計算し、Story inventory を抽出する。
クライアントヒントが別 artifact に対して算出されている場合や、未知の契約 version を使っている場合はヒントを破棄して全撮影する。

選択された story ID が Story inventory に存在しない場合も全撮影する。
空集合は正当性を証明できる専用の結果型として扱い、単なる空配列とは区別する。
専用結果型をまだ実装していない段階では、空集合を全撮影として扱う。

## 選択から撮影までの状態

変更検出後の build は、少なくとも次の集合を区別して記録する。

- `inventory`: 成果物に存在する全集合
- `selected`: SaaS が確定した撮影対象
- `executed`: ブラウザへ撮影を指示し、完了応答を得た対象
- `captured`: 画像をストレージへ永続化し、digest を記録できた対象
- `omitted`: 正当な選択計画によって撮影しなかった対象

比較へ進める条件は `selected == executed == captured` である。
集合が一致しなければ build を `failed` にするか、比較開始前に全撮影を再実行する。
欠けた対象を `unchanged` として補完してはならない。

`omitted` は選択計画に理由を持つ story だけで構成し、`inventory - selected` と一致させる。
比較の完全外部結合では、`omitted` を `removed` として扱わない。
`removed` は今回の Story inventory に存在しない baseline entry に限る。

選択計画と撮影証跡には、それぞれ正規化した集合の digest を保存する。
比較ジョブは digest と件数を再検証し、検証できない build を `passed` へ遷移させない。

## fail-closed の規則

次のいずれかに該当する場合、SaaS は全撮影へ戻す。

- 契約 version、算出器、story ID、参照形式のいずれかが未知である
- artifact digest、入力集合の digest、commit の結び付きが一致しない
- 選択計画を再現または検証できない
- 複数の検出器が矛盾し、安全な和集合を作れない
- 前回の inventory または baseline が欠けている
- 空集合の正当性を証明できない

全撮影へ戻した事実と理由は build record に残す。
全撮影自体に失敗した場合は build を `failed` にする。
変更検出を無効化して比較だけを成功扱いにする経路は設けない。

## 段階的な導入

第1段階では、version 付き API でクライアントヒントを受け取り、SaaS が Story inventory、artifact digest、集合一致を検証する。
SaaS 側の依存グラフが未実装でも、検証不能時の全撮影によって安全性を保てる。

第2段階では、SaaS が artifact 間の inventory 差分と、成果物に含めた依存メタデータから選択候補を算出する。
算出器はクライアントと同じ選択計画形式へ出力する。

第3段階では、複数の算出器を shadow mode で評価する。
shadow mode では全撮影結果を正解集合として偽陰性を測り、十分な観測期間を経た算出器だけを撮影削減へ使う。

## 比較ジョブの bounded parallelism

画像比較は、baseline 読み込み、今回画像の読み込み、decode、pixel comparison、必要時の diff encode と保存までを一つの比較単位とする。
decode が支配的であるため、pixel comparison だけを並列化しても十分な改善にならない。

同時実行数は次の最小値とする。

```text
workers = min(
  available_parallelism,
  configured_worker_cap,
  floor(comparison_memory_budget / measured_p95_bytes_per_comparison)
)
```

`available_parallelism` はコンテナが認識する CPU 数を使う。
`configured_worker_cap` は運用側が設定できる上限である。
`comparison_memory_budget` はプロセス全体の上限から、runtime、DB pool、キュー、ブラウザなどの固定費を引いた値である。

比較一件のメモリ見積もりには、圧縮済み入力、baseline と今回画像の decoded buffer、diff buffer、encode 作業領域を含める。
未知の画像寸法を平均値だけで見積もらず、観測した p95 と最大寸法の安全係数を使う。
予算から一 worker も割り当てられない入力は、直列処理へ落とすか、画像寸法上限違反として明示的に失敗させる。

## 結果不変性

並列化は比較結果の意味を変えてはならない。
各比較単位は入力を immutable とし、共有する集計値や出力配列を直接更新しない。

worker は story key、status、diff digest、diff pixel count、diff ratio、phase 計測値を結果として返す。
coordinator は全 worker の終了後に story key で安定 sort し、逐次版と同じ順序で永続化する。

比較中に一件でも失敗した場合は、新しい結果集合を build へ部分反映しない。
一時オブジェクトは build と attempt に紐付け、成功した attempt だけを transaction で可視化する。
リトライ時に過去 attempt の出力を混ぜない。

逐次版と並列版には、同じ fixture 集合を入力する differential test を設ける。
`added`、`removed`、`unchanged`、`changed`、decode failure、寸法違い、しきい値境界を含め、status、件数、diff digest、diff pixel count、diff ratio が一致することを検証する。
実行順序を意図的に変えても結果が一致することを固定する。

## 計測

本番 CI で再現できる benchmark job を用意し、5000 比較を固定 fixture と固定設定で実行する。
shard ごとに cold start と warm-up を分離し、判定対象の計測区間を明記する。

記録項目は次のとおりである。

- 総所要時間と shard 所要時間
- read、decode、compare、encode、write、DB commit の各 phase の p50 と p95
- peak RSS
- 比較件数、画像寸法と fixture digest
- worker 数とメモリ予算
- runner の論理 CPU 数、OS、architecture、runner image 名と version
- commit SHA、Rust toolchain、build profile
- 成功、差分あり、失敗の件数

phase 計測は比較単位ごとの monotonic clock で収集し、集計処理自体を判定区間から分離する。
peak RSS は Linux runner で `/usr/bin/time -v` など、計測方法を workflow 内に固定して取得する。
runner が 4 CPU でない場合や fixture digest が違う場合は gate 判定を行わない。

## 性能 gate

public GitHub Actions の 4 CPU runner で、5000 比較を実行する。
各 shard は 120 秒以内、peak RSS は 2 GiB 以内を合格条件とする。
SKIP、欠損 metric、runner 条件の不一致が一つでもあれば未計測として扱い、合格を記録しない。

gate を満たさない場合は、次の順序で対策する。

1. job を独立した shard へ分割する
2. decode を native 実装または同等の低オーバーヘッド経路へ移す
3. より大きい runner を選択する
4. 一 shard の比較規模を縮小する

既存計測では decode の比重が大きいため、pixel comparison のマイクロ最適化を最初の対策にしない。
対策後は同じ fixture digest と runner 条件で再計測し、結果不変性テストも再実行する。

## 未決事項

- SaaS 側で利用できる依存メタデータの生成形式と、対応する Storybook version
- クライアントヒントへ署名を要求するか、認証済み API と digest binding で足りるか
- 選択計画、撮影証跡、phase metric の保持期間
- 一比較の p95 メモリ量と安全係数を決めるための実測
- shard の分割キーと、同一 build の最終 commit を transaction 化する方法

これらが未決でも、未知入力を全撮影へ戻し、集合不一致を失敗させる規則は変更しない。
