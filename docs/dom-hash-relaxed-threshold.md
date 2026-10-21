# DOM と CSSOM の一致で画素の閾値を緩める仕組みの設計

本文書は、描画した DOM と CSSOM が baseline と一致したときに限り、画素の比較の閾値を緩める仕組みの設計を定める。
本文書の段階では設計のみを行い、実装は含めない。

## 結論

次の形で設計する。

- 撮影した瞬間の DOM と CSSOM を、正規化したうえで別々に hash にする。
- CSSOM は、全 stylesheet の全規則ではなく、その story の DOM に当たる規則だけを hash にする。
  全規則で hash を取ると、実物の Storybook で、別の commit の間に 260 本中 0 本しか一致しなかったためである。
- 直しは vrt の側だけで閉じる。
  利用者の repo の code、CSS、設定には手を入れさせない。
- 比較ジョブは、従来どおりの比較で「差分あり」になった組に限って、二つの hash の一致を見る。
- 二つとも一致し、撮影条件も同じなら、緩めた閾値でもう一度画素を比べる。
- 緩めた側にも、落とす上限を残す。
  「一致したら素通し」にはしない。
- 設定は既存の `diff_threshold` と `diff_ratio_fail` とは別の欄に置く。
  既定は「無効」で、未設定なら今までと寸分違わぬ振る舞いになる。
- 緩めたことは、比較の記録、一覧、詳細の各画面に必ず出す。

この仕組みが狙うのは、DOM も CSS も変わっていないのに画素だけが動く差である。
典型は、撮影環境の更新（Chromium や font の描画の変化）で、全 story に細かな差が出る場面である。
逆に、DOM にも CSSOM にも現れない変化（font ファイルの差し替え、外部の画像、canvas の中身）は、hash が一致したまま画素が大きく動く。
その差を見逃さないために、上限を設計の中心に置く。

## この文書の前提

### 引く原文について

設計の主張は、先行調査で読んだ公開の資料の引用で裏づける。
引用できる資料が無い主張は、「調べた範囲では裏が無い」と書く。

引用は、公開の URL と取得の日で示す。
元の頁は書き換わりうるので、引用の字は取得の日の時点のまま残す。
引用は、論を支えるのに要る短い一節にとどめ、元の文書は写さない。
引く資料は次のとおりである。
取得の日は、いずれも 2026-10-21 である。

| 略称 | 題名（出した者） | URL |
| --- | --- | --- |
| W3C 7171 | 「[cssom] Add `toString` method on `CSSStyleSheet`」（W3C の CSS Working Group の issue） | https://github.com/w3c/csswg-drafts/issues/7171 |
| Argos | 「How to fix flaky visual tests: every root cause, solved」（Argos のブログ） | https://argos-ci.com/blog/fix-flaky-visual-tests |
| Playwright | 「SnapshotAssertions」（Playwright の公式文書） | https://playwright.dev/docs/api/class-snapshotassertions |
| jest-image-snapshot | 「Jest matcher for image comparisons」の README（jest-image-snapshot の保守者） | https://github.com/kolodny/jest-image-snapshot |
| Sauce | 「Finding Root Cause Analysis in Your Visual Regressions with Hybrid Diff」（Sauce Labs のブログ） | https://saucelabs.com/blog/how-to-find-root-cause-analysis-hybrid-diff |
| Autify | 「Causes and solutions for failures in the Element should look the same step」（Autify のヘルプ） | https://help.autify.com/docs/causes-and-solutions-for-vrt-failures |

### Chromatic の画面の注記について

Chromatic は、比較の画面に DOM の差分を出し、その横の説明に次の注記を置いている。
この字は、画面を撮った画像から読み取ったもので、原文のファイルは無い。
調べた範囲では、公式の文書の側にこの文は見つかっていない。

> Chromatic does not use DOM diffs to detect visual changes. These are not Jest-based virtual DOM tests. Changes from upstream CSS or 3rd-party APIs will not be shown here.

同じ画像には、DOM の差分の欄がある。
左右の対で、先頭の `div` の `data-v-` に続く文字が `58…` と `a9…` で異なり、その行を含む塊と、図形（`svg`）の内側の八行が差分として赤と緑で並んでいる。
つまり、build ごとに変わる scoped style の印が、そのまま差分として出ている。
この見え方は、正規化なしに DOM を比べると、毎回「不一致」になることを示す実例である。

この注記が意味するのは、次の二点である。

- DOM の差分は、視覚の変化の検出には使われていない。
- DOM の外の原因（上流の CSS、外部の API）による変化は、DOM の差分に出ない。

後者は、本設計が DOM だけでは足りないと判断する直接の根拠になる（「何を hash に入れるか」）。

### 既存の実装との関係

現在の実装の事実を、以下の文書と実装から確かめた。

- 比較ジョブは、byte 列が一致する組を hash（`content_hash`）で素通しし、それ以外を `projects.diff_threshold` と `projects.diff_ratio_fail` で判定する（`apps/backend/crates/job/src/compare_build.rs`）。
- `diff_threshold` は画素ごとの許容、`diff_ratio_fail` は差分の割合がそれを超えたら `changed` にする線である。
  既定は 0.1 と 0.0 である（初期スキーマの `projects`）。
- DOM の字は、どこにも保存していない。
  `screenshots` の列は `storage_key`、`width`、`height`、`metadata`、`content_hash` で、`metadata` には story の ID と title が入る。
