# 「送らない」最適化の要否見積もりと設計

本文書は、`docs/saas-change-detection-and-performance.md` が②と番号づけた「送らない」最適化について、要否の見積もりと設計を定める。
本文書の段階では設計のみを行い、実装は含めない。

**「送らない」最適化**とは、CI が撮影した PNG のうち、サーバーが保持する baseline と内容が一致するものについて、画像本体の送信を省く仕組みである。
①（撮らない）と③（比較しない）は導入済みであり、②が主として削減するのは転送量である。
ビルド時間への効果は条件つきである。
時間の支配項が decode である構成では効かないが、CI 側の帯域が細く upload が critical path に入る構成では時間短縮になりうる。

## 結論：実装見送り

②は現時点では実装しない。
設計は本文書のとおり固め、文書として残す。

見送りの理由は「削減量が小さいから」ではなく、「削減量をまだ判断できないから」である。

- 転送削減の見積もりは前提しだいで桁が二つ動く。後述の一点推定では月およそ 2.8 GB だが、撮影率・バイト一致率・PNG サイズ・build 頻度の置き方しだいで月数 GB から数百 GB まで振れる（「前提が崩れる条件と結論の向き」）。
- 振れ幅を狭める実測データ（とくにバイト一致率）が現時点で存在しない。
- ストレージ削減はゼロである（実体コピー方式のため、新規オブジェクトは送信した場合と同じ量だけ増える）。
- ビルド時間の削減は構成依存である（時間の支配項が decode なら効かないが、upload が critical path に入る構成では効きうる）。

したがって、いま言えるのは「実測なしに着手する根拠がない」ことまでである。
実測が得られたら「再検討の指標」で転送削減量を再評価し、着手を再判断する。
一致率が実測でゼロに近ければ、②のヒット率もゼロであり、見送りのままとする。

見積もりの前提とした仮定は次のとおりである（一点推定。詳細は「仮定と削減量」）。

- story 数 300
- ①適用後に撮影する割合 10%（30 枚/build）
- 撮影分のうちバイト一致する割合 70%（未実証）
- PNG 1 枚 200 KB
- build 頻度 30/日 × 22 日 = 660/月

## 要否の見積もり

### 効果がバイト列の再現性に全面依存すること

content hash は受領した PNG のバイト列に対する sha256 である（`apps/backend/crates/service/src/screenshots.rs` の `content_hash`）。
decode 後のピクセルではない。
したがって、再撮影した PNG がバイト単位で baseline と一致しない限り、②のヒットはゼロになる。
バイト一致するかどうかは CI 側の撮影環境と PNG encoder の決定性に依存し、現時点で実測データはない。
③の fast path のヒット数を数える `content_hash_skipped_count` は一致の発生を示す観測値になるが、分母（照合対象の総数）を持たないため一致率そのものではない。
この計測にもまだ実運用データがない。

### 仮定と削減量

利用プロジェクトの規模の実データはないため、次をすべて仮定として置く。

| 項目 | 仮定値 | 備考 |
| --- | --- | --- |
| story 数 | 300 | 実データなし |
| ①適用後に撮影する割合 | 10%（30 枚/build） | 選択器は依存関係に対して保守的で、撮影対象の多くは実際には変化しない前提 |
| 撮影分のうちバイト一致する割合 | 70% | 未実証。encoder の決定性しだいでゼロにもなりうる |
| PNG 1 枚 | 200 KB | コンポーネント撮影の想定。フルページは 2 MB を超えることもある |
| build 頻度 | 30/日 × 22 日 = 660/月 | 実データなし |

この仮定では、転送削減は月あたりおよそ 660 × 30 × 0.7 × 0.2 MB ≒ 2.8 GB になる。

ストレージ削減は、本設計（実体コピー方式）ではゼロである。
サーバー側でコピーを作るため、新規オブジェクトは送信した場合と同じ量だけ増える。
参照方式なら転送と同程度のストレージ増加を抑えられるが、後述の理由で採らない。

### 前提が崩れる条件と結論の向き

上の 2.8 GB は一点推定であり、確定値ではない。
前提が崩れる条件と、崩れたとき結論がどちらへ動くかを列挙する。

| 条件 | 結論への向き |
| --- | --- |
| 撮影率が 10% を大きく超える（選択器の保守性、変更の大きい開発局面） | 削減量が比例して増え、実装へ寄る |
| PNG 1 枚のサイズが 200 KB を超える（フルページ撮影の比率が高い） | 削減量が比例して増え、実装へ寄る |
| build 頻度が 660/月 を超える（利用プロジェクト増、SaaS 化） | 削減量が比例して増え、実装へ寄る |
| バイト一致率が実測で高い（encoder が決定的） | ヒット率が上がり、実装へ寄る。逆にゼロ近傍なら②は無意味で、見送り確定 |
| CI 側の帯域が細く upload が critical path に入る | 転送削減が時間削減にもなり、実装へ寄る |
| storage 側で native copy（サーバー内コピー）が使えるようになる | claim 処理の実体読み・再アップロードが不要になりコストが下がり、実装へ寄る |
| 実体コピー前提が参照方式へ変わる | ストレージ削減も生まれるが、寿命管理の新設が要り、本設計自体の作り直しになる |

