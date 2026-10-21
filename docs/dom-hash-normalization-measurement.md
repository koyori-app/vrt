# DOM と CSSOM の正規化の実測

本文書は、`apps/backend/crates/service/src/dom_hash/` に作った正規化と hash が、実物の Storybook で成り立つかを測った結果を記す。
設計は `docs/dom-hash-relaxed-threshold.md` の段 2 にあたる。
測ったのは、同じ source を二度 build したときに hash が一致するか、そして source が違う二つの commit でも、見た目が変わらない story の hash が一致するかである。

## 結論

- 同じ source を二度 build すると、story 267 本のうち 266 本で、DOM と CSSOM の hash が両方一致した。
  残る 1 本は CodeMirror が作る乱数の id で、規則には入れていない。
- 別の commit の間では、CSSOM を全 stylesheet で hash にすると、260 本のうち 0 本しか一致しない。
  Tailwind CSS が一枚の stylesheet を出すので、どこかで utility が一つ増えるだけで、全 story の hash が動く。
  この形では、仕組みが働く機会がほとんど無い。
- CSSOM を「その story で使われる規則だけ」に絞ると、同じ組で 249 本が両方一致した。
  残る 11 本は、10 本が実際に画面の中身が変わった story で、1 本が上の乱数 id である。
- この絞り込みは、字を受け取る側（本 module）では行えない。
  DOM が生きているブラウザの側で、規則を選んでから直列化する必要がある。
  段 1 の仕事になる。
- Vue の scoped style の実物（arc-vue の Storybook）では、source が違う二つの build の間で、印の `data-v-` が変わった。
  この印だけを番号にしても一致せず、`v-bind()` が作る custom property の名と、`@keyframes` の名に付く印も番号にして、初めて一致した。

## 測ったもの

### 試料

利用側の Storybook（story 267 本、Vue 3.5、Tailwind CSS 4、Storybook 10、Vite）と、公開 repo の arc-vue の Storybook（story 5 本）を使った。
arc-vue の側は story が 5 本しか無く、十本に満たない。
scoped style の印を持つ試料として、補う目的で使った。

### 撮り方

- 各 build を、loopback の別々の port で配信した。
- Playwright 付属の Chromium を headless で動かし、viewport は 1280 × 720、時刻帯は UTC、動きを減らす設定を入れた。
- story ごとに別の context で開き、Storybook の描画の段階が終わるのを待ち、フォントを待ち、0.6 秒置いてから撮った。
  さらに 0.4 秒置いてもう一度撮り、一度目と二度目が一致するかを見た。
- 撮った字は、`document.documentElement.outerHTML` と、全 stylesheet（`document.styleSheets` と `adoptedStyleSheets`）の全規則の `cssText` を連ねた字である。
- 「使われる規則だけ」の版は、同じ page の中で、規則の selector が `document.querySelector` に当たるかで選んだ近似である。
  擬似要素と状態の擬似クラスは外して当てた。
  `@keyframes`、`@property`、`@font-face` は、残した規則の字に名前が出る物だけ残した。
  custom property の宣言は、残した規則から `var()` で参照される物だけ（推移的に）残した。

最初の撮り方では、描画の段階を待たずに撮ったため、play 関数の途中で撮った story が不安定になり、dialog を開く story 14 本は撮れなかった。
Storybook の描画の段階を待つ形に直して、全 story が撮れた。

### 比べ方

- 同じ source を二度 build した二つの出力は、管理用の JSON 一つを除いて、byte 単位で同じだった。
  この構成では、build そのものに揺れは無く、揺れは実行時にだけ出る。
- 別の commit は、同じ repo で frontend の 43 ファイルが違う二つを使った。
  共通する story は 260 本で、新しい側にだけある 7 本は比べていない。
- arc-vue は、同じ commit を二度 build した組と、`GanttChart.vue` だけを土台の commit の版に戻した組を比べた。

## 結果

### 同じ source を二度 build