- 撮影は、`storybook` モードでは VRT 側の runner が Chromium で行い（`docs/architecture.md` の「レンダリングジョブ」）、`screenshots` モードでは CI が行う。

### 利用者の repo に手を入れさせない

本設計は、利用者の repo に手を入れさせる形にしない。
利用者の component、CSS、Storybook の設定、試験の code を直さないと働かない仕組みは、採らない。
不一致の原因が利用者の repo の書き方にあっても、直すのは vrt の側である。

実測で出た不一致は、どれも vrt の側で解く。

- 一枚の大きな stylesheet（utility の CSS）で全 story の hash が動く問題は、使われている規則だけを選ぶ処理で解く。
  利用者に CSS を割らせない。
- build ごとに変わる印と名前（scoped style の印、`v-bind()` の名、自動で生える id）は、正規化の規則で畳む。
  利用者に build の設定を変えさせない。
- 乱数の id は、特定の部品の名を決め打つ規則にしない。
  一般の形で畳めるかを考える。
  畳めない物は、畳めないと書く。
- 試料が少ない、測れていない、という不足は、vrt の側の測りの不足である。
  利用者の repo を直す話にはしない。

story の側で時計や乱数を固定すれば、不一致が減る場合がある（「落とすもの、残すもの」の節の終わり）。
これは勧めであり、仕組みが働く条件にはしない。

### 実測について

実測の結果は、`docs/dom-hash-normalization-measurement.md` にある。
この文書は、本設計とは別の PR（#61）で入るので、本設計の PR の時点では、main にまだ無い。
本設計が引く数は、その文書の数である。

測ったのは、利用側の Storybook（story 267 本）と、公開の arc-vue の Storybook（story 5 本）である。
撮り方と試料は、その文書に書いてある。

## 先行が無いことについて

英語と日本語の検索役が独立に探し、DOM の一致を根拠に画素の閾値を変える道具は、どちらも見つけられなかった（先行調査の結果であり、公開の資料ではないので引用は付けない）。
この調査は、話題ごとに一度ずつ探したもので、網羅ではない。

最も近い道具は Sauce Visual の Smart Hybrid Diff である。
画面と DOM の写しを同時に撮るが、その目的は DOM の構造のどこが変わったかを見せることで、閾値の調整には使わない（Sauce、「you will also be able to fully understand what specifically in the DOM structure has changed」）。

閾値を二段に分ける形そのものには先行がある。
Playwright は、画素ごとの許容の `threshold` と、数や割合の許容の `maxDiffPixels` と `maxDiffPixelRatio` を別に持つ（Playwright、`maxDiffPixelRatio` は「An acceptable ratio of pixels that are different to the total amount of pixels, between 0 and 1」、`threshold` は「An acceptable perceived color difference in the YIQ color space between the same pixel in compared images」と説明されている）。
`threshold` は「どの画素を違うと数えるか」を決め、後者は「違う画素がどれだけあれば落とすか」を決める（Playwright、「threshold defines which pixels are considered different」）。
jest-image-snapshot も、画素ごとの `customDiffConfig.threshold` と、画像全体の `failureThreshold` を分けている（jest-image-snapshot、「the threshold set in the customDiffConfig is the per pixel sensitivity threshold」と「this is the failure threshold for the entire comparison」）。
ただし、どちらの閾値も DOM とは関係なく、利用者が固定の値で与える。

hash を使う考え自体は、W3C の CSS WG に提案があった。
投稿者は、DOM と CSSOM をそれぞれ直列化して hash を取り、screenshot の隣に保存すれば、hash が一致しない場合にだけ CI が screenshot を更新できる、と書いている（W3C 7171、「serialize the entire DOM and CSSOM and get hashes of each」）。
狙いは、OS や browser の違いによる描画の差で画像が合わなくなる問題である（同じ投稿の「subtle differences in rendering, especially fonts」）。
この提案は `CSSStyleSheet.toString()` を足す issue の中の一つの使い道として挙がったもので、調べた範囲では実装した道具は見つかっていない。

先行が無いことには、二通りの読み方がある。
誰も思いつかなかったか、誰かが試して捨てたかである。
本設計は後者を前提にして、捨てられる筋を先に挙げる（「捨てられる筋と防ぎ方」）。

## 何を hash に入れるか

### DOM と CSSOM を別々の hash にする

DOM だけでは足りない。
Chromatic の注記が言うとおり、DOM の差分には上流の CSS の変化が出ない。
ある component の CSS だけが変わり、DOM は一字も変わらない変更は、実際によくある。
DOM の hash だけを根拠に緩めると、この種の変更が見逃しの入口になる。

したがって、CSSOM を hash に入れる。
W3C の提案も、DOM と CSSOM の両方を対象にしている（W3C 7171 の同じ投稿）。

hash は一つにまとめず、二つの値（`dom_hash` と `cssom_hash`）にする。
別々に持てば、不一致のとき「DOM が違う」「CSSOM が違う」を利用者に言えるからである。
緩める条件は、二つとも一致することである。

### CSSOM の範囲

CSSOM は、その story の DOM に当たる規則だけを、文書の順に直列化して hash にする。
全 stylesheet の全規則を hash にする形は採らない。
最初の版では全規則を考え、当たる規則だけを選ぶ方式は、選ぶ処理が複雑で、選び損ねると見逃しになるので後回しにしていた。
実測で、後回しにした選択肢のほうが正しかったと分かった。