撮影率・PNG サイズ・build 頻度・一致率は互いに掛け算で効くため、複数が同時に動くと削減量は桁で変わる（月数 GB から数百 GB まで）。
振れ幅が二桁ある以上、一点推定を根拠に「割に合わない」と断定することはできず、逆に「割に合う」と断定することもできない。

### 判断できるか

self-host 構成では、一点推定の月 2.8 GB という水準の転送削減は小さい。
意味を持つ規模になるのは主に SaaS 化後で、ユーザー側 CI の帯域と、サーバー側の受信処理を減らせる可能性がある。
ただし前提のどれもが実測を欠くため、この見積もりは着手可否の判断材料にならない。
着手判断は「再検討の指標」の実測を待つ（冒頭の結論のとおり）。

### 再検討の指標

再検討には、転送削減量を再評価できる次の実測値を用いる。

- 一致率：分母は「検証済み hash を持つ baseline entry と照合できた shot 数」、分子はそのうちバイト一致した数。
- 一致バイト総量：バイト一致した画像の合計バイト数。枚数だけでは PNG サイズの偏りを拾えないため、バイト加重で見る。

`content_hash_skipped_count` 単独では判断できない。
③ fast path のヒット「数」だけであり、分母も一致画像のバイト総量も持たないため、転送削減量の再評価に足りないからである。
なお③の fast path が数える対象は、比較ジョブ側で shot と baseline entry の hash が一致し、entry の `verified_content_hash` 照合と保存実体の再照合まで通った組である（`compare_build.rs` の `compare_pair`）。
②の照合は plan 添付一覧（検証済み entry のみ）に対して CI 側で行うため、③のヒット集合と②の照合対象集合は同じとは限らない。
このずれを持ち込まないため、一致率の分母は上の定義「検証済み hash を持つ baseline entry と照合できた shot 数」一つに固定する。

backend の現状と不足は次のとおりである。

- ある：build ごとの `content_hash_skipped_count`（一致数。分子に相当）。
- ない：分母。検証済み hash を持つ baseline entry と照合できた shot 数（hash が NULL の entry 等、照合できなかった shot は含めない）。
- ない：一致バイト総量。fast path で skip した shot のバイト数の合算。

不足分は build 単位のカウンタ追加（照合対象総数、skip バイト総量）で足りる。
本文書では必要性の記載にとどめ、実装しない。

一致率と一致バイト総量が得られたら、実際の story 数・build 頻度を掛けて転送削減量を再計算し、着手を再判断する。

## 設計

### 照合の形（baseline hash 一覧の先渡し）

capture plan 添付の応答を拡張し、pin 済み baseline の `{name, content_hash}` 一覧を返す。
CI は撮影後に各 PNG の sha256 を手元で計算し、一覧と照合する。

一覧に載せるのは `content_hash == verified_content_hash` が成り立つ検証済み entry だけとする。
hash が NULL の entry（hash 導入前の baseline）や未検証の entry は載せない。
一覧に載らない story は照合できず、必ず本体送信になる。
これは fail-closed であり、②の全体を通じた原則である。

画像ごとに問い合わせる API 案は、往復回数が枚数に比例するため採らない。
実体を送ってからサーバー側で重複破棄する案は、転送削減がゼロで目的に合わないため採らない。

### 再利用の宣言（reuse claim）

hash が一致した story について、CI は本体の代わりに reuse claim（`{name, content_hash}` のみの POST）を送る。
サーバーは、既存の PNG アップロード（`store_ci_screenshot`）と同じガード（build 行ロック、pending 状態、capture plan の selected 照合、同名重複拒否）の内側で次を行う。
ただし同名重複拒否には例外を一つ設け、reuse claim では同一 claim（同じ `{name, content_hash}`）の再送を拒否せず受理済みとして返す（後述の冪等再送）。

1. pin 済み baseline の該当 entry を引く。entry がない、hash が一致しない、または hash が NULL なら claim を 4xx で拒否する。
2. entry の storage 実体を読み、バイト列から hash を再計算し、claim の hash と一致することを確認する。
3. carry-forward と同じ機構（決定的 UUIDv5 キー、upload、`ON CONFLICT DO NOTHING`、失敗時の補償削除）で、build 所有の storage key へ実体コピーする。
4. screenshots 行を insert する。`content_hash` はサーバーが再計算した値を保存する。metadata には `reused_by_content_hash: true`、claim された hash、参照元の baseline entry を記録する。carry-forward が書く `reused` キーは書かない。

