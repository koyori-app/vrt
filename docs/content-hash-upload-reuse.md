# 「送らない」最適化の要否見積もりと設計

本文書は、`docs/saas-change-detection-and-performance.md` が②と番号づけた「送らない」最適化について、要否の見積もりと設計を定める。
本文書の段階では設計のみを行い、実装は含めない。

**「送らない」最適化**とは、CI が撮影した PNG のうち、サーバーが保持する baseline と内容が一致するものについて、画像本体の送信を省く仕組みである。
①（撮らない）と③（比較しない）は導入済みであり、②が削減できるのは転送量に限られる。
ビルド時間には効かない。
アップロードは並列であり、時間の支配項は decode だからである。

## 結論：実装見送り

②は現時点では実装しない。
設計は本文書のとおり固め、文書として残す。

見送りの理由は次のとおりである。

- 削減できるのは転送量のみで、見積もりでも月およそ 2.8 GB にとどまる。
- ストレージ削減はゼロである（実体コピー方式のため、新規オブジェクトは送信した場合と同じ量だけ増える）。
- ビルド時間の削減もゼロである（アップロードは並列であり、時間の支配項は decode）。
- この転送削減の見積もり自体が、未実証の仮定（とくにバイト一致率）に全面依存する。

再検討の条件は、`content_hash_skipped_count` の実運用データで PNG のバイト列一致率が確認できたときとする。
一致率がゼロに近ければ、②のヒット率もゼロであり、見送りのままとする。

見積もりの前提とした仮定は次のとおりである（詳細は「仮定と削減量」）。

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
③の fast path のヒット数を数える `content_hash_skipped_count` がこの一致率の直接の観測値になるが、この計測にもまだ実運用データがない。

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

### 割に合うかの判断

self-host 構成では月 2.8 GB の転送削減は小さい。
意味を持つのは SaaS 化後で、ユーザー側 CI の帯域と、サーバー側の受信処理を減らせる。
一方で効果はバイト一致率に全面依存するため、実装の着手判断は `content_hash_skipped_count` の実測を待つ（冒頭の結論のとおり）。

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

1. pin 済み baseline の該当 entry を引く。entry がない、hash が一致しない、または hash が NULL なら claim を 4xx で拒否する。
2. entry の storage 実体を読み、バイト列から hash を再計算し、claim の hash と一致することを確認する。
3. carry-forward と同じ機構（決定的 UUIDv5 キー、upload、`ON CONFLICT DO NOTHING`、失敗時の補償削除）で、build 所有の storage key へ実体コピーする。
4. screenshots 行を insert する。`content_hash` はサーバーが再計算した値を保存する。metadata には `reused_by_content_hash: true`、claim された hash、参照元の baseline entry を記録する。

どの段階の失敗も claim の拒否（4xx/5xx）として返す。
CI は拒否されたら本体送信へフォールバックするか、build を失敗させる。
claim の失敗を黙って unchanged 扱いにする経路は作らない。

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

危険窓は plan 応答から claim 処理までの間に限られる。
この間に baseline が別の build へ前進すると、旧 source build の retention 保護（`baselines.source_build_id` による保護）が外れ、prune されうる。
その場合は claim 処理の実体読みが失敗し、claim は 5xx で拒否される。
黙って成功する経路はなく、CI は本体送信で継続できる。

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
そこでは carry-forward 複製を `is_reused` で除外しているが、この除外条件に `reused_by_content_hash` を加えて維持する。
これは「明示に宣言された shot だけを除外する」という条件の追加であり、検査の緩和ではない。

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
変更が必要なのは selected 照合の除外条件へ `reused_by_content_hash` を加える一点だけであり、前節で述べたとおり検査の緩和ではない。

## 設計段階で列挙する、結果を隠しうる経路

| 経路 | 設計の倒れ方 |
| --- | --- |
| sha256 の衝突 | サーバーは自身の baseline バイト列をコピーするため、衝突が成立しても保存されるのはサーバーが検証済みの画像であり、画像の置換は起きない。倒れ方は「真に変化した画像が変化なし扱いになる」見逃しだが、sha256 の衝突生成を要するため実務上は無視できる。 |
| 照会から claim までの間に元画像が消える | claim 処理の実体読みが失敗し、claim は 5xx で拒否される。黙って成功する経路はない。CI は本体送信で継続する。 |
| 参照先が retention で消える | 実体コピー方式のため、claim 受理後は影響を受けない。危険窓は claim 処理の内部に閉じる（前節「寿命」）。 |
| CI が hash を偽る | サーバーは hash を照合にしか使わず、実体は自身の baseline からコピーし、保存する hash は再計算する。偽の hash が baseline hash とたまたま一致する形で成立するのは「実際には変化した画面を、CI が変化なしと主張する」見逃しであり、これは CI が撮影自体を偽装できるという既存の信頼境界と同じ級に収まる。監査のため claim の hash と参照元は metadata に残す。 |

## 実装の前提条件

- 本文書のレビュー承認を得る。
- `content_hash_skipped_count` の実運用データでバイト一致率を確認する。一致率がゼロに近ければ実装を見送る。
- 実装時は、reuse claim の統合テストに少なくとも「hash 不一致 claim の拒否」「NULL hash entry の一覧除外」「claim なし省略の finalize 検出」「claim 受理後の retention 耐性」を含める。