全規則の hash は、source が違う二つの commit の間で、story 260 本のうち 0 本しか一致しなかった。
同じ組で、DOM は 249 本が一致した。
原因は、utility の CSS（Tailwind CSS）が一枚の stylesheet を出すことにある。
ある story と関係の無い場所で、utility が一つ増えるか、theme の変数が一つ増えるだけで、その一枚の字が変わり、全 story の hash が動く。
この形では、仕組みが働く機会がほとんど無い。

当たる規則だけに絞ると、同じ組で CSSOM は 250 本、DOM と合わせて 249 本が一致した。
残る 11 本のうち 10 本は、実際に画面の中身が変わった story である。
残る 1 本は、乱数の id である（「落とすもの、残すもの」の節）。
同じ source を二度 build した組では、全規則でも、絞っても、CSSOM は 267 本すべてが一致した。
全規則の hash が働かなくなるのは、source が変わったときである。
仕組みを使う場面は、source が変わった build の比較である。

選んだ規則の字が変われば、hash は動く。
その story に効く CSS の変更は、見逃さない。
見逃すのは、選び損ねた規則の変更である。

選ぶ処理は、DOM が生きているブラウザの側で行う。
DOM が無ければ、どの規則が当たるかを決められない。
したがって、この処理は段 1 の仕事である（「実装の段取り」の表）。

実測で使った近似は、次のとおりである。

- 規則の selector が、その page の `document.querySelector` に当たるかで選ぶ。
- 当たらなければ、selector から擬似要素と状態の擬似クラス（hover、focus、active など）を外して、もう一度当てる。
  外さないと、`:hover` 付きの規則を、いま hover していないという理由で落としてしまう。
- `@media`、`@supports`、`@layer` の中の規則は、中で選ぶ。
  中身が残らなければ、外側ごと落とす。
- `@keyframes`、`@property`、`@font-face` は、選んだ規則の字に名前が出る物だけ残す。
- custom property の宣言は、選んだ規則から `var()` で参照される物だけ、参照の先まで残す。

この近似が、使われている規則を落とすかどうかは、測れていない。
落とした規則の変更は、見逃す。
JS が custom property を名前で読む場合も、この近似は拾わない。

stylesheet が読めないときは、`cssom_hash` を作らない。
`cssRules` へのアクセスは、CORS で許可されていない別オリジンの stylesheet では例外になる。
この場合は「読めなかった」と記録して緩めない。
runner が配信する bundle は同一オリジンなので、通常は読める。
外部の CDN から読む font の stylesheet などが混ざる場合に、この経路に入る。

### 直列化の不確かさ

CSSOM の直列化には既知の不具合がある。
Chromium の `cssText` は、custom property を含む場合に不正な CSS を出すことがある（W3C 7171 の別の投稿、「it can produce invalid CSS when using custom properties」、Chromium の不具合の頁は https://issues.chromium.org/issues/40804066）。
同じ投稿は、`toString()` を足しても仕様上の直列化が同じなので解決しないと書いている（同じ投稿の追記、「.toString() would have the same issue since that's how it's specced to be serialized」）。

どの型の欠落かは原文に書かれておらず、「別の宣言が同じ文字列になる」（false match を生む）かどうかは、調べた範囲では裏が無い。
本設計の方針は、規則を `cssText` の一括で取らず、宣言ごとに `getPropertyValue` と `getPropertyPriority` で組み立てることである。
custom property は、名前を列挙して値を個別に取る。
この方針が、上の不具合を避けられるかは、実装の段で試験して確かめる。

### hash に出ないものを明に書く

次のものは、DOM にも CSSOM にも現れず、hash が一致したまま絵が変わる。

| 変化 | hash に出ない理由 | 原文 |
| --- | --- | --- |
| font ファイルの差し替え（URL が同じ場合） | CSSOM には URL しか入らず、ファイルの中身は入らない | Argos、「the browser renders the fallback font, and every text node on the page shifts by a few pixels」 |
| 外部の画像の差し替え（URL が同じ場合） | DOM には URL しか入らない | Argos、「they load after the screenshot (blank rectangle in one run, photo in the next)」 |
| canvas の中身 | 画素は DOM の外にある | Argos、「JavaScript-driven animations (GSAP, requestAnimationFrame loops, canvas) are not affected by CSS overrides」 |
| 擬似状態（hover、focus）、scroll の位置 | 属性にも規則にも出ない | 調べた範囲では裏が無い |
| 外部の API が返す内容の変化 | 反映された結果が DOM に出ていればよいが、画像や iframe の中身は出ない | Argos、「your screenshots inherit every instability of that API」 |
| GPU や OS、Chromium の版による描画の差 | これこそ緩めたい差である | Argos、「a GPU rendering text slightly differently」。Autify、「Minor adjustments to system fonts or changes in the rendering algorithm」 |

最後の行が、この仕組みの目的である。
他の行は、同じ「hash 一致」の中に混ざって、緩めた閾値の下で見逃される。
この見逃しを防ぐのが、上限である。

### 撮影条件を別に比べる

hash には、viewport の大きさ、`emulate_reduced_motion` の設定、runner が注入する静止用の CSS の版、正規化の版を、一つの「撮影条件」の値（`dom_hash_profile`）としてまとめて持たせる。
これらは DOM にも CSSOM にも出ないのに、絵を変える。
撮影条件が違う組では緩めない。

runner が撮影の直前に注入する静止用の stylesheet は、hash の対象から外す。
runner の版が変わるたびに全 story の `cssom_hash` が変わることを避けるためで、その版は `dom_hash_profile` の側で比べる。