| 規則を足していった順 | DOM が一致 | CSSOM が一致 | 両方一致 |
| --- | --- | --- | --- |
| 字句に割って書き戻すだけ | 0 | 267 | 0 |
| 属性と class の並び | 0 | 267 | 0 |
| コメント | 0 | 267 | 0 |
| 描画しない要素（head、script、link、style、noscript） | 258 | 267 | 258 |
| loopback の origin と blob | 266 | 267 | 266 |
| 残りの規則（scoped style の印、`v-bind()` の名、自動の id、CSS の空白） | 266 | 267 | 266 |

CSSOM は、規則を入れる前から 267 本が一致した。
全 stylesheet でも、使われる規則だけでも、結果は同じだった。

一致しなかった 1 本は `Tasks/CommentEditor` の「Ime Composition Does Not Accept A Candidate」である。
CodeMirror の補完の欄が、`aria-controls` と `aria-activedescendant` に、`cm-ac-` に乱数が続く id を持つ。
この id は build のたびに変わる。
測定のためだけに、この id を一つの字に畳むと 267 本すべてが一致した。
ただし、特定の部品の名を決め打つ規則は、本 module に入れていない。

### 別の commit

| 比べ方 | DOM が一致 | CSSOM が一致 | 両方一致 |
| --- | --- | --- | --- |
| 全 stylesheet の CSSOM | 249 | 0 | 0 |
| 使われる規則だけの CSSOM | 249 | 250 | 249 |

260 本のうち、249 本が DOM と使われる規則の CSSOM の両方で一致した。
一致しなかった 11 本は次のとおりである。

- 実際に画面の中身が変わった story が 10 本ある。
  PR の状態の絞り込みが増えた一覧が 5 本、PR の行の表示が変わった 3 本、カレンダーの行の高さと並びが変わった 2 本である。
  この 10 本は、DOM も使われる規則の CSSOM も、どちらも違った。
- CodeMirror の乱数 id の 1 本。

この試料には、DOM が同じで CSS だけが変わった story が無い。
使われる規則だけの CSSOM が、CSS だけの変更を捕らえることは、この測定では確かめられていない。
構造としては、使われる規則の字が変われば hash が動く。
ただし、規則の選び方が近似なので、選び損ねた規則の変更は見逃す。

### 二度の撮影の揺れ

0.4 秒あけた二度の撮影で、字が変わる story が、どちらの build でも同じ 8 本あった。
いずれも `Pages/TaskDetail` の編集系の story で、「保存しました」という状態の表示が、時間の経過で消える。
設計どおり、二度の撮影が違う story は hash を作らないとすると、同じ source の二度の build で、安定した 259 本のうち 258 本が一致する。

### Vue の scoped style の実物

arc-vue の 5 story で、`GanttChart.vue` の版が違う二つの build は、印が `data-v-58152a41` と `data-v-a9b69a0c` に変わった。
この二つは、Chromatic の DOM の差分の画面に出ていた違いと同じ形である。

| 規則を足していった順 | DOM が一致 | CSSOM が一致 | 両方一致 |
| --- | --- | --- | --- |
| 描画しない要素まで | 0 | 0 | 0 |
| scoped style の印 | 0 | 0 | 0 |
| `v-bind()` の custom property の名 | 5 | 5 | 5 |

印だけを番号にしても、一致しなかった。
`v-bind()` が `style` 属性と CSS に出す custom property の名も、build ごとに変わる（`--v5ad87c78` と `--cc7b5cd0` など）。
さらに、scoped style の中の `@keyframes` の名には、印の 16 進が後ろに付く（`koyori-gantt-shimmer-58152a41`）。
本 module は、前者を別の規則にし、後者は印の規則の中で番号にした。
この三つが揃って、5 本すべてが一致した。

同じ commit を二度 build した組は、CSSOM が規則を入れる前から 5 本すべて一致し、DOM も描画しない要素を落とすだけで 5 本すべて一致した。
同じ source なら、印も変わらない。
印が変わるのは、その `.vue` の source が変わったときだけである。