どの段階の失敗も claim の拒否（4xx/5xx）として返す。
CI は拒否されたら本体送信へフォールバックするか、build を失敗させる。
claim の失敗を黙って unchanged 扱いにする経路は作らない。

claim の応答を CI が受け取り損ねた場合（タイムアウト、接続断）に備え、claim の再送は冪等とする。
サーバー処理は決定的 UUIDv5 キーと `ON CONFLICT DO NOTHING` の上に組むため、同じ `{name, content_hash}` の claim を再送しても実体コピーと行 insert は重複しない。
同名の screenshots 行が既に存在する場合、その行が同じ hash の content-hash reuse であれば受理済みとして 2xx を返し、それ以外（本体アップロード済み、または異なる hash の reuse）は同名重複として 4xx で拒否する。
したがって CI は応答喪失時に同じ claim をそのまま再送してよく、再送が build を壊す経路はない。

処理証跡は「今回比較して差分がなかった」（`unchanged`）とは区別し、`reused_by_content_hash` として集計する。
build には `reused_by_content_hash_count` を追加し、既存の `content_hash_skipped_count` と同じ形で API へ露出する。

### 参照の形（実体コピーを採り、storage key 共有と参照テーブルを退ける）

reuse claim の結果は、carry-forward と同じサーバー側の実体コピーとして保存する。

storage key の共有や参照テーブルを採らない理由は寿命管理にある。
retention（`prune_old_builds`）は「build のオブジェクトは build と共に死ぬ」前提で build 単位に削除しており、build をまたいでキーを共有すると、参照カウント相当の寿命管理を新設しない限り、参照側が生きているのに実体が消える。
carry-forward が参照ではなく物理複製を選んだ理由として、`apps/backend/crates/job/src/compare_build.rs` の `materialize_carry_forward` に同じことが明文化されている。
また、Reuse が実体コピーであるという前提には manifest 系の承認ガードと統合テストが依存している（PR #8）。
本設計の見積もりでは、ストレージ削減はこの前提を作り直すほどの額にならない。

### 寿命

コピー完了後は build がオブジェクトを所有するため、参照元 build が retention で消えても影響を受けない。

plan 応答から claim 処理までの間に baseline が別の build へ前進しても、旧 source build が prune されることはない。
retention（`builds.rs` の `prune_old_builds`）の保護集合は、プロジェクトの baselines 全行の `source_build_id` から作られ、最新の baseline だけを保護するのではないからである。
baseline は昇格のたびに新しい行を insert するだけで、旧行の update / delete は存在しない（`builds.rs` の承認トランザクション）。
したがって plan が参照した baseline 行が生きている限り、その source build は保護され続け、baseline の前進で保護が外れる窓は開かない。
それでも claim 処理は実体読みの失敗（retention とは無関係な、storage 側の欠損・障害等）に備え、失敗を 5xx として返す。
黙って成功する経路はなく、CI は本体送信で継続できる（fail-closed）。

## 「送らなかった」と「送り損ねた」の区別

既存の plan 束縛（selected と uploaded の完全一致を finalize で検査する仕組み）と同じ強度を維持する。

hash 照合だけではこの強度に足りない。
hash は「内容が同じである」ことしか言えず、「CI がその story を確かに処理した」ことを言えないからである。
CI が途中でクラッシュして一部の story を送り損ねた場合、hash 照合にはその欠落が現れない。

区別は明示の宣言で行う。
selected の各 name は、「本体アップロードの成功」か「reuse claim の受理」のどちらかで消し込まれなければならない。
finalize は uploaded と reused の和集合が selected と完全一致することを検査し、欠けや余りがあれば 400 で失敗させ、不一致の name を列挙する。
claim を送らずに省略した story は「送り損ねた」として検出される。

比較ジョブ側にも同じ検査の多重防御がある（`compare_build.rs` の selected 照合）。
そこでは carry-forward 複製を `is_reused` で除外している。
`is_reused` は列や専用フラグではなく、screenshots 行の `metadata.reused == true` を読む関数である（`compare_build.rs`）。
carry-forward の insert が metadata に `reused: true` を書き、この関数がそれを拾う。
content-hash reuse の shot はこの除外に加えず、selected の充足として照合に含める。
finalize が「uploaded と reused の和集合 = selected」を検査する以上、content-hash reuse は selected の一部である。
これを比較側の照合から除外すると、finalize では selected の充足に数えた shot を比較側では数えないことになり、二つの検査が同じ集合を見なくなる。
したがって content-hash reuse の行には metadata に `reused: true` を書かず、`is_reused` が真にならない形で insert する。
content-hash reuse の行が metadata に持つのは `reused_by_content_hash: true` だけであり、`reused` キー自体を持たない。
識別は `reused_by_content_hash` で行う。