Chromium や font の版は、`dom_hash_profile` に含めない。
それらの違いこそ、緩めたい差だからである。

## 正規化で何を落とすか

### 原則

正規化は、hash の不一致を減らすためのもので、緩める側に働く。
落としすぎれば、本当の変化を同じ hash に畳み込み、見逃しの入口になる。
落とさなければ、不一致が増えるだけで、従来どおりの厳しい比較になり、見逃しは増えない。
この非対称があるので、方針は次の二つである。

- 描画に影響しないと確かめられるものだけを落とす。
- 迷うものは残す。
  残した結果の不一致は、仕組みが働かないだけで済む。

正規化の規則は番号（`norm_version`）をつけ、`dom_hash_profile` に含める。
番号が違う hash は比べない。

### 落とすもの、残すもの

| 対象 | 扱い | 理由 |
| --- | --- | --- |
| Vue の scoped style の印（`data-v-` に 8 桁の 16 進が続く属性） | 落とさず、出現順の連番に置き換える | build ごとに変わる値なので、そのままでは毎回不一致になる（画像の差分に実例）。属性ごと落とすと、どの要素にどの scope の CSS が効くかの対応が消える。連番にすれば、DOM の側と CSSOM の側の属性セレクターを同じ表で置き換えて、対応を保てる |
| `data-v-app` など、16 進でない `data-v-` の属性 | 残す | build で変わらないため |
| `v-bind()` が作る custom property の名（`--` に 8 桁の 16 進、または `--v` に 8 桁の 16 進） | 出現順の連番に置き換える | build ごとに変わる。実物の Vue の試料で、source が違う二つの build の間で名前が変わった。`style` 属性と CSSOM の `var()` の両方に出るので、同じ表で置き換えて、どの規則がどの変数を読むかの対応を保つ。この規則が無いと、同じ試料で DOM も CSSOM も一本も一致しなかった |
| scoped style の `@keyframes` の名の後ろに付く印（`name-58152a41`） | 既知の印だけ、印と同じ連番に置き換える | Vue は、scoped style の中の keyframes の名に、印の 16 進を付ける。印と同じ値なので、印と同じ番号にすれば畳める。知らない 16 進は別の物かもしれないので残す。この規則が無いと、DOM は一致しても CSSOM が一本も一致しなかった |
| 自動で生える id と、それを指す属性（`id`、`for`、`aria-labelledby`、`aria-describedby`、`aria-controls`、`aria-owns`、`href` の `#` 以降） | 既知の生成規則に合う id だけ、出現順の連番に置き換える | 例えば React の `useId` が作る id は、描画の順で変わりうる。参照の関係は連番で保たれる。手で書いた id（CSS の `#main` が指すもの）は残す |
| 未知の生成規則の id | 残す。ただし、参照で結ばれた id は、次の行の規則で連番にする | 落とすと、参照の取り違えを同じ hash に畳み込む。不一致になるだけで安全である |
| 参照で結ばれた id（`for`、`aria-*`、`headers`、`list`、`form`、`popovertarget`、`commandfor`、`href` の `#` 以降が指す id と、同じ値の `id`） | 出現順の連番に置き換える。ただし、CSSOM の字に現れる id は残す | build ごとに乱数が変わる id を、部品の名を決め打たずに畳める一般の形である。実測では、ある編集部品の補完の欄が作る id（`aria-controls` と `aria-activedescendant` の値）が、build ごとに違った。id の名前が絵に効く道は、CSS が id で選ぶ場合だけである（JS が名前で読んだ結果は、描画後の DOM に出る）。参照の対応は番号で残るので、参照先の取り違えは違いとして残る |
| `script`、`noscript` の要素 | 落とす | 描画しない。src に build ごとの hash が入る |
| `link` の要素（stylesheet、preload、modulepreload） | 落とす | 中身は CSSOM に入る。href に build ごとの hash が入る |
| `style` の要素 | 落とす | 中身は CSSOM に入るため二重になる |
| コメントノード | 落とす | 描画しない。framework が入れる占位の差を拾わないため |
| `head` の中身 | 落とす | 描画に影響するのは、CSSOM に入るものと `html` の属性である。`title` や `meta` の変化は絵に出ない |
| `html` の属性（`class`、`lang`、`dir`、`style`、`data-*`） | 残す | theme の切り替えなど、絵を変える |
| `body` の下の要素と text | 残す | 描画そのもの |
| text ノードの字 | 残す | 時刻や乱数が入る story は、緩められずに従来の比較になる |
| 属性の順 | 並べ替える | 描画に影響せず、序列は不定である |
| class の語の順 | 並べ替える | 同上 |
| 絶対 URL の origin（loopback の host と port） | 固定の記号に置き換える | runner は、空いている port で bundle を配信する（`docs/architecture.md` の「レンダリングジョブ」）。URL に入る port は毎回変わる |
| `blob:` URL の識別子 | 固定の記号に置き換える | 毎回変わる |
| `data:` URL | 残す | 内容が絵を決める。長いので hash で足りる |
| 画像と font の URL のパス | 残す | build の hash がパスに入る構成では、中身の変化が不一致として出る。これは望ましい。パスだけが変わる場合は不一致になるが、安全側である |
| 入力欄の値と `checked`（property） | 属性に書き出して残す | 絵を変える。属性には出ない |
| open の shadow root | 中身を残す | 描画の一部 |
| closed の shadow root | 見えない | 限界として記録する（runner の実装の注記にある限界と同じ） |
| 同一オリジンの iframe | 中身を再帰して残す | runner が font の待ちで既にたどる範囲と揃える |