## 規則ごとの効きと、落とすと見逃す物

「効き」は、全規則から、その規則だけを外したときに、両方一致する story がいくつ減るかである。

| 規則 | 効き | 落とすと見逃す物 |
| --- | --- | --- |
| 描画しない要素を落とす | 外すと、同じ source で 8 本減る。別 commit では 249 本が 0 本になり、別 source の arc でも 5 本が 0 本になる | head の `meta` と `title` の変化、script の字の変化。描画の結果は DOM に出るので、描画に出る変化は見逃さない。`style` と `link` の中身は CSSOM の側で見る |
| loopback の origin と blob | 外すと、同じ source で 8 本、別 commit で 8 本減る | origin の違いだけ。path は残すので、別の画像を指す変化は見逃さない |
| コメントを落とす | 外すと、別 commit で 30 本減る（同じ source と arc では減らない） | 描画しない。Vue が入れる `v-if` の占位と、fragment の印が、見た目に関係なく増減する |
| scoped style の印を番号にする | 外すと、別 source の arc で 5 本が 0 本になる。task の試料には印が無く、効きは測れていない | 印の字。どの要素にどの scope の規則が効くかの対応は、番号で残る |
| `v-bind()` の custom property の名を番号にする | 外すと、別 source の arc で 5 本が 0 本になる | 名前の字。どの規則がどの変数を読むかの対応は、番号で残る |
| 自動で生える id を番号にする | 測れていない（試料では、同じ build の中で id が揺れていない） | id の字。参照の対応は番号で残る。単体試験でだけ確かめた |
| 属性と class の並び、CSS の空白 | 外しても減らない（試料では並びも空白も揺れていない） | 並びと空白の違いだけ |

規則を足して一致を増やすほど、見逃す物も増える。
本 module は、次の線で止めた。

- 落とす物は、描画に影響しないと確かめられる物と、名前の字だけが毎回変わり、参照の対応を番号で残せる物に限った。
- text の字、属性の値、画像と font の path は落とさない。
  時刻と乱数が入る story は、一致せずに従来どおりの比較になる。
- CodeMirror の `cm-ac-` のような、特定の部品の乱数の id は、規則に入れていない。
  入れれば、この試料の一致は 267 本すべてになる。
  入れない理由は、部品ごとの規則が際限なく増えること、そして、自動で生える id の「既知の生成規則」を決め打ちで増やす形が、設計の「迷うものは残す」と相容れないことである。

## 設計との食い違い

次の三点は、設計の文書と違う。

- CSSOM の範囲。
  設計は、最初の版を全 stylesheet の全規則とした。
  測った結果、Tailwind CSS のような utility の CSS では、別の commit の間で一本も一致しなかった。
  使われる規則だけを選ぶ版が、設計で後回しにした選択肢であり、実測で必要になった。
  選ぶ処理は、DOM が生きているブラウザの側で行う必要がある。
- 規則の追加。
  `v-bind()` の custom property の名と、scoped style の `@keyframes` の名に付く印は、設計に無かった。
  arc-vue の実物で見つかった。
  どちらも、印と同じく、参照の対応を番号で残せる。
- hash の形。
  設計の `dom_hash_profile`（撮影条件）は、本 module に入れていない。
  撮影条件は段 1 と段 3 の仕事である。
  正規化の版は `NORM_VERSION` として持ち、hash の入力に含める。

## 測っていないこと

- DOM が同じで、CSS だけが変わった story の検出（上に記した）。
- 規則の選び方の近似が、使われている規則を落とす場合。
- closed の shadow root、canvas の中身、画像と font のファイルの中身。
  これらは、DOM にも CSSOM にも出ない。
  設計の上限で受ける。
- 自動で生える id の規則の効き。
  試料に、build ごとに変わる id が無かった。
- runner の撮影の中での揺れ。
  測定の撮影は、runner の静止処理を通していない。
- scoped style を持つ試料の数。
  5 本しか測っていない。