carry-forward と content-hash reuse で扱いが分かれる理由は、selected に対する立場の違いにある。

- carry-forward は「撮らなかった」story の複製である。①で撮影対象から外れた story は selected に含まれず、CI は何も送らない。サーバーが比較の連続性のために複製するだけであり、selected の充足とは無関係だから、照合から除外する。
- content-hash reuse は「撮ったが送らなかった」story である。story は selected に含まれ、CI が撮影し、claim で消し込む。selected の充足そのものであるから、照合に含める。

## content hash 決定スキップ（PR #11）の三担保への影響

②が壊してはならない既存の担保は三つある。
依存箇所と、本設計で壊れない理由を個別に示す。

### verified marker（昇格時の実体健全性）

承認 preflight は全 shot について storage の実体を再読し、hash を照合し、full decode の成功を確認してから marker を書く（`apps/backend/crates/service/src/screenshots.rs` の `verify_baseline_candidate`、`apps/backend/crates/service/src/builds.rs` の承認経路）。

reuse された shot も build 所有の実体コピーを持つため、この経路は変更なしで機能する。
参照方式であれば「実体を読む」対象が他 build のオブジェクトになり、寿命問題が preflight に波及したが、コピー方式ではその問題自体が生じない。

### NULL hash が承認へ届く経路の封鎖

承認は `verified_by_shot` に欠けがあれば失敗し、baseline_entries の hash 列には必ず値を書く（`builds.rs` の承認トランザクション）。

reuse された shot の `content_hash` は、サーバーがコピーしたバイト列から再計算して保存する。
これは carry-forward が baseline の NULL を継承せず再計算するのと同型である。
CI が申告した hash は照合にだけ使い、保存しない。
また hash が NULL の entry は一覧に載せないため、NULL は reuse の入口にも入らない。
したがって②を経由して NULL または未検証の hash が承認へ届く経路はない。

### carry-forward が実体コピーであるという前提

この前提に依存する箇所は次のとおりである。

- `compare_build.rs` の `materialize_carry_forward`：物理複製を選んだ理由の明文。
- `builds.rs` の `prune_old_builds`：retention の保護対象が `baselines.source_build_id` だけであること。
- `apps/backend/crates/service/src/screenshots.rs` のモジュール doc：storage key が build に閉じ、例外は baseline 昇格時の参照だけであること。
- `compare_build.rs` の selected 照合：`is_reused` による複製の除外。

②は同じ実体コピー機構を使うため、この前提は変わらない。
selected 照合の除外条件も変えない。
除外するのは carry-forward だけであり、content-hash reuse は selected の充足として照合に含める（「『送らなかった』と『送り損ねた』の区別」のとおり）。

## 設計段階で列挙する、結果を隠しうる経路

| 経路 | 設計の倒れ方 |
| --- | --- |
| sha256 の衝突 | サーバーは自身の baseline バイト列をコピーするため、衝突が成立しても保存されるのはサーバーが検証済みの画像であり、画像の置換は起きない。倒れ方は「真に変化した画像が変化なし扱いになる」見逃しだが、sha256 の衝突生成を要するため実務上は無視できる。 |
| 照会から claim までの間に元画像が消える | retention では起きない（保護集合は baselines 全行の `source_build_id` であり、baseline は insert-only。前節「寿命」）。storage 側の欠損等で起きた場合は claim 処理の実体読みが失敗し、claim は 5xx で拒否される。黙って成功する経路はない。CI は本体送信で継続する。 |
| 参照先が retention で消える | claim 受理前は保護集合が防ぎ、claim 受理後は build 所有の実体コピーがあるため、いずれも影響を受けない（前節「寿命」）。 |
| CI が hash を偽る | サーバーは hash を照合にしか使わず、実体は自身の baseline からコピーし、保存する hash は再計算する。偽の hash が baseline hash とたまたま一致する形で成立するのは「実際には変化した画面を、CI が変化なしと主張する」見逃しであり、これは CI が撮影自体を偽装できるという既存の信頼境界と同じ級に収まる。監査のため claim の hash と参照元は metadata に残す。 |

## 実装の前提条件

- 本文書のレビュー承認を得る。
- 「再検討の指標」の実測値（一致率の分母・分子、一致バイト総量）で転送削減量を再評価する。一致率がゼロに近ければ実装を見送る。
- 実装時は、reuse claim の統合テストに少なくとも「hash 不一致 claim の拒否」「NULL hash entry の一覧除外」「claim なし省略の finalize 検出」「claim 受理後の retention 耐性」「claim 再送の冪等受理」を含める。