参照で結ぶ規則が見逃すのは、id の名前だけが変わり、CSS にも描画にも出ない変化である。
外の頁から `#id` で指される anchor の名前の変更も、絵には出ないので見逃す。
CSSOM の字に現れるかは、id の字の部分一致で見る。
`[id="…"]` の属性セレクターで選ぶ場合も、これで守れる。

この規則を、試作で測った。
同じ source の二度の build で 267 本すべてが一致し、別の commit で 250 本が一致した。
どちらも、特定の部品を決め打って畳んだ場合と同じ数で、落ちた一致は無かった。
この試作の数は、実測の文書にはまだ載っていない。
PR #61 の module は、既知の生成規則の版までで、参照で結ぶ版は未実装である。
段 2 の続きで入れる。

時刻と乱数は、text を正規表現で消す方式を採らない。
消せば、本当の text の変化を同じ hash に畳み込む。
時刻や乱数が絵に入る story は、時計と乱数を固定して、描画を決まったものにするのが本筋である（Argos、「anything nondeterministic in your data ends up nondeterministic in your pixels」）。
固定しない story は、不一致になるだけで、従来どおりの比較が続く。

### 撮影の瞬間の安定

DOM の直列化は、画像の撮影の直前と直後の二度行い、結果が違う場合は hash を作らない（「不安定」と記録する）。
撮影の間に DOM が動いた場合、画像と hash が別の状態を指すからである。
hash を作らなければ、その story は従来どおりの比較になる。

## 実測の結果と、測れていないこと

実測の数を、本設計の判じに関わる範囲で、短く載せる。
詳しくは、前提の節で示した実測の文書にある。

| 組 | 全規則の CSSOM | 当たる規則だけの CSSOM |
| --- | --- | --- |
| 同じ source を二度 build（story 267 本） | DOM と CSSOM が両方一致 266 本 | 同じ 266 本 |
| source が違う二つの commit（共通 story 260 本） | 両方一致 0 本（DOM は 249 本一致） | 両方一致 249 本 |

同じ source の二度の build で一致しなかった 1 本は、乱数の id である。
参照で結ぶ規則を試作で入れると、267 本すべてが一致した。
scoped style の実物（story 5 本）では、source が違う二つの build が、印、`v-bind()` の名、`@keyframes` の名の三つを畳んで、5 本すべて一致した。

次のことは、測れていない。

- DOM が同じで CSS だけが変わる story の検出。
  試料に、そのような story が無かった。
- 当たる規則の選び方の近似が、使われている規則を落とす場合。
- 閉じた shadow root、canvas の中身、画像と font のファイルの中身。
  DOM にも CSSOM にも出ないので、上限で受ける。
- 自動で生える id の、既知の生成規則の効き。
  試料に、build ごとに変わる id でその形の物が無かった。
- runner の静止の処理を通した揺れ。
  測定の撮影は、runner の静止処理を通していない。
- scoped style の試料の数。
  5 本しか測っていない。

これらは、vrt の側の測りの不足である。
利用者の repo に手を入れて埋める話ではない。

## 設定の欄

既存の `diff_threshold` と `diff_ratio_fail` は変えない。
別の欄を足し、未設定なら仕組みごと無効にする。

| 欄 | 型 | 既定値 | 意味 |
| --- | --- | --- | --- |
| `dom_match_mode` | 文字列（`off`、`shadow`、`enforce`） | `off` | `off` は撮影も比較も今までどおり。`shadow` は hash を取り、緩めた場合の結果を記録するが、判定は変えない。`enforce` は判定を変える |
| `dom_match_diff_threshold` | 小数、null 可 | null | hash が一致した組で使う、画素ごとの許容。null なら `diff_threshold` と同じ |
| `dom_match_diff_ratio_fail` | 小数、null 可 | null | hash が一致した組で使う、落とす割合。null なら `diff_ratio_fail` と同じ |

検証の規則は次のとおりである。

- `dom_match_mode` が `off` 以外のとき、`dom_match_diff_threshold` と `dom_match_diff_ratio_fail` のうち、既存の値と違う値を持つものが一つは要る。
  緩める値が無いのに有効にすることは、断る。
- `dom_match_diff_threshold` は、`diff_threshold` 以上かつ 0.5 以下とする。
- `dom_match_diff_ratio_fail` は、`diff_ratio_fail` 以上かつ 0.05 以下とする。
- 0.5 と 0.05 は仮の上限で、実測のあとで決め直す。
  上限の根拠は、Argos が画素ごとの閾値の既定を 0.5 にしており、それが anti-aliasing の雑音を吸うためだとしていること（Argos、「with a 0.5 default tuned to absorb anti-aliasing noise」）にある。
  ratio の 0.05 は、根拠となる原文が調べた範囲に無い。
- 上限を超える値と、1.0 は受け付けない。
  1.0 は「素通し」と同じだからである。

この三つの欄は、既存の `emulate_reduced_motion`（`m20260811000000_reduced_motion.rs`）と同じく、`projects` の列として足し、既存の行の既定が `off` になるようにする。
`dom_match_mode` が `off` のプロジェクトでは、runner は DOM の直列化を行わず、比較ジョブは hash の列を読まない。
このとき、判定の経路は今のまま一字も変わらない。

欄を二段に分ける理由は、緩める軸が二つあることにある。
画素ごとの許容（`dom_match_diff_threshold`）は、細かな色の差を「違う」と数えない働きで、AA の雑音に効く。
落とす割合（`dom_match_diff_ratio_fail`）は、数の許容で、広く散った小さな差に効く。
既定を null にして継承させておけば、利用者は片方だけを緩められる。
本設計は、割合より画素ごとの許容を先に緩めることを勧める。
割合の上限は、画面が大きいほど小さな局所の変化を呑む。
たとえば 1280 × 720 の画像の 0.5% は約 4600 画素で、32 × 32 の icon の差し替え（約 1000 画素）より大きい。
色の差を見る画素ごとの許容は、icon の差し替えのような大きな色の差を、緩めた後でも差として数える。

## 緩めても上限を残す

### 判定の流れ

比較ジョブは、次の順で判定する。

1. byte が一致する組は、今までどおり素通しする。
2. 従来の閾値で比べる。
   `unchanged` なら、そこで終わる。
3. `changed` になり、`dom_match_mode` が `off` 以外で、`dom_hash`、`cssom_hash`、`dom_hash_profile` が両側にあり、すべて一致するときだけ、緩めた閾値でもう一度比べる。
4. 緩めた閾値の結果が `dom_match_diff_ratio_fail`（null なら `diff_ratio_fail`）以下なら、緩めた判定を使う。
   超えたら `changed` のままにする。
5. `shadow` では、緩めた結果を記録するだけで、判定は従来のままにする。
   `enforce` では、緩めた判定を使う。

3 の再比較は、従来の比較で `changed` になった組にしか走らない。
計算が増えるのは、差が出た組だけである。

### 上限が働く訳

hash が合っても、画素が大きく動いたなら、CSS か font か外の資源が変わったということである。
それこそ、見たい差である。
「一致したら素通し」にすると、前の節の表の見逃しがすべて通る。

本設計では、緩めた側にも落とす線が必ず残る。
次の三つが、素通しにならない構造である。

- 緩めた閾値の下でも、色の差が大きい画素は数える。
- 数えた割合が、緩めた側の `dom_match_diff_ratio_fail` を超えたら、`changed` にする。
  継承して null のときは、既存の `diff_ratio_fail` が線になる。
  既定の 0.0 なら、緩めた閾値を超える画素が一つでもあれば `changed` になる。
- 設定値には上限があり、素通しになる値は設定できない。

Argos も、同じ危険を指摘している。
全体に効く大きな許容は、font の入れ替わりを呑むのと同時に、CSS の一行の本当の退行も呑む（Argos、「A global tolerance high enough to absorb a font swap or a mid-animation frame will also absorb real one-line CSS regressions」）。
Argos が勧めるのは、安定させることを先にして、許容は画像ごとに絞って使うことである（同じ記事の「Stabilize the page first, capture in one canonical environment, then apply tolerance surgically per screenshot rather than globally」）。
本設計の緩めは、hash が一致した組だけに、かつ上限つきで使うので、この勧めに沿う。

### 緩めた時の絵の扱い

緩めて `unchanged` にした組でも、従来の閾値での差分の画像は保存する。
利用者が、緩める前に何が差だったかを見られるようにするためである。
保存の量は、緩めた組の数に比例し、差分があった組だけである。

## どこで撮り、どれだけ溜まるか

### 撮る口

DOM と CSSOM の直列化は、`storybook` モードの runner の撮影の中で行う。
撮る位置は、font の待ち、静止、撮影直前の再確認が済んだあと、画像を撮る直前と直後である（前の節の「撮影の瞬間の安定」）。
同じ瞬間に、同じ page から取るので、画像と hash が同じ状態を指す。
CSSOM は、DOM が生きているこの口で、その story に当たる規則だけを選んでから直列化する（「CSSOM の範囲」）。

`screenshots` モードでは、撮影を CI が行うので、DOM は VRT に届かない。
CI 側の SDK や CLI が DOM の hash を送る経路は、この設計の対象外である。
hash が無い組は、緩めずに従来の比較になる。
この限界は、設定画面にも書く。

### 保存の欄

hash は、`screenshots` と `baseline_entries` の両方に、列として持つ。
`content_hash` と同じ置き方である（`m20260803000000_content_hash_decode_skip.rs`）。

| 欄 | 置き場 | 内容 |
| --- | --- | --- |
| `dom_hash` | `screenshots`、`baseline_entries` | 正規化した DOM の hash（hex） |
| `cssom_hash` | 同上 | 正規化した CSSOM の hash（hex） |
| `dom_hash_profile` | 同上 | 撮影条件の値（短い hash） |

baseline を作る処理（`apps/backend/crates/service/src/baselines.rs`）は、`content_hash` と同じ場所で、この三つの欄も写す。
写し忘れは、「baseline に hash が無い」になり、従来の比較になる。
安全側の失敗である。

比較の記録（`comparisons`）には、次の欄を足す。

| 欄 | 内容 |
| --- | --- |
| `dom_match_state` | `relaxed`（緩めて通した）、`capped`（hash は一致したが緩めても上限を超えた）、`dom_differs`、`cssom_differs`、`profile_differs`、`no_hash`、`unreadable`、`unstable`、`not_needed`（従来の比較で通った）、`disabled` のどれか |
| `dom_match_strict_ratio` | 従来の閾値での差分の割合 |
| `dom_match_relaxed_ratio` | 緩めた閾値での差分の割合 |

ビルド（`builds`）には、緩めた比較の数（`dom_match_relaxed_count`）を足す。
`content_hash_skipped_count` と同じ置き方である。

### 量の見積もり

story 262 本、build の頻度は月 660 を仮定する（`docs/content-hash-upload-reuse.md` の仮定を借りる）。
次の値は、実測ではなく仮定である。

| 項目 | 仮定 |
| --- | --- |
| story の数 | 262 |
| 正規化した DOM の大きさ | 1 story あたり 20 KB |
| CSSOM の直列化の大きさ | 150 KB（全 story で同じ内容） |
| 圧縮率 | 5 分の 1 |
| PNG 1 枚 | 200 KB |
| build の頻度 | 月 660 |

hash だけを持つ場合は、1 story あたり、hex 64 字を二つと、短い値で 150 byte ほどである。
1 build あたり約 40 KB、月で約 26 MB になる。
比較にならぬほど小さい。

本文も持つ場合は、DOM の本文が 1 build あたり、262 × 20 KB で約 5 MB（圧縮して約 1 MB）、月で約 3.4 GB（圧縮して約 0.7 GB）である。
PNG は 262 × 200 KB で約 52 MB、月で約 34 GB と見積もられるので、本文の圧縮後はその 2% 程度である。
圧縮しなくても 10% 程度に収まる。

本文を、hash を名前にした内容アドレスで置けば、増えるのは「内容が変わった story」だけになる。
ほとんどの story は build ごとに変わらないので、実際の増加は上の見積もりよりずっと小さい。
CSSOM の本文も、全 story で同じなので、1 build に一つだけである。
ただし、内容アドレスの置き方には、参照されなくなった本文を掃除する処理が要る。
掃除の規則は、build の保持数の仕組み（`m20260730000000_build_retention_limit.rs`）と、baseline が指す本文を落とさない配慮を合わせて決める必要があり、この設計では決めない。

### 選び

hash だけを先に持つ。
理由は三つある。

- 判定に要るのは hash の一致だけで、本文は要らない。
- 本文は、画面に差分を見せるためだけに要る。
  見せる画面は、判定の仕組みが働くと確かめたあとでよい。
- hash だけなら量が小さく、保持の掃除を設計に入れずに済む。

本文は、画面の段で、内容アドレスで足す。
DOM の本文は baseline の側の分も要るので、baseline が指す本文は掃除の対象から外す。

## 画面で見せる

緩めたことが見えなければ、見逃しに気づけない。
次の四つを出す。

### 一覧

- 緩めて通した比較に、「DOM と CSSOM が一致したので緩和」と読める印をつける。
  `unchanged` のなかで、普通に一致したものとは別に数える。
- 絞り込みに「緩和された比較」を足す。

### 詳細

- 判定の根拠の欄を足す。
  DOM の一致、CSSOM の一致、撮影条件の一致のそれぞれの結果と、従来の閾値での差分の割合、緩めた閾値での差分の割合、上限の値を並べる。
- 不一致のときは、どれが違ったか（`dom_differs`、`cssom_differs`、`profile_differs`、`unreadable`、`unstable`）を、利用者が読める言葉で出す。
  仕組みが働かなかった理由が見えれば、正規化の漏れに気づける。
- 従来の閾値での差分の画像を、緩めたあとでも見られるようにする。

### DOM の差分

Chromatic の画面のように、正規化した DOM の本文の左右の差分を表示する案である。
本文を持つ段で足す。
正規化の後の字を見せるので、`data-v-` の違いは差として出ない。
CSSOM の差分も同じ形で出せるが、全規則を直列化した文字列は長く、差分の画面は規則の単位に折り畳む必要がある。

### ビルドの要約

- 緩めた比較の数を、ビルドの結果の要約に出す。
- GitHub の commit status の説明の文にも、緩めた数を載せる。
  `passed` の中の何件が緩めて通ったかが、PR の側から見える。

`enforce` では、緩めた比較は `unchanged` になり、レビューの対象から外れる。
緩めた比較にも人のレビューを要求する選び（`needs_review` の扱い）は、この設計では決めず、「決めておくべきこと」に残す。

## 捨てられる筋と防ぎ方

先行が無い理由が「誰かが試して捨てた」だとすれば、捨てる筋は見逃しが出て信を失うことである。
Argos は、誤検出が積もると利用者が見ずに承認するようになり、本当の退行がすり抜けると書いている（Argos、「Each false positive trains reviewers to click "approve" without looking」）。
緩めた仕組みでも、同じことが逆向きに起きる。
見逃しが一度出れば、利用者は緩めた判定全体を信じなくなる。

| 捨てられる筋 | 防ぎ方 |
| --- | --- |
| 見逃しが出て、仕組みへの信が落ちる | 上限を必ず残す。`shadow` の期間を置き、実際の判定を変える前に、緩めた結果とレビューの結果を突き合わせる。画面で、緩めたことを必ず見せる |
| font や外部の画像が同じ URL のまま変わる | 上限で受ける。将来の手当てとして、runner が読み込んだ font と画像の応答の hash を `dom_hash_profile` と並べて取る案を残す（CDP の network の応答から取れるが、本設計では検証していない） |
| 正規化が強すぎて、本当の変化を同じ hash に畳む | 迷うものは残す原則。連番の置き換えで、対応を保つ。正規化の規則に番号をつけ、規則ごとの試験で縛る |
| 正規化が弱すぎて、毎回不一致になり、仕組みが働かない | 不一致の理由を、一覧と詳細に出す。`shadow` の期間に、不一致の理由の内訳を数える。`data-v-` の例のように、実例で規則を足す |
| CSSOM の直列化が違いを潰す（Chromium の不具合） | 宣言ごとに組む。実装の段で、規則を変えたとき hash が変わることを試験する |
| 当たる規則の選び損ねで、CSS の変更を見逃す | 選んだ規則の字が変われば hash は動く。選び方の近似の精度は、まだ測れていない。`shadow` の期間に、人が却下した比較で、選び損ねが原因かを調べる。原因の型が分かれば、選び方を直す（vrt の側） |
| 利用者の repo の書き方で不一致が続き、仕組みが働かない | 利用者に直させない。不一致の理由を画面に出し、実例が出たら vrt の正規化と選び方の規則で解く |
| 設定値が少しずつ緩められ、上限に張り付く | 設定値に上限を置く。緩めた比較の数を、ビルドと日ごとに見られるようにする |
| 緩めた判定が `unchanged` に隠れて見えない | 「画面で見せる」の四つ。緩めた数を要約に出す |
| 正規化の規則を変えたとき、古い hash と合わなくなる | `norm_version` を `dom_hash_profile` に含める。版が違えば比べず、従来の比較になる |

### 緩めて良いかを `shadow` で確かめる

`enforce` へ上げる前に、`shadow` で次を見る。

- 「緩めれば通る」と記録された比較のうち、あとで人が却下（`rejected`）したものの数。
  これが一件でもあれば、見逃しの例であり、その原因を調べるまで `enforce` へ上げない。
- 「緩めれば通る」比較のうち、人が承認したものの数。
  承認は、意図した変更だった場合も含むので、見逃しの無さの証明にはならない。
- hash が不一致だった比較の、不一致の理由の内訳。

この基準の数値（何 build を見るか）は、実測の前には決められない。

## 実装の段取り

実装は、次の段に分ける。
各段は、前の段の上に積み、段ごとに戻せるかを書く。

| 段 | 内容 | 判定への影響 | 戻せるか |
| --- | --- | --- | --- |
| 1 撮る | runner の撮影に、DOM と CSSOM の直列化を足す。CSSOM は、DOM が生きているうちに、その story に当たる規則だけを選んでから直列化する（擬似要素と状態の擬似クラスを外して当てる近似。選び損ねの見逃しは、まだ測れていない）。`dom_match_mode` が `off` のときは何もしない。`screenshots` と `baseline_entries` に列を足す。baseline を作る処理に、列の写しを足す | 無し | 戻せる。mode を `off` に戻せば撮らなくなる。列は残るが、読まれない |
| 2 hash を取る | 正規化の規則と、hash の作り方を、判定から独立した純粋な関数として作る。規則ごとの試験を、`data-v-` の連番化から入れる。規則は、scoped style の印、`v-bind()` の名、`@keyframes` の名に付く印、参照で結ばれた id、毎回変わる origin、コメントと描画しない要素の除去である | 無し | 戻せる。呼ばなければ働かない |
| 3 設定 | `projects` に三つの欄を足す。API、OpenAPI、設定画面の欄を足す。検証の規則を入れる | 無し（既定が `off`） | 戻せる。欄は null と `off` のまま残せる |
| 4 比べる（`shadow`） | 比較ジョブに、hash の一致の判定を足す。`shadow` では、記録だけをして判定を変えない。`comparisons` の欄を足す | 無し | 戻せる。mode を `off` に戻せばよい |
| 5 画面 | 一覧の印、詳細の根拠の欄、ビルドの要約、commit status の説明を足す | 無し | 戻せる。表示だけである |
| 6 判定を変える（`enforce`） | `enforce` で、緩めた判定を使う | 有る | 一部戻せる。まだ承認していないビルドは、mode を `off` に戻して再比較（`recompare`）すれば、元の判定に戻る。承認済みのビルドの記録は、そのとき書かれた判定のまま残る |
| 7 本文と差分の画面 | 本文を内容アドレスで保存し、DOM の差分の画面を足す。掃除の規則を決める | 無し | 戻せる。ただし、保存した本文の掃除は別に要る |

段 5 は、段 6 より先に出す。
緩めたことが見えない状態で、判定を変えてはならない。

段 1 から 5 までは、判定を変えない。
利用者にとっての危険は、段 6 の一箇所に集まる。
そこへ上げる条件は、「`shadow` で確かめる」の節の基準である。

## 決めておくべきこと

次は、この設計では決めない。

- 上限の値（0.5 と 0.05 の仮置き）。
  `shadow` の実測のあとで決める。
- `enforce` で緩めた比較にも、人のレビューを要求するか。
  要求すれば、緩めた比較は `unchanged` のままレビューの対象になり、承認の手間が残る代わりに、見逃しの目が残る。
- 内容アドレスで置いた本文の掃除の規則。
- 正規化の規則を、利用者が足せるようにするか（project ごとの属性の無視の規則）。
  足せれば不一致を減らせるが、利用者が誤った規則を入れて見逃しを作る入口にもなる。
- `screenshots` モードに DOM の hash を送らせる経路。
  CLI と SDK の側の変更になる。
- runner が読み込んだ font と画像の応答を hash にする案の、費用と効果。
- 当たる規則の選び方の近似の精度。
  実測では、260 本のうち 249 本が一致したが、選び損ねの有無は測れていない。
