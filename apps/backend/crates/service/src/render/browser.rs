//! 展開済み Storybook バンドルをヘッドレス Chromium で撮影する。
//!
//! ## 構成
//!
//! 1. [`StaticServer`] が展開先ディレクトリを `127.0.0.1:0`（OS 任せの空きポート）で配信する。
//!    ループバック限定なので外部からは触れない
//! 2. [`StoryRenderer`] が chromiumoxide で Chromium を起動し、
//!    `http://127.0.0.1:{port}/iframe.html?id={story_id}&viewMode=story` を開く
//!    （project 設定で有効な場合は、ナビゲーション前に
//!    `Emulation.setEmulatedMedia` で `prefers-reduced-motion: reduce` を
//!    エミュレートし、撮影直前に適用を実測する——効いていなければ撮らない）
//! 3. Storybook 自身の描画完了シグナル（アドオンチャンネルの `storyRendered`）を待ち、
//!    短い settle 待ちのあと [`FREEZE_SCRIPT`] でキャレットとアニメーションを
//!    決定的に静止させてから、ビューポートを PNG で撮る
//!
//! ## 描画完了の判定
//!
//! かつては「`#storybook-root` に子要素が入ったら完了」という DOM ヒューリスティックだった。
//! これは**中身が空のストーリーで永久に成立しない**。実際に
//! `v-if="strength"` で何も描かない `PasswordStrengthBar` の `strength ''` が
//! 30 秒タイムアウトでビルドを落とした。空の描画結果は正当な「真っ白なスクリーンショット」であって、
//! 失敗ではない。
//!
//! そこで判定はシグナル優先の 2 段構えにする。
//!
//! 1. **Storybook のシグナル（主）**: ナビゲーション前に [`READY_HOOK_SCRIPT`] を注入し、
//!    `window.__STORYBOOK_ADDONS_CHANNEL__` に生えた瞬間へリスナーを差し込む。
//!    `storyRendered` で完了、`storyErrored` / `storyThrewException` /
//!    `playFunctionThrewException` / `storyMissing` はそのストーリーのエラーとして扱う
//!    （30 秒待たされずに理由が出る）。rendered / error の印には観測時の
//!    `documentElement`（世代）を併記し、現在の document と一致するものだけを
//!    信じる——state は window 上で `document.open()` / `document.write()` を
//!    生き延びるため、世代なしでは前 document の印でやり直しの巡が素通りする
//!    （cmd_661 ①）。チャンネルを掴み損ねた場合の保険として
//!    `__STORYBOOK_PREVIEW__.storyRenders[].phase` も見る——こちらは世代を
//!    刻む口が無いので、現在の document の root に描画結果があることを併せて
//!    要求する（root つきの内容へ差し替える形は依然すり抜ける。既知の限界）
//! 2. **DOM フォールバック（従）**: Storybook ランタイムがまったく存在しないバンドル
//!    （手書きの最小 iframe.html など）でだけ効く。ランタイムの起動を待ち損ねないよう
//!    [`SIGNAL_GRACE`] の猶予を置いてから、旧ヒューリスティックで判定する。
//!    猶予は**検証列の巡ごと**に測り直す——story 全体の開始から測ると、
//!    やり直しの巡では猶予が最初から尽きており、入れ替わった document の
//!    途中の絵を ready と誤判定する（cmd_661 ③。deadline は従来どおり
//!    story 全体で共有）
//!
//! ## window に常駐する判定状態（世代の扱い）
//!
//! READY 判定が読む window 常駐の状態は三系統ある。document を差し替える
//! story（reload・`document.open()`）では「前 document の状態が生き残って
//! いないか」が常に問題になる——系統ごとに、置く理由・世代を刻めるか・
//! 刻めなければ何が起きるかを揃えて管理する（**window 常駐の判定状態を
//! 足したら、この表にも行を足すこと**。cmd_662 ⑥）。
//!
//! | 常駐状態 | 置く理由 | 世代を刻めるか | 刻めなければ／刻むまで何が起きたか |
//! |----|----|----|----|
//! | `window.__VRT_READY__`（rendered / error） | channel の accessor は window にしか張れない。state **自体**は document 側へも置ける（accessor は window・state は document という分担は、フォントの印 `dataset` で実装済みの形）が、window に置いたままにした——document 側へ移す道は取らなかった。理由: errorRoot / renderedRoot は documentElement への**参照**で `dataset`（文字列のみ）には刻めず、世代印だけで `document.open()` の生き残りは既に塞がっている——置き場を移しても同じ検知を別の形で得るだけで、検知の強度は上がらない（cmd_663 ⑧） | **刻める**——イベント発火時の documentElement を rendered / error 各々に併記し、読む側（[`READY_PROBE`]）は現在の document と一致する印だけを信じる。error の「先着固定」も**世代の中で閉じる**——前 document の error が新 document の error の記録を塞がない（cmd_661 ①・cmd_662 ②） | 世代なしの rendered は前 document の印でやり直しの READY 待ちを素通りさせ、未描画の絵を撮った（cmd_661 ① 実測）。書く側が世代を見ない error は、新 document の二度目のエラーを記録せず、rendered だけが新世代で立って撮影が通った（cmd_662 ② 実測。fail-open） |
//! | `__STORYBOOK_PREVIEW__.storyRenders`（保険経路） | Storybook 自身が window に置く内部状態——我々の設計物ではない | **刻んでいない**（採らなかった判断。証明されているのは「現状の実装が刻まない」まで——Storybook の内部状態へ書き込んで世代を刻む改変は原理的には可能かもしれないが、他所の内部実装への書き込みはバージョン差で壊れる面を増やすため採らない。cmd_663 ⑧） | completed: 現在の document の root に描画結果があることを併せて要求して緩和（root つき内容への差し替えは依然すり抜け——既知の限界）。**この門の代償**: 保険経路にしか乗れない構成（channel を掴み損ねた・`__STORYBOOK_ADDONS_CHANNEL__` を経由しない等）では、中身が空になる**正当な** story（null を返す story・portal で body 直下へ描く story——root は空のまま）が `domReady` を永久に満たせず、門の導入前は撮れていたものが 30 秒 Timeout になる——倒れる向きは fail-closed（未検証の絵は撮らない）だが巻き添えであり、既知の限界として併記（cmd_663 ⑤。「root つき差し替えのすり抜け」と逆方向の対）。errored / aborted: 門なし——stale が読まれても「誤った理由での story 失敗」（fail-closed 側）にしか倒れず、domReady は error の門として意味を成さない（[`READY_PROBE`] 内の判断コメント。cmd_662 ③） |
//! | `__STORYBOOK_ADDONS_CHANNEL__`・`__STORYBOOK_PREVIEW__` の**存在自体**（runtime 判定） | Storybook が代入する。hook は accessor で捕捉するだけで、存在の有無を「ランタイムがいるか」の判定に使う | **不要／不可**（イベント側の世代は hook が刻む。存在の有無に世代は無い） | 差し替え後も window に残るため、Storybook を持たない document へ差し替えると probe は runtime ありと誤認して永久に `pending`——DOM ヒューリスティック（Absent 経路）へは二度と落ちず、共有 deadline の Timeout へ倒れる（fail-closed。未検証の絵を撮る方向には壊れない。`a_stale_storyrenders_phase_does_not_ready_the_swapped_document` が同型の到達側を固定） |
//!
//! ## 層ごとの失敗経路
//!
//! **層を足したら、その層自身が失敗したときどう倒れるかを数え、この表に行を
//! 足すこと。** 静止・検証の仕組みは層の積み重ねであり、層を足すたびに
//! 「その層自身の失敗」という新しい経路が生まれる。過去に三度、これを
//! 数え損ねて fail-open（失敗したのに成功として撮る）を作った——
//! 静止の失敗（`FREEZE_SCRIPT` が無条件 true）、解析の失敗（`ok` 欠落を
//! 成功扱い）、検証の失敗（catch で `errors` に積むだけで `ok: true` へ到達）。
//! 診断を**集める**ことと判定に**使う**ことは別である。集めるだけの
//! エラーリストは、対処した気にさせる分だけ無いより悪い。
//!
//! | 層 | 失敗を検知できるか | 検知したらどう倒れるか / 検知できぬ理由 |
//! |----|----|----|
//! | [`READY_HOOK_SCRIPT`] 注入 | CDP エラーは検知 | Rust 側 `Err`（fail-closed）。`defineProperty` 失敗は JS 内で握るが、`storyRenders` 保険が外れれば pending のままタイムアウトへ倒れる（fail-closed） |
//! | [`READY_PROBE`] | 検知 | evaluate 失敗はリトライし期限で `Timeout`。JSON が壊れて/想定外なら [`Readiness::parse`] が「まだ待つ」へ倒しタイムアウト（fail-closed。誤って完了扱いにしない）。hook の rendered / error は世代（観測時の documentElement）が現在の document と一致するものだけ読む——`document.open()` を生き延びた前 document の印では判定しない（cmd_661 ①）。`storyRenders` 保険には世代を刻む口が無いため、現在の document の root に描画結果があることを併せて要求する——**root つきの内容へ差し替える形は依然すり抜ける**（既知の限界。`a_stale_storyrenders_phase_does_not_ready_the_swapped_document` が到達できる側を固定） |
//! | 静止 CSS 注入（`freezeRoot`） | throw・API 欠落を検知 | constructed stylesheet（CSSOM）で注入する。構築・`replaceSync`・`adoptedStyleSheets` 代入の throw、`CSSStyleSheet` コンストラクタの欠落は `errors` → `ok: false`（fail-closed）。CSSOM 操作は CSP `style-src` の管轄外なので、旧 `<style>` 注入が持っていた「CSP による例外なしの黙殺」という検知不能経路は**構造ごと消えている** |
//! | seek・pause | throw は検知 | `errors` → `ok: false`（fail-closed） |
//! | 収束反復 | running 残は検知 | `MAX_SWEEPS` 内に running=0 とならねば `ok: false`。rAF が返らないハングは JS 内では検知できぬ（promise が解決せず evaluate が返らない）が、Rust 側で evaluate を READY 待ちと共有の deadline（`started + story_timeout`）の残余の `tokio::time::timeout` に載せてあり、時間内に静止が終わらねば失敗（fail-closed） |
//! | FREEZE evaluate の CDP 往復 | 検知 | evaluate 自体のエラー——story 側の navigation / reload による「Inspected target navigated or closed」、rAF コールバックを捨てるページでの pending promise GC 回収「Promise was collected」（いずれも -32000・実測）——は READY probe と同様 deadline までリトライし、期限で [`RenderError::Timeout`]（story 単位に隔離。即中断の [`RenderError::Cdp`] へは倒さない） |
//! | running 収集（`collectRunning`） | throw・API 欠落を検知 | 収集は `freezeRoot` と共通の `collectAnimations` を通る。`getAnimations`・走査の失敗、および root 側 `getAnimations` API の欠落（擬似要素アニメを数える口が無い）は `errors` → `ok: false`（fail-closed。空 `[]` へ黙って倒さない）。クロスオリジン iframe の中は**原理的に数えられぬ**（`contentDocument` が null）——README「届かない範囲」に契約として明記 |
//! | CSS 適用検証 | 検知 | 注入 sheet が root のカスケードに入ったかを、root ごとに 1 要素の `--vrt-frozen` プローブで実測する（sheet の存在は root 単位の性質なので 1 点で足りる。既定値と偶然一致して素通しする値でもない）。プローブ欠落は `ok: false`、検証呼び出し自体の throw も `errors` → `ok: false`（fail-closed）。個別要素の `!important` 上書きは検証対象に**しない**——README「届かない範囲」の best-effort 契約であり、ハード失敗させると契約と食い違う。切り離された root は描画に影響しないため対象外 |
//! | 結果の JSON 化 | 間接的に検知 | `JSON.stringify` の差し替え・失敗は文字列でない/読めない応答となり、Rust 側 [`freeze_verdict`] が unparseable として失敗（fail-closed） |
//! | Rust 側の解析（[`freeze_verdict`]） | 検知 | `ok === true` と確かめられた場合のみ撮影へ進む。欠落・型違い・parse 失敗はすべて unparseable（fail-closed） |
//! | iframe・shadow 走査（`freezeRoot` 再帰） | 部分的 | open shadow root と同一オリジン iframe には到達。closed shadow root は**列挙する API が存在せず検知不能**、静止処理より後から生成される root にも届かない——いずれも README「届かない範囲」でページ側の責務と定めてある |
//! | スクリーンショット | 検知 | CDP エラーは Rust 側 `Err`（fail-closed） |
//! | reduced-motion 適用（`Emulation.setEmulatedMedia`） | 検知（CDP エラー・無応答とも） | project 設定で有効なときだけ `new_page` 直後（ナビゲーション前）に一度呼ぶ。CDP エラーは Rust 側 `Err` → [`RenderError::Cdp`]（環境分類・即中断。story のスクリプトを待たない一往復で、失敗の原因はブラウザ側——`new_page` と同じ分類）。無応答は chromiumoxide の request timeout（既定 30 秒）が `CdpError::Timeout` を返し、同じ `Cdp` へ倒れる（fail-closed） |
//! | reduced-motion 適用の検証（[`REDUCED_MOTION_PROBE`]） | 部分的に検知 | 「呼び出しは成功したが実際にはメディアクエリが変わっていない」を撮影直前に実測する——constructed stylesheet の `@media (prefers-reduced-motion: reduce)` が効いたかのプローブ（`--vrt-reduced-motion`）と `matchMedia().matches` の**両輪**。どちらかが不成立なら `ok: false` → [`RenderError::Story`]（fail-closed。reduce を返さない壊れた/モックされた `matchMedia`——polyfill やテストダブルの事故——はここで落ちる）。evaluate の CDP エラーは READY probe と同様 deadline までリトライし期限で [`RenderError::Timeout`]。**ページが両方の観測を偽装する積極的な偽りは原理的に検知不能**——検証はページの JS realm で走り、CDP に emulated media 状態を読み戻す API が無い。脅威モデルは一貫して事故であり悪意ではない（README「検証層自身の失敗も fail-closed である」と同じ契約） |
//! | reduced-motion 有効なのに呼び出し自体が漏れる | 実行時には検知不能 | 検知器の不在そのものがこの失敗であり、実行時観測では塞げない（「呼ばれなかったこと」を観測する層は、それ自身も呼ばれない）。構造で塞ぐ——適用は [`StoryRenderer::render_story`] の単一チョークポイントにだけ置き、分岐は `RenderOptions::emulate_reduced_motion` の一つ、project 列からの配線は `render_build` の単体テストで固定、経路全体は「ON で絵が変わる」positive control テスト（`reduced_motion_emulation_changes_the_picture_and_is_deterministic`）が貫通して固定する |
//! | フォント条件待ち① 読み込みの**失敗**（[`FONTS_WAIT_SCRIPT`]） | 検知する（波が尽きた時点で個々の `FontFace.status === 'error'` を列挙し、family を Set で一意化） | **意図した fail-open**——撮って、[`fonts_verdict`] が有界の警告に整形し、[`RenderedStory::font_warning`] として成功値に載せる——`render_all` が build log（`LogLevel::Warn`）へ永続化して利用者に届く（`tracing::warn` はサーバー運用ログ止まりで利用者には見えない。cmd_660 C）。cmd_658 は (a) fail-closed（1 つでも error なら story 失敗）を採ったが、cmd_659（yupix レビュー）の新しい実証で (b) 撮って警告へ転換した——`document.fonts` は document 全体の集合で、原因の `@font-face` は preview-head 等で **project 全体に共有**される。(a) では、egress の無いワーカーで外部フォントを参照する project・`local('Helvetica Neue')` を Linux で撮る project が「全 story 失敗・スクリーンショット 0 枚」になり、そのフォントを一切表示しない story まで道連れになる（修正前は fallback で決定的に緑だった）。**(b) が救うのは 404・接続拒否・`local()` 不在のように読み込みが error へ確定する場合だけ**である——無応答（blackhole された CDN 等）では FontFace が `loading` のまま `ready` が解決せず、(b) でも経路④の Timeout に倒れる。(a) と挙動は同一で、300 story × 30 秒＝約 2.5 時間かけて全滅する費用は **(a)(b) 共通の限界**（cmd_659/660 で将軍の誤帰属を訂正。時間上限の考えは「層ごとの手当て」表とREADME を参照）。cmd_658 の (a) は当時の材料（断続 CDN が二つの baseline を生む）では妥当で、(b) は cmd_658 が明示的に許した道——覆したのは新しい実証であって後戻りではない。代償: 断続的にしか届く外部フォントは run ごとに違う絵を作りうる——「同じビルドから同じ絵」は**外部依存の応答が同じ場合**の保証となる（README・docs/architecture.md に同じ限定を明記。警告本文にもこの旨を書く）。恒久対処はフォントの同梱か `@font-face` 参照の除去（警告が名指しする）。厳格側（(a) を project 設定でオプトイン）は後日の選択肢として残す。`a_failing_font_load_captures_with_a_warning_and_stays_deterministic` が固定する（警告が成功値に載ることも assert——`FONTS_WAIT_SCRIPT` が `failed` を返す契約の script 側の固定を兼ねる。cmd_660 F）——黙って fail-closed へ戻す変更・`failed` を落とす変更はこの試験を落とす |
//! | フォント条件待ち② `ready` 解決後の新たなフォント要求 | 応答前の波は検知 | 応答の直前に `status` を読み直し、`'loading'` へ戻っていれば失敗にせず `ready` を**待ち直す**——二段以上でフォントを読むページは各波が有限なら収束し、そこから先は決定的に撮れる。読み込み**中**は失敗ではない——ここで即失敗にすると、フォントが問題なく届く二波ページを恒久的に撮影不能へ誤分類する（cmd_657 実測: 旧実装で二波 fixture が 8/8 失敗）。①の失敗検知は波が尽きた後に行うので、待ち直しと衝突しない。待ち直しに回数上限は設けない——上限値はどんな数でも根拠がなく、停止性は共有 deadline が担う。収束しないページは経路④の Timeout へ倒れる（`a_second_font_wave_after_ready_is_awaited_not_failed`・`a_second_wave_that_never_ends_times_out_without_capturing` が固定）。応答から撮影までの窓に始まる要求・document の入れ替わりは、検証列の最後（静止の後・撮影の直前）の再確認（[`FONTS_RECHECK_PROBE`]——経路⑤）が検知し、検証列をやり直す。再確認から撮影までの**最後の一往復**に始まる変化だけは原理的に検知できぬ——スクリーンショットは JS を走らせない一往復の CDP コマンドで、その瞬間のフォント状態を読み戻す API が無い（README「届かない範囲」と同じ契約） |
//! | フォント条件待ち⑤ 検証後の document 入れ替わり・新しい波（[`FONTS_RECHECK_PROBE`]） | 検知する（撮影の直前に、[`FONTS_WAIT_SCRIPT`] が成功時に検証した各 document へ残す印 `documentElement.dataset.vrtFontsVerified` と `document.fonts.status` を、FREEZE の `freezeRoot` と同じ範囲——open shadow root へ潜り `iframe` と `frame` の両方——で同一オリジン iframe まで再帰して 1 往復で読む。cmd_661 ②）。`dataset` を持たない documentElement（素の XML document）には印を刻む口が無く、両側で要求しない——その document の入れ替わりは印では捉えられない（fonts.status の検査は残る。cmd_661 追送）。**印が消えるのはナビゲーションと `document.open()` / `document.write()`**——どちらも documentElement を作り直す（印を window に置くと `document.open()` がグローバルオブジェクトを維持するため生き残り、入れ替わりを見逃す——cmd_660 実測で訂正）。**同一 document 内の DOM 全面置換（`body.replaceChildren` 等）では documentElement が残るため印も残り、捉えられない**（下の「本 PR で扱わないもの」を参照） | 不成立なら失敗ではなく検証列を**READY 待ちと `SETTLE_DELAY` から**やり直す（cmd_660）——Blink の `FontFaceSet.ready` は「load イベント完了＋読み込み中フォント無し」で解決するため、reload 後の document では story 未描画の時点で fonts 待ちが通ってしまい、fonts 待ちからのやり直しでは `storyRendered` 前の未描画の絵を撮る（cmd_660 実測: reload 後の再描画が遅い fixture で白い絵が撮れた。既存 reload fixture は reload 後 40ms で `storyRendered` を出すためこの経路を踏まなかった）。READY 待ちから回すことで二巡目の `storyErrored` / `storyThrewException` も観測される（cmd_660 実測: 修正前は黙殺して撮れていた）。ただし READY の印が世代を持つことが前提——`document.open()` はグローバルオブジェクトを維持するため、世代なしの `window.__VRT_READY__.rendered` は前 document の `true` のまま生き残り、やり直しの READY 待ちが新 document を一度も待たずに素通りしていた（cmd_661 ① 実測: 差し替え後の再描画が遅い fixture で未描画の白い絵が撮れ、二巡目の `storyThrewException` は黙殺された。世代印で修正）。storyRendered 後に自分を reload する story は、fonts 待ちが前 document で ok を返し FREEZE が navigated or closed のリトライを経て後 document で成功するため、修正前はフォント未検証の document がそのまま撮れた（cmd_659 実測。窓はリトライ全長＝数十秒になりうる）。フォント待ちを FREEZE の後ろへ動かす形は採らない（FREEZE は最終レイアウトを見る必要があり、フォント適用で始まる CSS transition も静止の対象）。常に二度待つ形も採らない——再待ちは窓が開いたと検知された時だけ。停止性は共有 deadline（やり直しが尽きねば [`RenderError::Timeout`]・fail-closed）。応答の解析不能は [`RenderError::Story`]（fail-closed）。`a_story_that_reloads_after_the_fonts_wait_is_not_captured_unverified`（陽性対照つき）・`a_reloading_story_whose_fonts_arrive_recovers_and_captures_verified`・`a_document_swapped_via_document_open_is_recaptured_verified`・`a_slowly_rerendering_reload_waits_for_its_second_ready`・`a_story_error_after_reload_is_observed_by_the_redo`・`a_document_open_swap_with_a_slow_rerender_waits_for_its_second_ready`・`a_story_error_after_a_document_open_swap_is_observed_by_the_redo`・`a_second_error_in_the_swapped_document_is_recorded_not_masked`・`a_stale_storyrenders_phase_does_not_ready_the_swapped_document`・`a_redo_round_regrants_the_dom_heuristic_its_signal_grace`・`fonts_inside_a_shadow_dom_iframe_are_awaited_before_the_capture`・`fonts_inside_a_frameset_frame_are_awaited_before_the_capture`・`a_same_origin_xml_iframe_does_not_break_the_fonts_wait` が固定 |
//! | フォント条件待ち③ ページが `document.fonts` を差し替える | 形の壊れは検知 | FontFaceSet らしい形（`status` が文字列・`ready.then` が関数）でなければ**待たずに** `ok: false`（fail-closed。無いものは待てないが、無いことを黙って通さない）。仕様どおりの顔で即解決を返す偽物は原理的に検知不能——脅威モデルは一貫して事故であり悪意ではない（reduced-motion 検証と同じ契約）。判定は `fonts_verdict_accepts_only_a_verified_ok_true`・`fonts_verdict_rejects_unparseable_results`（単体。手書き JSON への受理条件のみ）が固定し、実ブラウザ貫通は `a_page_that_replaces_document_fonts_fails_the_shape_check`（形チェック分岐——`document.fonts` を非 FontFaceSet 形へ差し替えて [`RenderError::Story`] と `errors` の文言まで実測）と `garbled_fonts_result_fails_instead_of_silently_succeeding`（unparseable 分岐）が固定 |
//! | フォント条件待ち④ `ready` が期限内に解決しない | 検知 | evaluate が返らないので、READY 待ちと共有の deadline の残余（`tokio::time::timeout`）が期限で [`RenderError::Timeout`] へ倒す（fail-closed——**撮らない**。修正前は 250ms 経過後に代替字形のまま撮れてしまい、揺れる絵が baseline に混ざった）。②の待ち直しが収束しない場合の安全弁もこの経路（`a_font_that_never_arrives_fails_instead_of_capturing_fallback_glyphs`・`a_second_wave_that_never_ends_times_out_without_capturing` が固定）。**`ready` が解決しない原因はフォントに限らない**——`ready` は仕様上 load イベント完了にも門を掛けられており、到達不能な非フォント subresource 1 本・load の終わらない同一オリジン iframe 1 つでも同じ Timeout に倒れる（既知の限界＝費用。cmd_663 ⑥。[`DEFAULT_STORY_TIMEOUT`] doc と README のフォント節に明記） |
//! | フォント条件待ち⑥ 待ちの**最中**の document の切り離し（iframe の DOM からの除去・`location.replace`） | 検知する（各 `ready` を切り離し検知の race に載せる。走査（[`COLLECT_DOCUMENTS_JS`]）自体も `defaultView` の無い document を集めない——FREEZE の `freezeRoot` と同じ門。cmd_663 ②） | 切り離された document は描画されず `fonts.ready` は二度と settle しない——race が検知して**待ちからも判定からも捨てる**（撮る絵に影響しない document のために story を落とさない）。修正前はこの門が写されておらず、`Promise.all` が永久 pending となり story_timeout（既定 30 秒。実測 30.31s）を丸ごと消費して④の Timeout に倒れていた。検知の setTimeout ポーリングは fake timers で止まる——その場合は従来どおり deadline の Timeout（fail-closed）。`location.replace` で入れ替わった新 document は次の巡・撮影直前の再確認（⑤）が拾う。`a_detached_iframe_mid_fonts_wait_does_not_time_out_the_story` が固定 |
//!
//! 残る fail-open は二種に分けて管理する。
//!
//! **原理的に観測できないもの**: closed shadow root・クロスオリジン iframe・
//! 後から生成される root・reduced-motion 検証の観測を両輪とも偽装するページ・
//! 再確認から撮影までの最後の一往復に始まる変化。これらは検知不能な理由と
//! ともに README の「届かない範囲」で利用者との契約に昇格させてある。
//!
//! **観測できるが、判定に使わないと決めたもの**（一覧と理由。宣言なき
//! fail-open を残さないため、ここに載らない「観測できるのに使わない」失敗を
//! 作らないこと）:
//!
//! - フォントの読み込み失敗（`FontFace.status === 'error'`）——撮って警告を
//!   残す。判定に使うと巻き添えが story ではなく **project 全体**に及ぶ
//!   （`document.fonts` は document 全体の集合で、原因の `@font-face` は
//!   preview-head 等で全 story に共有される。害の比較は失敗経路①を参照）。
//!   厳格側を project 設定で選べるようにするのは後日の選択肢
//! - 個々の FontFace の想定外 status——`unloaded` は正当（未使用の宣言は
//!   読み込まれないまま残る）、`loading` は集合 status が `'loaded'` の時点で
//!   仕様上ありえず、未知の値は仕様外。列挙は「`error` があるか」だけを見る
//!
//! **本 PR（#27）で扱わないもの**（捉えられぬもの・別途扱うもの。cmd_660 G）:
//!
//! - **同一 document 内の DOM 全面置換**（`body.replaceChildren` /
//!   `documentElement.innerHTML` 差し替え等）——documentElement が残るため
//!   検証済みの印も残り、再確認では捉えられない（新しいフォント波を伴う
//!   場合だけ `status` 側で検知される）。document の同一性ではなく**内容**の
//!   世代を追う必要があり、印の機構では原理的に届かない
//! - **検証世代の全層への一般化**——「印」で document の入れ替わりを追う
//!   機構は、フォントの付属物ではなく検証列全体（READY・settle・fonts・
//!   reduced-motion・freeze）の門番になりうるが、それは #27 の主題
//!   （フォントの時間待ちを状態待ちへ）を超える。世代カウンタ等の一般化は
//!   別 PR で扱う
//!
//! ## 層ごとの手当て（横並び）
//!
//! **層を足したら、先にある層が持つ手当てをその層へも移すこと。** 層は時間差で
//! 積まれるため、後から足した層は先の層が当然に持つ手当て（時間上限・判定への
//! 反映・テストでの固定）を欠いたまま生まれやすい。実例: READY 待ちには最初から
//! deadline があったが、後から足した FREEZE evaluate には時間上限が無く、rAF を
//! 発火させないページで render がハングした（cmd_630 で story_timeout を移した）。
//! この表は各層の手当てを横に並べ、欠けを目視できるようにする——空欄を見つけたら
//! 埋めるか、埋められぬ理由を書くこと。
//!
//! | 層 | 時間上限（停止性） | 失敗の届き先 | 固定するテスト |
//! |----|----|----|----|
//! | Chromium 起動 | `LAUNCH_TIMEOUT` ×最大 `LAUNCH_MAX_ATTEMPTS` 回 | [`RenderError::Launch`] | `launching_a_missing_chromium_fails_fast` |
//! | READY 待ち（probe ポーリング） | `story_timeout` の deadline | [`RenderError::Timeout`] / [`RenderError::Story`] | `a_story_that_renders_nothing_still_produces_a_screenshot`・`a_story_error_signal_fails_fast_with_the_reason` |
//! | FREEZE evaluate | READY 待ちと**共有**の deadline（`started + story_timeout`）の残余。1 story の最悪所要は約 `story_timeout` + `SETTLE_DELAY` に収まる | [`RenderError::Timeout`]（時間切れ。READY 側と同じ分類。evaluate の CDP エラーも READY probe と同様 deadline までリトライし、期限で同じ Timeout——即 [`RenderError::Cdp`] へは倒さない） | `raf_suppressed_page_fails_within_the_story_timeout`・`freeze_timeout_shares_the_story_deadline_with_the_ready_wait`・`reloading_page_during_freeze_fails_story_scoped`・`collected_freeze_promise_fails_story_scoped` |
//! | FREEZE 結果の解析 | 即時（待ちなし） | [`RenderError::Story`]（`freeze_verdict`） | `freeze_verdict_*` 単体群・`garbled_freeze_result_fails_instead_of_silently_succeeding` |
//! | スクリーンショット | **なし**——CDP 呼び出しが返らない場合は上位の CI ジョブタイムアウト頼み。JS の promise を待たない一往復コマンドで、ハングの既知経路が無いため保留（欠けと認識した上での判断） | [`RenderError::Cdp`] | `renders_a_story_to_a_png_with_the_requested_viewport` |
//! | reduced-motion 適用 | chromiumoxide の request timeout（既定 30 秒。一往復コマンド共通の機構） | [`RenderError::Cdp`] | `reduced_motion_emulation_changes_the_picture_and_is_deterministic` |
//! | reduced-motion 検証 | evaluate リトライは READY 待ちと共有の deadline 残余。判定自体は即時 | [`RenderError::Story`]（[`reduced_motion_verdict`]）/ リトライ期限切れは [`RenderError::Timeout`] | `a_page_that_breaks_matchmedia_fails_instead_of_silently_capturing`・`reduced_motion_verdict_*` 単体群 |
//! | フォント条件待ち（[`FONTS_WAIT_SCRIPT`]） | READY 待ちと**共有**の deadline の残余（`ready` が解決しない・再読み込みの波が尽きないページは evaluate ごと期限で倒れる）。**フォント待ちに独立の短い上限は未着手**——解決しないフォントは story ごとに最大 `story_timeout`（既定 30 秒）を消費し、story 数に比例して累積する（既知の限界。[`DEFAULT_STORY_TIMEOUT`] の doc と README のフォント節に明記） | [`RenderError::Timeout`]（期限）/ [`RenderError::Story`]（[`fonts_verdict`]——`document.fonts` の欠落・形違い・`ready` の reject）/ 読み込み**失敗**は警告つきで撮る（[`RenderedStory::font_warning`] → `render_all` が build log の `LogLevel::Warn` へ永続化。失敗経路①） | `waiting_for_fonts_ready_makes_repeated_captures_agree`・`a_font_that_never_arrives_fails_instead_of_capturing_fallback_glyphs`・`a_second_font_wave_after_ready_is_awaited_not_failed`・`a_second_wave_that_never_ends_times_out_without_capturing`・`a_failing_font_load_captures_with_a_warning_and_stays_deterministic`・`a_page_that_replaces_document_fonts_fails_the_shape_check`・`fonts_inside_a_same_origin_iframe_are_awaited_before_the_capture`・`fonts_inside_a_frameset_frame_are_awaited_before_the_capture`・`fonts_verdict_*` 単体群 |
//! | 撮影直前の再確認（[`FONTS_RECHECK_PROBE`]） | READY 待ちと**共有**の deadline の残余（evaluate リトライも、やり直しループ自体も同じ期限で打ち切る） | [`RenderError::Timeout`]（期限）/ [`RenderError::Story`]（[`fonts_recheck_verdict`]——解析不能のみ。不成立は失敗でなく検証列の READY 待ちからのやり直し） | `a_story_that_reloads_after_the_fonts_wait_is_not_captured_unverified`・`a_reloading_story_whose_fonts_arrive_recovers_and_captures_verified`・`a_document_swapped_via_document_open_is_recaptured_verified`・`a_slowly_rerendering_reload_waits_for_its_second_ready`・`a_story_error_after_reload_is_observed_by_the_redo`・`fonts_recheck_verdict_allows_capture_only_when_still_verified` 単体 |
//!
//! ## 走査と門の対応表（揃えた対象の付随物）
//!
//! 「既存の X に揃えた」は、走査の**形**（どこへ降りるか）だけでなく X が持つ
//! **門**（除外条件・前提）まで写して初めて成り立つ（cmd_663 ①。defaultView の
//! 門が六巡のあいだ fonts 側に写されていなかった）。**走査を揃える・門を
//! 足すときは、この表の全行に同じ列を検めること。**
//!
//! | 走査 | 担い手 | 降下（shadow / iframe / frame） | cross-origin | 切り離し（`defaultView`）の門 | 切断（`isConnected`）の門 | 走査自身の throw |
//! |----|----|----|----|----|----|----|
//! | `walkRoots`（freezeRoot＝凍らせる側と collectRunning＝数える側が共有。cmd_663 ⑦で一本化） | [`FREEZE_SCRIPT`] | 全部降りる | `contentDocument` が null / throw——意図した握りつぶし（原理的に触れない。README 契約） | 注入は `defaultView` の無い document を対象外（描画されない）。シーク・収集は切り離し root にも走るが、描画されないので絵に影響しない | ——（root 単位の門は下の検証行が担う） | `collectAnimations` 内で `errors` → `ok: false`（fail-closed） |
//! | 適用検証ループ（`frozenRoots`） | [`FREEZE_SCRIPT`] | 走査済み root の線形走査（降下なし） | ——（frozenRoots に載るのは到達できた root のみ） | document root は `defaultView` の無いものを検証対象外 | shadow root は `isConnected` でない host のものを検証対象外 | `errors` → `ok: false`（fail-closed） |
//! | [`COLLECT_DOCUMENTS_JS`]（検証側 [`FONTS_WAIT_SCRIPT`] と再確認側 [`FONTS_RECHECK_PROBE`] が共有。cmd_662 ④） | フォント待ち・再確認 | 全部降りる（cmd_661 ②） | 同上（契約） | **`defaultView` の無い document を集めない＋待ちの最中の切り離しは `ready` との race が捨てる（cmd_663 ②——この列が空欄だった）** | ——（document 単位の走査。shadow root を root として扱わない） | 各利用側の try → fail-closed |
//!
//! ## story 固有の失敗と環境の失敗（隔離の分類・全経路）
//!
//! **経路を足したら、その失敗が story 固有か環境かを決めてこの表に行を足す
//! こと。** 分類基準は「次の story も同じ理由で落ちるか」——story の内容に
//! 起因する失敗はその story だけをエラーにして残りを撮り続け（発見性）、
//! 環境の失敗は即中断する（続行は同じエラーの羅列に story_timeout×N を
//! 費やすだけ）。ただし story 単位の CDP 失敗（[`RenderError::Cdp`]）だけは
//! 中断へ倒す前に**新しいタブで 1 回やり直す**（[`retry_once_on_cdp`]。
//! タブ単位の間欠故障——セッション確立メッセージの取りこぼしでそのタブへの
//! コマンドが全て 30 秒タイムアウトする——の実測に対する救済。2 回目も
//! 失敗すれば従来どおり即中断）。判定の実装は `render_build::is_story_scoped`（[`RenderError`]
//! のホワイトリスト——新 variant の既定は中断側）と、名前検証の隔離
//! （`render_build::render_all`）。**隔離してもビルドは fail-closed のまま**
//! ——story_failures が 1 件でもあればビルドは `failed` になり、緑には
//! ならない。
//!
//! | 経路 | エラー | 分類 | 根拠 |
//! |----|----|----|----|
//! | Chromium 起動 | [`RenderError::Launch`] | 環境（ループ前に中断） | story を 1 つも処理できない |
//! | 静的サーバー起動 | [`RenderError::Server`] | 環境（ループ前に中断） | 同上 |
//! | `new_page` / READY hook 注入 / `goto` | [`RenderError::Cdp`] | 環境（新タブで 1 回やり直し→即中断） | story のスクリプトはまだ実行されていない——失敗の原因にブラウザ側しかいない |
//! | READY probe の evaluate エラー | リトライ→期限で [`RenderError::Timeout`] | story | ナビゲーション中の一時的な context 差し替えが主因 |
//! | READY 待ち時間切れ | [`RenderError::Timeout`] | story | その story が描画完了シグナルを出さない |
//! | `storyErrored` 等のシグナル | [`RenderError::Story`] | story | Storybook 自身による story 単位の失敗通知 |
//! | FREEZE evaluate のエラー・ハング | リトライ→期限で [`RenderError::Timeout`] | story | navigation / reload・rAF 捨ては story のスクリプトの挙動（実測経路は上表） |
//! | FREEZE verdict（静止失敗・解析不能） | [`RenderError::Story`] | story | その story のアニメーション・応答の内容に起因 |
//! | スクリーンショット | [`RenderError::Cdp`] | 環境（新タブで 1 回やり直し→即中断） | JS を待たない一往復の CDP コマンド——失敗はブラウザ側 |
//! | reduced-motion 適用（`setEmulatedMedia`） | [`RenderError::Cdp`] | 環境（新タブで 1 回やり直し→即中断） | `new_page` 直後・story のスクリプトを待たない一往復——失敗はブラウザ側 |
//! | reduced-motion 検証の evaluate エラー | リトライ→期限で [`RenderError::Timeout`] | story | READY probe と同じ——ナビゲーション中の一時的な context 差し替えが主因 |
//! | reduced-motion 検証の不成立・解析不能 | [`RenderError::Story`] | story | `matchMedia` の差し替え等、そのページの内容に起因 |
//! | フォント条件待ちの evaluate エラー・`ready` 未解決 | リトライ→期限で [`RenderError::Timeout`] | story | どのフォントを要求し返すかは story のバンドル・内容に起因（FREEZE の rAF 捨てと同じ分類） |
//! | フォント条件待ちの不成立・解析不能 | [`RenderError::Story`]（[`fonts_verdict`]） | story | `document.fonts` の形・フォント状態はそのページの内容に起因 |
//! | 撮影直前の再確認の evaluate エラー・やり直しが尽きた | リトライ→期限で [`RenderError::Timeout`] | story | document を入れ替え続ける・フォントを読み続けるのは story のスクリプトの挙動（FREEZE の reload 経路と同じ分類） |
//! | 撮影直前の再確認の解析不能 | [`RenderError::Story`]（[`fonts_recheck_verdict`]） | story | 応答を壊すのはそのページの内容に起因 |
//! | スクリーンショット名の規則違反 | `StoryFailure` 直行（`render_build`） | story | story の title / name に起因。全違反を 1 ビルドで列挙する |
//! | ストレージ・DB・baseline 流用の失敗 | `anyhow`（`render_build`） | 環境（即中断） | 保存経路の異常は次の story でも再現する |
//! | バンドル展開・stories 空 | `anyhow`（`render_build`） | ビルド全体（ループ前に中断） | story 以前の前提が壊れている |
//!
//! 誤分類の非対称性（自己点検）: 環境の失敗を story と誤分類しても、ビルドは
//! story_failures 非空で `failed` のまま（fail-open にならない）、続く story は
//! 同じ環境異常なら `new_page` の環境分類で中断する——失う最大は
//! 1 story あたり 2 試行分（[`retry_once_on_cdp`] の 1 回やり直しを含む）の
//! 時間予算。逆に story の失敗を環境と誤分類すると「1 ビルドで 1 件ずつ」しか
//! 発見できない劣化になる（cmd_631/632 が潰した形）。どちらの向きも
//! 「環境起因をビルド緑で通す」経路にはならない。
//!
//! ## 後始末
//!
//! `StoryRenderer` / `StaticServer` はどちらも drop で確実に止まる
//! （Chromium の子プロセスは kill、HTTP サーバーのタスクは abort）。
//! 正常系では [`StoryRenderer::close`] を呼んで明示的に閉じること。
//!
//! ## 並列性
//!
//! ブラウザはジョブごとに 1 インスタンスとし、`render_build` が独立した page を
//! 最大 2 枚開いて story を並列レンダリングする。上限を固定して Chromium の
//! CPU / メモリ使用量を抑え、3 枚目は先行 2 枚の保存後に開始する。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::emulation::{MediaFeature, SetEmulatedMediaParams};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::ScreenshotParams;
use futures::StreamExt;
use thiserror::Error;
use tokio::task::JoinHandle;

/// 1 ストーリーあたりの描画待ちタイムアウト。
///
/// **既知の限界（フォント待ちの累積費用。cmd_660 E）**: 解決しない
/// フォント（無応答の CDN・`local()` 不在等で `document.fonts.ready` が
/// 確定しないもの）は、story ごとに最大でこの時間を丸ごと消費してから
/// Timeout に倒れる。原因の `@font-face` は preview-head 等で project
/// 全体に共有されるため、費用は **story 数に比例して累積**する
/// （300 story × 30 秒 ≒ 2.5 時間）。これは fail-open（撮って警告）でも
/// fail-closed でも同じ——読み込みが error へ確定しない限り、どちらの
/// 方針でも `ready` の解決を待つしかない。フォント待ちに独立の短い上限
/// （例: 数秒）を持たせて被害を読みやすくするのは**未着手**である
/// （README のフォント節にも同じ限界を明記）。
///
/// **既知の限界（load イベントの門。cmd_663 ⑥）**: `document.fonts.ready` は
/// 仕様上「document の load イベント完了＋読み込み中フォント無し」で解決
/// する——費用はフォントの無応答に限らない。フォントが全て `loaded` でも、
/// 到達不能なホストへの `<script src>` / `<img>` / beacon が 1 本あるだけで
/// `ready` は pending のまま全 story がこの時間ずつ失敗する。走査が降りる
/// 同一オリジン iframe では **iframe 側の load イベント**も同じ門になる。
/// 条件待ち導入前（`storyRendered`＋250ms）はどちらも撮れていた——
/// フォント CDN 無応答と同じ費用の項として README にも明記。
pub const DEFAULT_STORY_TIMEOUT: Duration = Duration::from_secs(30);
/// 描画完了を検出してから撮るまでの落ち着き待ち。
///
/// かつての注記は「フォント・アニメーションの初期化ぶん」だったが、どちらも
/// もうこの時間待ちの担当ではない——アニメーションは #19 の条件つき静止
/// （`FREEZE_SCRIPT`・fail-closed）、フォントは [`FONTS_WAIT_SCRIPT`] の
/// `document.fonts.ready` 条件待ち（cmd_656・fail-closed）がそれぞれ
/// 引き受けた。時間待ちは「間に合ったか」がキャッシュの温度や負荷で変わり、
/// 同じビルドから違う絵を作る（cmd_656 で実測）。
///
/// 残しているのは、完了シグナルも条件待ちも未整備の非同期初期化——画像の
/// 読み込み・デコードなど、`storyRendered` の後に絵を変えうるがどの層も
/// まだ待っていないもの——への暫定の緩衝としてである。これを外すのは、
/// それらにも「何を待つか」が明示された条件待ちを整えてから（cmd_656 と
/// 同型の実測つきで）行うこと。時間待ちを条件待ちの代わりに数えてはならない。
pub const SETTLE_DELAY: Duration = Duration::from_millis(250);
/// 描画完了判定のポーリング間隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Storybook ランタイムの起動を待つ猶予。
///
/// これを過ぎてもチャンネルもプレビューも現れないバンドルは
/// 「Storybook ランタイムを持たない」と判断して DOM ヒューリスティックに落ちる。
/// 短すぎるとランタイム起動前の DOM で誤判定し、長すぎると手書きバンドルが遅くなる。
pub const SIGNAL_GRACE: Duration = Duration::from_millis(1500);

/// Chromium 起動の websocket URL 解決を待つタイムアウト。
///
/// chromiumoxide の既定は 20 秒。CI では 3 本のジョブが同時にコールドスタートし、
/// 負荷とコンテナの遅さでこの 20 秒に間に合わず `LaunchTimeout` を踏むことがある。
/// 余裕を持たせて広げておく。
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(45);
/// 起動の最大試行回数（初回 + リトライ）。
const LAUNCH_MAX_ATTEMPTS: u32 = 3;

/// ナビゲーション前に document へ注入するフック。
///
/// `__STORYBOOK_ADDONS_CHANNEL__` はプレビューランタイムが**後から**代入するので、
/// アクセサを先に張っておいて代入の瞬間にリスナーを差し込む。
/// こうしないと `storyRendered` を取りこぼす（イベントは再送されない）。
const READY_HOOK_SCRIPT: &str = r#"
(() => {
  if (window.__VRT_READY__) return;
  // rendered / error には「どの document で観測したか」の世代
  // （その時点の documentElement への参照）を併記する。accessor は window に
  // しか張れないが、state 自体は document 側へも置ける（フォントの印 dataset
  // と同じ分担）——window に置いたままにしたのは判断である（cmd_663 ⑧）:
  // errorRoot / renderedRoot は documentElement への参照で dataset（文字列
  // のみ）には刻めず、世代印だけで差し替えの生き残りは塞がっている。
  // `document.open()` / `document.write()` はグローバルオブジェクトを維持
  // したまま document を差し替えるため、印だけ window に残ると、やり直しの
  // READY 待ちが前 document の rendered: true で素通りする（cmd_661 ①。
  // フォントの印を document 側へ移したのと同じ前提から導かれる）。イベントは
  // 発火のたびにその時点の documentElement を刻むので、差し替え後に生き残った
  // channel へ再シグナルが来れば新しい世代で立ち直る。
  const state = { rendered: false, renderedRoot: null, error: null, errorRoot: null };
  window.__VRT_READY__ = state;

  const describe = (payload) => {
    if (payload == null) return 'unknown error';
    if (typeof payload === 'string') return payload;
    const err = payload.error || payload;
    const message = err.message || err.title || err.name;
    if (message) return String(message);
    try { return JSON.stringify(payload); } catch (e) { return String(payload); }
  };

  // error は「同じ document の中では先着固定」（最初の失敗が根本原因で、
  // 続く失敗は巻き添えのことが多い）だが、**先着の判定は世代の中で閉じる**。
  // 素朴な `if (!state.error)` だと、前 document で立った error が新 document
  // の error の記録を塞ぐ——読む側（READY probe）は世代不一致の error を
  // 読まないので、新 document の error は誰にも観測されず、rendered だけが
  // 新世代で立って撮影が通る（rendered は書く側も読む側も世代を見るのに、
  // error は読む側だけが見る非対称だった。cmd_662 ②）。
  const recordError = (message) => {
    const current = document.documentElement;
    if (!state.error || state.errorRoot !== current) {
      state.error = message;
      state.errorRoot = current;
    }
  };

  const attach = (channel) => {
    if (!channel || typeof channel.on !== 'function' || channel.__vrtAttached) return;
    channel.__vrtAttached = true;
    channel.on('storyRendered', () => {
      state.rendered = true;
      state.renderedRoot = document.documentElement;
    });
    // play 関数の例外は「描画は終わったが検証に失敗した」状態。撮影より診断を優先する。
    for (const event of [
      'storyErrored',
      'storyThrewException',
      'playFunctionThrewException',
      'unhandledErrorsWhilePlaying',
    ]) {
      channel.on(event, (payload) => {
        recordError(event + ': ' + describe(payload));
      });
    }
    channel.on('storyMissing', (id) => {
      recordError('storyMissing: ' + String(id));
    });
  };

  let channel = window.__STORYBOOK_ADDONS_CHANNEL__;
  try {
    Object.defineProperty(window, '__STORYBOOK_ADDONS_CHANNEL__', {
      configurable: true,
      get: () => channel,
      set: (value) => { channel = value; attach(value); },
    });
  } catch (e) {
    // 定義できない環境ではポーリング側の保険（storyRenders）に任せる。
  }
  attach(channel);
})()
"#;

/// 描画状態を問い合わせる式。JSON 文字列を返す（`state` は ready / error / pending / absent）。
///
/// `absent` は「Storybook ランタイムが見当たらない」。呼び出し側が
/// [`SIGNAL_GRACE`] 経過後に `dom_ready` を見て判断する。
const READY_PROBE: &str = r#"
JSON.stringify((() => {
  const loaded = document.readyState === 'complete' || document.readyState === 'interactive';
  const root = document.querySelector('#storybook-root') || document.querySelector('#root');
  const domReady = !!root && (root.childElementCount > 0 || (root.textContent || '').trim().length > 0);

  // rendered / error は「現在の document で観測したもの」だけを信じる。
  // hook の state は window 上で document.open() / document.write() を
  // 生き延びるため、世代（観測時の documentElement）が現在と一致しない印は
  // 前 document の残骸——読まずに待ち続ける（cmd_661 ①）。
  const hook = window.__VRT_READY__;
  const current = document.documentElement;
  if (hook && hook.error && hook.errorRoot === current) {
    return { state: 'error', message: String(hook.error) };
  }
  if (hook && hook.rendered && hook.renderedRoot === current) return { state: 'ready' };

  // フックがチャンネルを掴めなかったときの保険。SB 8〜10 のプレビューは
  // 進行中/完了したレンダーを storyRenders に持ち、phase で状態を出す。
  try {
    const renders = window.__STORYBOOK_PREVIEW__ && window.__STORYBOOK_PREVIEW__.storyRenders;
    if (Array.isArray(renders) && renders.length > 0) {
      const phase = renders[renders.length - 1].phase;
      if (phase === 'completed') {
        // storyRenders は Storybook 自身が window に置く状態で、hook と違い
        // 世代を刻んでいない（内部実装への書き込みは採らない判断——モジュール
        // doc の常駐状態表を参照）——前 document の completed が差し替えを
        // 生き延びる。現在の document の root に描画結果があることを併せて
        // 要求する（root つきの内容へ差し替える形は依然すり抜ける。既知の
        // 限界としてモジュール doc の失敗経路表に明記）。
        //
        // この門の代償（cmd_663 ⑤・errored 分岐の判断の逆向きの帰結）:
        // 何も描かず終える story は正当（このモジュールの出発点の
        // PasswordStrengthBar）だが、保険経路にしか乗れない構成（channel を
        // 掴み損ねた等）では null を返す story・portal で body 直下へ描く
        // story の domReady が永久に false になり、門の導入前は撮れていた
        // ものが 30 秒 Timeout に倒れる。向きは fail-closed（stale completed
        // で未検証の絵を撮る fail-open を塞ぐ側）だが巻き添えである——
        // root の中身に依存しない突き合わせ（completed 観測時の世代を Rust
        // 側で覚える等）は検証世代の一般化そのもので、#27 の範囲外として
        // 併記に留める（モジュール doc「本 PR で扱わないもの」）。
        if (domReady) return { state: 'ready' };
        return { state: 'pending', dom_ready: domReady };
      }
      if (phase === 'errored' || phase === 'aborted') {
        // こちらには completed 側の domReady のような門を**掛けない**（判断。
        // cmd_662 ③）。理由は二つ。(1) 失敗の向き: 前 document の stale な
        // errored が読まれても、起きるのは「誤った理由での story 失敗」で
        // あって未検証の絵の撮影ではない——fail-closed 側にしか倒れない
        // （completed 側の門は fail-open——未描画の差し替え document を撮る
        // ——を塞ぐためにある。門の要否は向きで決まる）。(2) domReady は
        // error の門として意味を成さない: 何も描かず終える story は正当
        // （このモジュールの出発点の PasswordStrengthBar）で、errored story の
        // root が空なのも普通——domReady を要求すると、本物のエラー通知を
        // 黙らせて 30 秒の Timeout に劣化させる。世代を刻む口が無いのは
        // completed と同じ（モジュール doc の常駐状態の表を参照）。
        return { state: 'error', message: 'render phase: ' + String(phase) };
      }
      return { state: 'pending', dom_ready: domReady };
    }
  } catch (e) {
    // プレビュー内部形が変わっていても致命ではない。シグナル待ちを続ける。
  }

  const runtime = !!(window.__STORYBOOK_ADDONS_CHANNEL__ || window.__STORYBOOK_PREVIEW__);
  if (runtime) return { state: 'pending', dom_ready: domReady };
  return { state: 'absent', dom_ready: domReady && loaded };
})())
"#;

/// 撮影直前にページを**決定的な静止状態**へ持ち込むスクリプト。
///
/// キャレットの明滅とアニメーションは「いつ撮ったか」で絵が変わる、VRT にとって
/// 純粋な雑音である。しきい値（`diff_ratio_fail`）を緩めて誤差を許すのではなく、
/// 撮影の入力自体から時刻依存を消す。利用者の Storybook / preview には一切
/// 手を入れず、レンダラがナビゲーション後のページに注入する。
///
/// ## なぜ「決定的」といえるか
///
/// - **キャレット**: `caret-color: transparent !important` で明滅の位相に
///   よらず不可視にする（Playwright の `caret: 'hide'` も同じプロパティ強制で
///   実現している。ただしあちらは編集要素へのインライン指定、こちらは
///   スタイルシート注入）。
///   `caret-color` は継承プロパティだが、継承値はカスケードでは最弱で、
///   shadow 内に `caret-color` を明示した要素には勝てない。ゆえに継承には
///   頼らず、静止 CSS を document と各 open shadow root のそれぞれへ
///   constructed stylesheet（`new CSSStyleSheet()` + `replaceSync` +
///   `adoptedStyleSheets`）として直接注入する。CSSOM 操作は CSP `style-src`
///   の管轄外なので、`<style>` の appendChild と違い CSP 起因の適用失敗が
///   そもそも起きない（sheet は document 単位に 1 つ構築し、同一 document の
///   全 root で共有する。別 document——same-origin iframe——には
///   その realm の `CSSStyleSheet` で構築し直す。cross-document 共有は
///   `NotAllowedError` になるためである）
/// - **有限アニメーション**（CSS animation / CSS transition / Web Animations API）:
///   `currentTime = endTime` へシークして pause。終端は仕様上ただ一つに
///   定まる状態（`fill: forwards` なら最終キーフレーム、無指定なら基底スタイル）
///   であり、壁時計に依存しない。`endTime` が `CSSNumericValue`（percent 等）の
///   場合は同じ型のまま `currentTime` へ渡す——数値変換すると progress-based
///   timeline で `TypeError` になる
/// - **無限アニメーション**: 終端が存在しないので `currentTime = 0` へ巻き戻して
///   pause。タイムライン座標 0 の絵はこれもただ一つに定まる。ここでも型は保つ
///   ——progress-based timeline の `currentTime` は `CSSNumericValue` しか
///   受け付けないため、`endTime` かタイムライン現在値が単位つきなら
///   `CSSUnitValue(0, unit)` を渡す（infinite + scroll timeline の正当な
///   story を数値 0 の `TypeError` で落とさない）。
///   「paused にするだけ」では**止まった位置**が撮影タイミング依存のままで、
///   flaky さは消えない——座標を明示的に固定するからこそ 2 回撮って同じ絵になる
/// - **今後始まる transition**: `transition-duration: 0s` と `transition-delay: 0s`
///   の下では、スタイルが変わっても transition は**そもそも生成されない**
///   （CSS Transitions Level 1 の開始条件は combined duration
///   （= max(duration, 0s) + delay）が 0s より大きいことを要求する。
///   <https://www.w3.org/TR/css-transitions-1/#starting>）。プロパティ値は
///   即座に終値へ変わるが、transition が存在しない以上
///   `transitionrun` / `transitionstart` / `transitionend` は**いずれも発火しない**。
///   一方、**注入の時点ですでに走っていた transition** は上の有限アニメーションの
///   経路で終端へシークされ、完了として `transitionend` を**発火する**（Chromium
///   実測）。この境界はモジュール末尾の
///   `transitions_under_freeze_fire_no_events` が両方向とも固定している。
///   ゆえに注入後に始まる transition の `transitionend` を待って見た目を
///   更新するコンポーネントには届かない——
///   その代償と採否の理由は README の「届かない範囲」を参照。
///   `transition-duration` は継承しないプロパティなので、これも root ごとの
///   注入があって初めて shadow 内に届く
///
/// ## 収束と fail-closed
///
/// シーク後に rAF を 2 回待って合成済みフレームへ反映させ、新たに始まった
/// アニメーションがあれば同じ手順で再固定する。これを**安定状態まで反復**し、
/// 上限 10 巡以内に収束しなければ失敗を返す（上限の根拠: 実測では通常の
/// Storybook ページは 2 巡で収束する。`animationend` ハンドラが次を開始する
/// 連鎖でも、連鎖の長さがページ内の要素数を超えることはないため 10 巡は
/// 十分な余裕。連鎖が 10 段を超えるなら Storybook 側で止めるべきである）。
///
/// 収束の判定は running な animation が**ゼロになったか**で行う。
/// ゼロになれば即座に抜け、残っていれば MAX_SWEEPS まで反復を続ける。
/// identity（animation 名と対象要素の組み）による早期停止は行わない——
/// proxy（tagName 等）では要素の同一性を表せず、異なる要素への順移動
/// （同一 keyframes が el1→el2→el3→el4）を誤判定するためである。
///
/// 最終巡回後、全 root の running な animation を数え、1 つでも残っていれば
/// **失敗の JSON を返す**——何が凍らせられなかったかを含む形で上へ伝える。
/// 全て止まっていれば、注入 sheet が実際に各 root のカスケードへ入ったかを
/// `getComputedStyle` の `--vrt-frozen` プローブで検証してから成功の JSON を
/// 返す。「代入できた」は「効いた」の証明ではないため、操作の成功ではなく
/// 効果の実測を以て判定する（root ごとに 1 要素で足りる——sheet の存在は
/// root 単位の性質であり、`--vrt-frozen: 1` は既定値と偶然一致しえない。
/// 個別要素の `!important` 上書きはここで検証**しない**——それは README
/// 「届かない範囲」の best-effort 契約であり、ハード失敗させると契約と
/// 食い違う）。CSP を `Page.setBypassCSP` で迂回すると本番と異なる絵を撮る
/// ことになるため、迂回は行わない——そもそも CSSOM 経由の注入は CSP の
/// 管轄外なので、迂回する理由も無い。
///
/// **検証層自身の失敗も fail-closed である**。`getComputedStyle` が throw
/// する（ページ側の差し替え・壊れた環境）など、注入・シーク・収集・検証の
/// どの層でも `errors` に積まれた失敗が 1 件でもあれば、最後に `ok: false`
/// を返して撮影を止める。`errors` は診断の置き場ではなく判定の入力である
/// ——「検証できなかった」を「検証を通った」へ倒さない。唯一の例外は
/// クロスオリジン iframe への到達不能で、これは原理的に触れない
/// 「届かない範囲」として README に明記してある。各層の失敗経路の全体は
/// モジュール先頭の「層ごとの失敗経路」を参照。
///
/// open shadow root と同一オリジン iframe（shadow 内にあるものも含む）は
/// root 単位で再帰的に辿り、CSS 注入とアニメーションのシークを行う。
/// closed shadow root、クロスオリジン iframe、canvas / rAF 駆動の JS
/// アニメーション、`getAnimations()` に載らないブラウザネイティブの時間変化
/// （アニメーション画像・メディア再生・SVG SMIL・smooth scroll 等——Chromium
/// 実測で不可視を確認）、および利用者側が `!important` で宣言した
/// `caret-color` / `transition` には届かない
/// （モジュール末尾のテストと README「届かない範囲」を参照）。
const FREEZE_SCRIPT: &str = r#"
(async () => {
  const MAX_SWEEPS = 10;
  // --vrt-frozen は静止 CSS の適用検証プローブ。caret-color や
  // transition-duration と違い既定値が '1' になることはないため、
  // 「偶然その値だった」による検証の素通しが起きない。
  const CSS = [
    '*, *::before, *::after {',
    '  caret-color: transparent !important;',
    '  transition-duration: 0s !important;',
    '  transition-delay: 0s !important;',
    '  --vrt-frozen: 1 !important;',
    '}',
  ].join('\n');
  const errors = [];
  // 収束の反復で同じ層が同じ失敗を繰り返すと、errors が巡回数ぶん水増しされて
  // 原因が読みにくくなる。判定に効くのは件数でなく有無なので、同文は 1 回だけ積む。
  const pushError = (msg) => { if (!errors.includes(msg)) errors.push(msg); };
  const frozenRoots = [];
  // 静止 sheet は document 単位に 1 つ構築し、同一 document の全 root
  //（document 自身と open shadow root）で共有する。constructed stylesheet は
  // 構築元の document に紐づき、別 document への adopt は NotAllowedError に
  // なるため、same-origin iframe にはその realm の CSSStyleSheet で作り直す。
  const sheetByDoc = new Map();

  // endTime が CSSNumericValue（progress-based timeline 等）か数値かを判定し、
  // 有限なら終端値を、無限なら null を返す。
  const finiteEnd = (timing) => {
    if (!timing) return null;
    const end = timing.endTime;
    if (typeof end === 'number') return Number.isFinite(end) ? end : null;
    if (end && typeof end === 'object' && typeof end.value === 'number') {
      return Number.isFinite(end.value) ? end : null;
    }
    return null;
  };

  // 1 つの root の中の animation と要素を数える共通収集。freezeRoot（凍らせる側）と
  // collectRunning（残りを数える側）の両方がここを通ることで、視野の非対称
  // ——凍らせた範囲と数えた範囲のずれ——を構造的に消す。
  //
  // Chromium の root.getAnimations()（Document / ShadowRoot）はその root の
  // subtree 全体の animation を擬似要素ぶんも含めて返すので、root 側が
  // 成功したら per-element の getAnimations() は全要素ぶんの no-op になる
  //（2000 ノードのページで walk ごと約 2000 回、freezeRoot と collectRunning
  // の両方が通るゆえ sweep あたりその倍）。per-element の走査は root 側の
  // API が欠落・throw した場合だけのフォールバックとする。その場合は
  // 擬似要素のアニメを数える口が無く網羅を保証できない——黙って倒すと
  // 「running 無し」と区別できず偽の成功になるので、errors に積んで判定へ
  // 反映した上で（fail-closed）、数えられる範囲は診断のために集める。
  const collectAnimations = (root) => {
    const animations = new Set();
    let elements = [];
    if (!root) return { animations, elements };
    try { elements = root.querySelectorAll('*'); } catch (e) {
      pushError('querySelectorAll failed: ' + String(e && e.message || e));
    }
    let rootEnumerated = false;
    if (typeof root.getAnimations === 'function') {
      try {
        for (const a of root.getAnimations()) animations.add(a);
        rootEnumerated = true;
      } catch (e) {
        pushError('getAnimations failed on root: ' + String(e && e.message || e));
      }
    } else {
      pushError('getAnimations API missing on a root: pseudo-element animations cannot be enumerated');
    }
    if (!rootEnumerated) {
      for (const el of elements) {
        try { for (const a of el.getAnimations()) animations.add(a); } catch (e) {
          pushError('getAnimations failed on element: ' + String(e && e.message || e));
        }
      }
    }
    return { animations, elements };
  };

  // root は Document または ShadowRoot。どちらも同じ手順で静止させる。
  // caret-color は継承プロパティだが、継承値は shadow 内で明示された宣言に
  // 勝てず、transition-duration はそもそも継承しない。ゆえに静止 CSS は
  // 継承に頼らず root ごとに constructed stylesheet として adopt する。
  // CSSOM 操作は CSP style-src の管轄外なので、<style> の appendChild と
  // 違い CSP に黙殺される経路が存在しない。adopted sheet は同 root の
  // <style>/<link> より後の順序に置かれるため、同 specificity の !important
  // 同士なら注入側が勝つ——利用者側がより高い specificity で !important を
  // 宣言した場合に負けるのは <style> 注入と同じ（README「届かない範囲」）。
  // 走査（open shadow root への降下・`iframe` / `frame` の contentDocument への
  // 降下）の共有ヘルパ。凍らせる側（freezeRoot）と数える側（collectRunning）は
  // 従来この walk を各自に持つ写しで、片側だけ狭まる退行——「凍らせていないのに
  // 数えもしない」fail-open——をどの試験も赤くできなかった（cmd_663 ⑦）。
  // fonts 側の走査を COLLECT_DOCUMENTS_JS へ一本化した（cmd_662 ④）のと同じ
  // 理由で、降下はこの一箇所だけが担う。perRoot が root ごとの仕事（注入＋
  // シーク／running の収集）を行い、collectAnimations が走査済みの全要素を返す。
  const walkRoots = (root, perRoot) => {
    if (!root) return;
    const elements = perRoot(root);
    for (const el of elements) {
      if (el.shadowRoot) walkRoots(el.shadowRoot, perRoot);
      if (el.localName === 'iframe' || el.localName === 'frame') {
        // クロスオリジン iframe には原理的に触れない（contentDocument は
        // null / throw）。ここの握りつぶしは意図的で、errors に積まない。
        try { walkRoots(el.contentDocument, perRoot); } catch (e) {}
      }
    }
  };

  const freezeRoot = (root) => walkRoots(root, (r) => {
    try {
      const doc = r.nodeType === Node.DOCUMENT_NODE ? r : r.ownerDocument;
      const win = doc && doc.defaultView;
      // browsing context を持たない document（切り離された iframe 等）は
      // 描画されないので注入も検証も対象外（検証側の disconnected スキップと同じ扱い）。
      if (win) {
        let sheet = sheetByDoc.get(doc);
        if (!sheet) {
          if (typeof win.CSSStyleSheet !== 'function') {
            // 注入層自身の失敗も fail-closed——構築できないまま黙って進むと
            // 「注入できたか不明」の絵を成功として撮ることになる。
            pushError('CSSStyleSheet constructor missing: the freeze CSS cannot be injected');
          } else {
            sheet = new win.CSSStyleSheet();
            sheet.replaceSync(CSS);
            sheetByDoc.set(doc, sheet);
          }
        }
        if (sheet && !r.adoptedStyleSheets.includes(sheet)) {
          r.adoptedStyleSheets = [...r.adoptedStyleSheets, sheet];
          frozenRoots.push(r);
        }
      }
    } catch (e) {
      // CSS 注入の失敗は静止の前提を崩すので記録する。
      pushError('CSS injection failed: ' + String(e && e.message || e));
    }

    // document.getAnimations() は shadow tree の中を返さない実装があるため、
    // root 単位で collectRunning と同じ共通収集を通す（Set で重複は消える）。
    const { animations, elements } = collectAnimations(r);

    for (const anim of animations) {
      try {
        const timing =
          anim.effect && anim.effect.getComputedTiming
            ? anim.effect.getComputedTiming()
            : null;
        const end = finiteEnd(timing);
        // 有限は終端へ、無限は初期（タイムライン座標 0）へ。どちらも
        // 壁時計に依存しない一意な座標なので、2 回撮っても同じ絵になる。
        // end が CSSNumericValue の場合はそのまま渡す（型を保って seek）。
        if (end != null) {
          anim.currentTime = end;
        } else {
          // 無限アニメの 0 も型を保つ。progress-based timeline（scroll() 等）の
          // currentTime は CSSNumericValue しか受け付けず、数値 0 の代入は
          // TypeError になる——endTime か timeline の現在値が単位つきなら
          // 同じ単位の 0 を作って渡す（infinite + scroll timeline の正当な
          // story を落とさないため）。
          const unitOf = (v) =>
            v && typeof v === 'object' && typeof v.unit === 'string' ? v.unit : null;
          const unit = unitOf(timing && timing.endTime)
            || unitOf(anim.timeline && anim.timeline.currentTime);
          anim.currentTime = unit != null ? new CSSUnitValue(0, unit) : 0;
        }
        anim.pause();
      } catch (e) {
        pushError('seek/pause failed: ' + String(e && e.message || e));
      }
    }
    return elements;
  });

  // 全 root から running な animation を集める。収集の視野は freezeRoot と
  // 同じ collectAnimations——収集失敗・API 欠落はそこで errors に積まれ、
  // 「残っていない」と「数えられなかった」が混ざらない（fail-closed）。
  // 降下は freezeRoot と共有の walkRoots（cmd_663 ⑦）。
  const collectRunning = (root) => {
    const running = [];
    walkRoots(root, (r) => {
      const { animations, elements } = collectAnimations(r);
      for (const a of animations) {
        if (a.playState === 'running') {
          const name = a.animationName || a.transitionProperty || a.id || '';
          const target = (a.effect && a.effect.target && a.effect.target.tagName) || 'unknown';
          running.push(name + ':' + target);
        }
      }
      return elements;
    });
    return running;
  };

  const nextFrame = () =>
    new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));

  // sweeps は「実際に回した巡回数」。while の頭で加算するため、上限で抜けた
  // ときも MAX_SWEEPS を超えない（旧 for 形は脱出後に 11 となり、失敗
  // メッセージが実際の 10 巡と食い違う off-by-one を持っていた）。
  let sweeps = 0;
  let still = [];
  while (sweeps < MAX_SWEEPS) {
    sweeps++;
    freezeRoot(document);
    await nextFrame();
    still = collectRunning(document);
    // 最低 2 巡は回す。注入 CSS は caret-color と transition-duration/delay
    // しか設定しないため、それ自体が新しい animation を誘発することはない
    //（combined duration 0 の transition はそもそも生成されない）。2 巡目の
    // 価値は別にある: 1 巡目の seek が発火させる animationend / transitionend
    // ハンドラが開始する連鎖と、double-rAF の待ちの間に遅延開始した animation
    // の捕捉である。ゆえに「1 巡目が 0 件なら 2 巡目を省ける」は成り立たない
    // ——0 件の巡でもイベント由来の新規開始は次の巡でしか見えない。
    if (still.length === 0 && sweeps >= 2) break;
  }

  // 最終巡の collectRunning からここまで await が無く、イベントループへ戻って
  // いない。タイムラインの現在時刻はタスク内で固定され（Web Animations 仕様）、
  // 新しい animation の開始もスクリプト実行を要するため、ここで再収集しても
  // 同じ集合が返る——still をそのまま最終判定に使い、二重の全ツリー走査を省く。
  if (still.length > 0) {
    return JSON.stringify({
      ok: false,
      sweeps: sweeps,
      running: still,
      errors: errors.length > 0 ? errors : undefined,
    });
  }
  // 注入 sheet が実際に各 root のカスケードへ入ったか検証する。
  // 「adoptedStyleSheets へ代入できた」は「効いた」の証明ではないため、
  // 効果を実測する。見るのは --vrt-frozen プローブだけ:
  // - caret-color / transition-duration を見ないのは、利用者側の !important
  //   上書き（README「届かない範囲」の best-effort 契約）をハード失敗に
  //   格上げしないため。sheet がカスケードに入っているか——検証したい命題
  //   ——は root 単位の性質なので、root ごとに 1 要素のサンプルで足りる
  // - プローブは既定値を持たないので、「たまたま期待値だった」ことによる
  //   検証の素通し（旧実装の fail-open）も起きない
  for (const root of frozenRoots) {
    try {
      // 検証時点で切り離された root（除去された iframe の document・
      // 切断された host の shadow root）は描画に影響しない上、computed
      // style が空になって偽の失敗を生むので検証対象から外す。
      if (root.nodeType === Node.DOCUMENT_NODE ? !root.defaultView : !root.isConnected) continue;
      const el = root.querySelector('*');
      if (!el) continue;
      const probe = getComputedStyle(el).getPropertyValue('--vrt-frozen').trim();
      if (probe !== '1') {
        return JSON.stringify({
          ok: false,
          sweeps: sweeps,
          reason: 'CSS not applied: the freeze probe --vrt-frozen is missing on a sampled element'
            + ' (got: ' + JSON.stringify(probe) + ')',
          errors: errors.length > 0 ? errors : undefined,
        });
      }
    } catch (e) {
      // 検証自体の失敗。ここで積んだエラーは下の errors 判定で
      // 失敗として返る——「検証できなかった」を成功へ倒さない。
      pushError('CSS verification failed: ' + String(e && e.message || e));
    }
  }
  // errors は診断の置き場ではなく判定の入力である。1 件でも積まれていれば
  // 「静止できたと確かめられていない」状態なので、撮影へ進ませず失敗を
  // 返す（fail-closed）。注入・シーク・収集・検証のどの層の失敗もここで
  // 拾われる——集めるだけで見ない fail-open を残さない。
  if (errors.length > 0) {
    return JSON.stringify({
      ok: false,
      sweeps: sweeps,
      reason: 'freeze layer reported ' + errors.length + ' internal error(s)',
      errors: errors,
    });
  }
  return JSON.stringify({ ok: true, sweeps: sweeps });
})()
"#;

/// reduced-motion エミュレーションが実際に効いているかを撮影直前に実測するプローブ。
///
/// `Emulation.setEmulatedMedia` の応答が成功でも、「メディアクエリが実際に
/// 変わった」ことの証明にはならない——できたことは効いたことの証明ではない。
/// 二輪で実測する:
///
/// 1. **CSS 輪**: constructed stylesheet の `@media (prefers-reduced-motion:
///    reduce)` 内でだけ立つカスタムプロパティ（`--vrt-reduced-motion`）を
///    `getComputedStyle` で読む。CSS カスケードの実評価であり、
///    `matchMedia` 関数オブジェクトには依存しない
/// 2. **matchMedia 輪**: `window.matchMedia('(prefers-reduced-motion:
///    reduce)').matches === true`。JS 実装が実際に参照する観測面であり、
///    reduce を返さない壊れた/モックされた `matchMedia`（polyfill・テスト
///    ダブルの事故）をここで検出する——CSS 輪だけでは「CSS には効いたが
///    JS には見えない」ページを素通ししてしまう
///
/// どちらか一方でも不成立なら `ok: false`（fail-closed）。プローブ自体の
/// throw も `errors` → `ok: false` で、集めた診断は判定にも表示にも使う。
/// 両輪とも偽装するページは原理的に検出できない（モジュール doc の表を参照）。
const REDUCED_MOTION_PROBE: &str = r#"
(() => {
  const errors = [];
  let cssApplied = false;
  let mmMatches = false;
  try {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(
      '@media (prefers-reduced-motion: reduce) { :root { --vrt-reduced-motion: on; } }'
    );
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
    cssApplied =
      getComputedStyle(document.documentElement)
        .getPropertyValue('--vrt-reduced-motion')
        .trim() === 'on';
    document.adoptedStyleSheets = document.adoptedStyleSheets.filter((s) => s !== sheet);
    if (!cssApplied) {
      errors.push('the reduce media query did not apply to the CSS cascade');
    }
  } catch (e) {
    errors.push('css probe: ' + String(e));
  }
  try {
    mmMatches = window.matchMedia('(prefers-reduced-motion: reduce)').matches === true;
    if (!mmMatches) {
      errors.push(
        'matchMedia does not report reduce (the emulation was not applied, or matchMedia is broken or mocked)'
      );
    }
  } catch (e) {
    errors.push('matchMedia probe: ' + String(e));
  }
  return JSON.stringify({ ok: cssApplied && mmMatches, errors: errors });
})()
"#;

/// webfont の読み込み完了を条件待ちするスクリプト。
///
/// `storyRendered` はフォントを待たずに出る——フォント読み込みは style/layout が
/// 要求してから走る非同期処理で、描画完了シグナルの管轄外にある。かつては
/// [`SETTLE_DELAY`]（250ms の時間待ち）がこれを吸収するつもりでいたが、
/// 「間に合えば本来の字形・間に合わねば代替字形」という競争をそのまま
/// 撮っていた（cmd_656 で実測。同じバンドルを同じブラウザで繰り返し撮ると
/// 序盤の撮影だけがバイト不一致になり、不一致の絵はすべて
/// `document.fonts.check()` 不成立＝フォント未着だった）。時間待ちは
/// キャッシュの温度で勝敗が変わる。`document.fonts.ready` は時間ではなく
/// 「読み込み中のフォントが無くなった」という**状態**を待つので、温度に依らない。
///
/// 返り値は JSON 文字列。`ready` の解決後に `status` を読み直し、
/// `'loaded'` と確かめられた場合だけ先へ進む。解決と応答の間に新たな
/// フォント要求が始まっていたら（`'loading'` に戻っていたら）失敗にはせず、
/// `ready` を**待ち直す**——二段以上でフォントを読むページ（本文フォントの
/// 後にアイコンフォント等）は各波が有限なら間もなく収束し、そこから先は
/// 決定的に撮れる。ここで即失敗にすると、そうしたページを**毎回・恒久的に**
/// 撮影不能へ誤分類する（cmd_657 で実測: 二波 fixture は旧実装で 8/8 失敗した）。
///
/// 波が尽きたら、個々の `FontFace.status === 'error'` を列挙する。読み込みに
/// **失敗**したフォントがあっても撮影は止めず、family を Set で一意化した
/// 一覧を `failed` として返す——Rust 側（[`fonts_verdict`]）が警告に整形する
/// （意図した fail-open。cmd_658 の (a) fail-closed から cmd_659 で (b) へ
/// 転換した。理由: 原因の `@font-face` は preview-head 等で **project 全体に
/// 共有**され、失敗判定にすると egress の無いワーカーの外部フォント参照や
/// `local()` 前提の宣言ひとつで、そのフォントを表示しない story まで全 story
/// が落ちる。採否の害の比較はモジュール doc 失敗経路①を参照）。列挙が
/// throw する形（部分的な差し替え等）は観測不能への転落なので従来どおり
/// `ok: false`（fail-closed）。
///
/// 到達可能な**同一オリジン iframe を再帰**し、top document と各
/// `contentDocument` の fonts をまとめて待つ——走査範囲は FREEZE の
/// `freezeRoot` と同じ（open shadow root へ潜り、`localName` で `iframe` と
/// `frame` の両方を見る。cmd_661 ②）であり、フォント検証はその鏡である。
/// クロスオリジン iframe は `contentDocument` が null で**原理的に観測
/// できない**（README「届かない範囲」の契約。FREEZE の書き方に揃える）。
/// 走査は freezeRoot の **`defaultView` の門**も写す（cmd_663 ②）——
/// browsing context の無い document（切り離された iframe 等）は描画されず、
/// その `fonts.ready` は二度と settle しないので、集めない。待ちの**最中**に
/// 切り離された document は、各 `ready` と race するポーリングが検知して
/// 待ちからも判定からも捨てる（詳細は `settle()` のコメント）。
///
/// 成功時は検証した各 document の
/// `documentElement.dataset.vrtFontsVerified = 'true'` の印を残す
/// （`dataset` を持たない documentElement——素の XML document——には刻む
/// 口が無いのでスキップし、再確認側も同じ条件で印を要求しない）。印を
/// window ではなく **document 側**に置くのは、`document.open()` /
/// `document.write()` がグローバルオブジェクトを維持したまま document を
/// 差し替えるため——window の印は差し替えを生き延びてしまい、撮影直前の
/// 再確認（[`FONTS_RECHECK_PROBE`]）が入れ替わりを見逃す（cmd_660 実測）。
/// documentElement は ナビゲーションでも `document.open()` でも作り直される
/// ので、どちらの入れ替わりでも印は消える。**同一 document 内の DOM 全面
/// 置換（`body.replaceChildren` 等）では documentElement が残るため印も
/// 残る**——この形は印では捉えられない（モジュール doc 失敗経路⑤を参照）。
/// 印は「検証した document」の要素参照へ直接書く——完了時の `document`
/// グローバルを読み直すと、待ちの間に差し替わった**未検証の** document へ
/// 印を付けてしまう。
///
/// `document.fonts` が欠落・非 FontFaceSet 形なら（iframe 側も含め）待たずに
/// `ok: false`（fail-closed。無いものを待てないが、無いことは黙って通さない）。
/// `ready` が解決しない・波が尽きないページはこの evaluate 自体が返らず、
/// Rust 側の共有 deadline（[`evaluate_with_deadline_retry`]）が期限で
/// [`RenderError::Timeout`] へ倒す（fail-closed——撮らない）。
/// 検証側（[`FONTS_WAIT_SCRIPT`]）と再確認側（[`FONTS_RECHECK_PROBE`]）が
/// **同じ文字列を注入して**共有する document 走査。到達可能な同一オリジン
/// iframe を再帰して document を集める。走査範囲は FREEZE の `freezeRoot` と
/// 同じ（cmd_661 ②）——`querySelectorAll` は shadow 境界を越えないため
/// open shadow root ごとに潜り、`localName` で `iframe` と `frame` の両方を
/// 見る。cross-origin は `contentDocument` が null（観測不能——README
/// 「届かない範囲」の契約）。走査自体の throw は各利用側の try に届いて
/// 失敗へ倒れる（fail-closed）。
///
/// 二本のスクリプトへ**別々に写す形は採らない**（cmd_662 ④）。待った範囲と
/// 確かめる範囲の一致は再確認の前提だが、`wait_for_fonts = false` の陽性対照は
/// 待ちと再確認を同時に外すため、**片側だけ**走査が狭まる退行（＝検証済みで
/// ない document を再確認が見逃す fail-open）をどの試験も赤くできない。
/// 同じ定数を両方へ注入すれば片側だけ狭まる形は構造的に作れず、共有走査
/// そのものの退行（shadow 降下の欠け・`frame` 見落とし等）は検証側の試験
/// （`fonts_inside_a_shadow_dom_iframe_are_awaited_before_the_capture`・
/// `fonts_inside_a_frameset_frame_are_awaited_before_the_capture` 等）が
/// 両側まとめて赤くする。
const COLLECT_DOCUMENTS_JS: &str = r#"const collectDocuments = (doc, out) => {
    // browsing context を持たない document（切り離された iframe 等）は描画に
    // 影響せず、その fonts.ready は二度と settle しない——FREEZE の freezeRoot
    // が持つ defaultView の門をここにも写す（cmd_663 ②。走査は検証側と
    // 再確認側で共有されるので、門も一箇所で両側に効く）。待ちの**最中**の
    // 切り離しはこの門では捉えられない——そちらは settle() の race が担う。
    if (!doc.defaultView) return;
    out.push(doc);
    const walk = (root) => {
      for (const el of root.querySelectorAll('*')) {
        if (el.shadowRoot) walk(el.shadowRoot);
        if (el.localName === 'iframe' || el.localName === 'frame') {
          let child = null;
          try { child = el.contentDocument; } catch (e) { child = null; }
          if (child) collectDocuments(child, out);
        }
      }
    };
    walk(doc);
  };"#;

static FONTS_WAIT_SCRIPT: LazyLock<String> = LazyLock::new(|| {
    [
        "(() => {\n  ",
        COLLECT_DOCUMENTS_JS,
        r#"
  // 各 document の FontFaceSet を形チェックつきで束ねる。壊れた形は throw で"#,
    ]
    .concat()
        + FONTS_WAIT_BODY
});

/// [`FONTS_WAIT_SCRIPT`] の走査（共有の [`COLLECT_DOCUMENTS_JS`]）以外の本文。
const FONTS_WAIT_BODY: &str = r#"
  // 失敗へ倒す（fail-closed）。待ちの巡ごとに再収集する——待ちの間に
  // 追加された iframe・入れ替わった document も次の巡で対象になる。
  const gather = () => {
    const docs = [];
    collectDocuments(document, docs);
    return docs.map((doc) => {
      let fonts = null;
      try {
        fonts = doc.fonts;
      } catch (e) {
        throw 'accessing document.fonts threw: ' + String(e);
      }
      if (!fonts || typeof fonts.status !== 'string' || !fonts.ready || typeof fonts.ready.then !== 'function') {
        throw 'document.fonts is missing or does not look like a FontFaceSet (status: ' + String(fonts && fonts.status) + ')';
      }
      return { doc: doc, fonts: fonts };
    });
  };
  // ready が解決しても status が 'loading' に戻っていたら、新しい波の
  // 完了（読み込み再開で差し替わった新しい ready promise）を待ち直す。
  // 待ち直しの回数に上限は設けない——上限値はどんな数でも根拠がなく
  // （250ms の SETTLE_DELAY と同じ「たまたま足りた/足りない」の再発明）、
  // 停止性は回数ではなく Rust 側の共有 deadline が保証する。収束しない
  // ページはこの promise が解決せず、期限で Timeout に倒れて撮られない。
  // 待ち直しの前に setTimeout(0) で 1 ティック譲る——`ready` が解決済みの
  // まま status が 'loaded' 以外を返し続ける非準拠実装（`ready` を作り
  // 直さない FontFaceSet 相当物）で、マイクロタスクだけの再帰がレンダラの
  // メインスレッドを deadline まで占有しないため。requestAnimationFrame は
  // rAF を捨てるページで永久に返らないので使わない。setTimeout の側にも
  // 前提がある——ページが fake timers（sinon 等）で setTimeout を止めて
  // いれば、この譲りは発火せず evaluate は返らないが、その場合も共有
  // deadline の Timeout へ倒れる（fail-closed。黙って撮る方向には壊れない）。
  // 待っている間に document が browsing context から切り離される（iframe の
  // DOM からの除去・location.replace 等）と、その fonts.ready は二度と
  // settle せず Promise.all が永久に pending になる——settle() の再帰は
  // Promise.all の解決後にしか走らないため、「巡ごとに再収集する」はこの
  // 局面では一度も回らず、story は共有 deadline の Timeout へ倒れていた
  // （cmd_663 ②）。そこで各 ready を「切り離しの検知」と race させ、外れた
  // document は待ちからも判定からも捨てる——描画されない document のために
  // story を落とさない（FREEZE の freezeRoot が持つ defaultView の門と同じ
  // 強度）。検知は setTimeout ポーリング——fake timers で setTimeout を
  // 止めるページでは検知が発火せず、共有 deadline の Timeout へ倒れる
  // （下の譲りティックと同じ前提・fail-closed）。ready 側が先に settle
  // したらフラグでポーリングを止め、タイマーを残さない。
  const DETACH_POLL_MS = 50;
  const readyOrDetached = (s) => {
    let settled = false;
    const ready = Promise.resolve(s.fonts.ready).then(
      (v) => { settled = true; return v; },
      (e) => { settled = true; throw e; }
    );
    const detached = new Promise((resolve) => {
      const check = () => {
        if (settled) return;
        if (!s.doc.defaultView) { resolve(); return; }
        setTimeout(check, DETACH_POLL_MS);
      };
      setTimeout(check, DETACH_POLL_MS);
    });
    return Promise.race([ready, detached]);
  };
  const settle = () => {
    let sets;
    try {
      sets = gather();
    } catch (e) {
      return JSON.stringify({ ok: false, errors: [String(e)] });
    }
    // ready の二度目の読み（一度目は gather の形チェック）も try で囲む——
    // 初回は形チェックを通り後の読みで throw する stateful getter では、裸の
    // 読みが settle 自体の throw になり、evaluate のリトライへ化けて原因と
    // 食い違う phase の Timeout になる（印の書き込みと同じ理由。cmd_663 ③）。
    let races;
    try {
      races = sets.map((s) => readyOrDetached(s));
    } catch (e) {
      return JSON.stringify({ ok: false, errors: ['reading document.fonts.ready threw: ' + String(e)] });
    }
    return Promise.all(races).then(
      () => {
        // status の二度目の読みも同じ理由で try に倒す（cmd_663 ③）。切り離しを
        // race が検知した場合は、ここで生きている document だけへ絞る——外れた
        // document の fonts は待ちからも判定からも外す（読めるとも限らない）。
        try {
          sets = sets.filter((s) => s.doc.defaultView);
          if (sets.some((s) => s.fonts.status !== 'loaded')) {
            return new Promise((resolve) => setTimeout(resolve, 0)).then(settle);
          }
        } catch (e) {
          return JSON.stringify({ ok: false, errors: ['reading document.fonts.status threw: ' + String(e)] });
        }
        // 読み込みに失敗したフォントは撮影を止めない（意図した fail-open。
        // 採否の理由はモジュール doc 失敗経路①）。family を Set で一意化した
        // 一覧だけを返し、警告への整形・上限は Rust 側 fonts_verdict が担う。
        // 列挙が throw する形（部分的な差し替え等）は観測不能への転落なので
        // errors へ載せて失敗に倒す——観測できなくなったことを黙って通さない。
        let failed;
        try {
          failed = new Set();
          for (const s of sets) {
            for (const face of s.fonts) {
              if (face.status === 'error') failed.add(String(face.family));
            }
          }
        } catch (e) {
          return JSON.stringify({ ok: false, errors: ['enumerating document.fonts threw: ' + String(e)] });
        }
        // 検証済みの印。検証したその document の documentElement へ直接書く
        // （完了時の document グローバルを読み直さない——待ちの間に
        // 差し替わった未検証の document へ印を付けないため）。document が
        // 入れ替われば documentElement ごと作り直されて印は消えるので、
        // 撮影直前の再確認（FONTS_RECHECK_PROBE）が入れ替わりを検知できる。
        //
        // dataset は HTMLOrSVGElement mixin——素の XML document（feed.xml を
        // 読んだ同一オリジン iframe 等）の documentElement は持たない。印を
        // 刻む口が無い document はスキップする（再確認側も同じ条件で印を
        // 要求しない——両側で揃える。cmd_661 追送）。スキップした document は
        // 差し替わっても印では検知できない（fonts.status の検査は残る）。
        // 書き込み自体の想定外の throw は、gather やフォント列挙と同じく
        // ok: false と errors へ倒す（fail-closed。印を書けたか不明のまま
        // 「検証済み」を名乗らない）——ここだけ裸だと、promise の reject が
        // evaluate のリトライへ化けて、原因と食い違う phase の Timeout になる。
        try {
          for (const s of sets) {
            const el = s.doc.documentElement;
            if (el && el.dataset) {
              el.dataset.vrtFontsVerified = 'true';
            }
          }
        } catch (e) {
          return JSON.stringify({ ok: false, errors: ['writing the verified mark threw: ' + String(e)] });
        }
        return JSON.stringify({ ok: true, status: 'loaded', failed: Array.from(failed), errors: [] });
      },
      (e) => JSON.stringify({ ok: false, errors: ['document.fonts.ready rejected: ' + String(e)] })
    );
  };
  return settle();
})()
"#;

/// 撮影直前の軽い再確認プローブ。フォント検証（[`FONTS_WAIT_SCRIPT`]）が
/// **これから撮る document で**成立しているかを 1 往復で読む。
///
/// フォント検証は FREEZE の前にある——FREEZE は最終レイアウトを見る必要が
/// あり、フォント適用で始まった CSS transition も静止の対象なので、後ろへは
/// 動かせない。だがその位置ゆえ、`storyRendered` の後に自分を reload する
/// story では「検証した document」と「撮影される document」が別物になりうる
/// （fonts 待ちが前 document で ok → FREEZE が navigated or closed の
/// リトライを経て後 document で成功——窓は evaluate 二往復ではなく
/// **deadline までのリトライ全長**で、数十秒になりうる。cmd_659 実測）。
///
/// そこで検証列の最後・撮影の直前にこのプローブを置く。
/// [`FONTS_WAIT_SCRIPT`] と同じ範囲——FREEZE の `freezeRoot` と同じく
/// open shadow root へ潜り `iframe` と `frame` の両方を見る走査——で到達
/// 可能な同一オリジン iframe を再帰し（検証の鏡——待った範囲と確かめる
/// 範囲を揃える。cmd_661 ②）、各 document で二点を読む:
///
/// - `documentElement.dataset.vrtFontsVerified`——[`FONTS_WAIT_SCRIPT`] が
///   成功時に残す印。document が入れ替われば（ナビゲーションでも
///   `document.open()` でも）documentElement ごと作り直されて消えるので、
///   入れ替わりの検知器になる。**同一 document 内の DOM 全面置換では
///   残る**——この形は捉えられない（モジュール doc 失敗経路⑤）。
///   `dataset` を持たない documentElement（素の XML document）には検証側が
///   印を刻めないため、ここでも要求しない——そうした document の
///   入れ替わりは印では捉えられない（fonts.status の検査は行う。
///   cmd_661 追送）
/// - `document.fonts.status`——同じ document のまま新しい読み込み波が
///   始まっていれば `'loading'` に戻っている
///
/// どこかの document で不成立なら Rust 側が検証列（READY 待ち→settle→
/// フォント待ち→reduced-motion 検証→静止）を**やり直す**。常に二度待つの
/// ではなく、窓が実際に開いた時だけ待ち直す形であり、停止性は共有 deadline
/// が担う。このプローブからスクリーンショットまでの最後の一往復に始まる
/// 変化は原理的に検知できない（スクリーンショットは JS を走らせない一往復の
/// CDP コマンド）。
static FONTS_RECHECK_PROBE: LazyLock<String> = LazyLock::new(|| {
    [
        "(() => {\n  ",
        COLLECT_DOCUMENTS_JS,
        r#"
  // 走査は検証側（FONTS_WAIT_SCRIPT）と共有の COLLECT_DOCUMENTS_JS——
  // 待った範囲と確かめる範囲は同じ一本の走査で揃う（cmd_661 ②・cmd_662 ④。
  // 別々に写すと片側だけ狭まる退行をどの試験も赤くできない）。走査の throw は
  // 下の try に届いて未検証へ倒れる（fail-closed）。
  let verified = true;
  let status = 'loaded';
  // 走査も各 document の検査も、想定外の throw は未検証へ倒す（fail-closed。
  // 旧・visit 再帰では両方が一つの try の中にあった——分割後も同じ強度を保つ）。
  try {
  const docs = [];
  collectDocuments(document, docs);
  for (const doc of docs) {
    // dataset を持たない documentElement（素の XML document 等）には印を
    // 刻む口が無い——検証側（FONTS_WAIT_SCRIPT）も同じ条件でスキップした
    // ので、ここでも印を要求しない（両側で揃える。cmd_661 追送）。その
    // document の fonts.status の検査は下で行う。documentElement 自体が
    // 無い document は検証のしようがないので未検証に倒す（fail-closed）。
    const el = doc.documentElement;
    if (!el) {
      verified = false;
    } else if (el.dataset && el.dataset.vrtFontsVerified !== 'true') {
      verified = false;
    }
    let s = null;
    try {
      if (doc.fonts && typeof doc.fonts.status === 'string') {
        s = doc.fonts.status;
      }
    } catch (e) {
      s = null;
    }
    // 「読めない（null）」も「loading」も loaded ではない——最初に見つかった
    // 不成立を報告へ残す（Rust 側はどちらもやり直しへ倒す。collectDocuments
    // は親を先に積む pre-order なので、旧・visit 再帰と同じ順で最初の不成立が
    // 残る）。
    if (s !== 'loaded' && status === 'loaded') {
      status = s;
    }
  }
  } catch (e) {
    return JSON.stringify({ verified: false, status: null });
  }
  return JSON.stringify({ verified: verified, status: status });
})()
"#,
    ]
    .concat()
});

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("failed to launch chromium at {path}: {source}")]
    Launch {
        path: String,
        #[source]
        source: chromiumoxide::error::CdpError,
    },
    #[error("failed to start the bundle static server: {0}")]
    Server(String),
    /// story ごとの時間予算（`story_timeout`）を使い切った。READY 待ちと
    /// FREEZE evaluate は**同じ deadline を分け合う**ので、どちらの段で
    /// 時間切れになってもこの分類で報告する（`phase` が段を示す）。
    #[error("story `{story_id}` did not complete within {timeout:?}: {phase}")]
    Timeout {
        story_id: String,
        timeout: Duration,
        /// どの段で時間切れになったか（人間向けの説明文）。
        phase: &'static str,
    },
    /// Storybook 自身が「このストーリーは失敗した」と言ってきた場合。
    /// タイムアウトより遥かに早く、理由つきで返せる。
    #[error("story `{story_id}` reported a render error: {message}")]
    Story { story_id: String, message: String },
    #[error("story `{story_id}` failed to render: {source}")]
    Cdp {
        story_id: String,
        #[source]
        source: chromiumoxide::error::CdpError,
    },
}

/// [`READY_PROBE`] の返り値。
#[derive(Debug, PartialEq, Eq)]
enum Readiness {
    /// Storybook が描画完了を通知した（または DOM フォールバックが成立した）。
    Ready,
    /// Storybook がこのストーリーの失敗を通知した。
    Error(String),
    /// ランタイムはいる。まだ描画中。
    Pending,
    /// Storybook ランタイムが見当たらない。`dom_ready` は旧ヒューリスティックの結果。
    Absent { dom_ready: bool },
}

impl Readiness {
    /// プローブが返した JSON 文字列を解釈する。
    ///
    /// 壊れた値・想定外の値は「まだ待つ」に倒す。誤って完了扱いにして
    /// 白紙を撮るより、タイムアウトさせたほうが原因が分かりやすい。
    fn parse(value: Option<&serde_json::Value>) -> Self {
        let Some(raw) = value.and_then(|v| v.as_str()) else {
            return Readiness::Pending;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
            return Readiness::Pending;
        };
        match parsed.get("state").and_then(|v| v.as_str()) {
            Some("ready") => Readiness::Ready,
            Some("error") => Readiness::Error(
                parsed
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            ),
            Some("absent") => Readiness::Absent {
                dom_ready: parsed
                    .get("dom_ready")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            },
            _ => Readiness::Pending,
        }
    }
}

/// 1 story の撮影の成果物。
///
/// PNG に加えて、**判定は通したが利用者に届けるべき警告**を載せる。
/// `tracing::warn` はサーバーの運用ログにしか出ず、ビルドを眺める利用者には
/// 届かない——警告を成功値に載せることで、呼び出し側（`render_build` の
/// `render_all`）が build log（`build_logs::append` / `LogLevel::Warn`）へ
/// 永続化できる（cmd_660 C）。
#[derive(Debug)]
pub struct RenderedStory {
    /// 撮影された PNG バイト列。
    pub png: Vec<u8>,
    /// フォント読み込み失敗の警告（[`fonts_verdict`] が整形したもの。
    /// 失敗経路①の意図した fail-open——代替字形のまま撮った印）。
    /// 検証列をやり直した場合は**撮影された巡**の警告である。
    pub font_warning: Option<String>,
}

/// [`RenderError::Cdp`] だけを 1 回やり直すリトライ骨格。
///
/// 対象を `Cdp` に限る理由: `Timeout` / `Story` は story 固有（やり直しても
/// 同じ理由で落ちる）、`Launch` / `Server` はこの層（story 単位の撮影）には
/// 来ない。2 回目も `Cdp` なら従来どおり環境失敗として返す——リトライは
/// 1 回きりで、ブラウザ本体が本当に死んでいる場合の即中断（続行は同じ
/// エラーの羅列に 30 秒×N を費やすだけ）を骨抜きにしない。
///
/// テストはこの骨格だけを固定する（意図した割り切り）——「`render_story` が
/// この骨格を通り、2 回目が新しいタブで走る」配線は、実ブラウザでタブ単位の
/// CDP 故障を決定的に再現する手段が無く、貫通テストにできない。
async fn retry_once_on_cdp<F, Fut>(
    story_id: &str,
    mut attempt: F,
) -> Result<RenderedStory, RenderError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<RenderedStory, RenderError>>,
{
    match attempt().await {
        Err(err @ RenderError::Cdp { .. }) => {
            tracing::warn!(
                %story_id,
                error = %err,
                "story hit a CDP failure; retrying once on a fresh tab"
            );
            attempt().await
        }
        result => result,
    }
}

/// レンダリングのパラメータ。ビューポートはプロジェクト設定から渡す。
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub chromium_path: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub story_timeout: Duration,
    /// 撮影直前に [`FREEZE_SCRIPT`] を注入するか。既定は `true`。
    ///
    /// `false` はテスト専用の裏口で、「静止させないと本当に絵が揺れる」ことを
    /// 検証する positive control のためにある。本番経路（`render_build`）は
    /// [`RenderOptions::new`] を通るので常に `true`。
    pub freeze_before_capture: bool,
    /// `prefers-reduced-motion: reduce` をエミュレートして撮るか。
    /// 既定は `false`（project 設定の既定 OFF と一致）。
    ///
    /// `true` のときはナビゲーション前に `Emulation.setEmulatedMedia` を
    /// 一度設定し、撮影直前に [`REDUCED_MOTION_PROBE`] で「実際に効いている」
    /// ことを実測する。効いていると確かめられなければ撮らない（fail-closed）。
    pub emulate_reduced_motion: bool,
    /// 撮影前に `document.fonts.ready` の条件待ちを行うか。
    ///
    /// `false` はテスト専用の裏口で、「待たないと本当に絵が揺れる」ことを
    /// 検証する positive control のためにある（`freeze_before_capture` と
    /// 同じ規約）。本番経路（`render_build`）は [`RenderOptions::new`] を
    /// 通るので常に `true`。
    pub wait_for_fonts: bool,
}

impl RenderOptions {
    pub fn new(
        chromium_path: impl Into<String>,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Self {
        Self {
            chromium_path: chromium_path.into(),
            viewport_width,
            viewport_height,
            story_timeout: DEFAULT_STORY_TIMEOUT,
            freeze_before_capture: true,
            emulate_reduced_motion: false,
            wait_for_fonts: true,
        }
    }
}

// ── 静的配信 ────────────────────────────────────────────────────────────

/// 展開済みバンドルをループバックで配信する使い捨て HTTP サーバー。
///
/// drop でタスクを abort するため、ジョブがどこで落ちてもポートは解放される。
pub struct StaticServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl StaticServer {
    /// `root` を `127.0.0.1` のランダムポートで配信する。
    pub async fn start(root: impl AsRef<Path>) -> Result<Self, RenderError> {
        let root: PathBuf = root.as_ref().to_path_buf();
        let app = axum::Router::new().fallback_service(tower_http::services::ServeDir::new(root));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| RenderError::Server(format!("bind 127.0.0.1:0: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| RenderError::Server(format!("local_addr: {e}")))?;

        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::warn!(error = %e, "storybook bundle server stopped");
            }
        });

        Ok(Self { addr, task })
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ── ブラウザ ────────────────────────────────────────────────────────────

/// 起動済み Chromium。ジョブ 1 本につき 1 インスタンス。
pub struct StoryRenderer {
    browser: Browser,
    handler_task: JoinHandle<()>,
    options: RenderOptions,
    /// このインスタンス専用のユーザーデータディレクトリ。
    /// フィールドとして持つのは drop 時に自動で消すため（`_` で握るだけ）。
    _user_data_dir: tempfile::TempDir,
}

impl StoryRenderer {
    /// Chromium を起動する。
    ///
    /// CI では複数ジョブが同時にコールドスタートし、負荷で起動タイムアウトや
    /// プロセスの即死を踏むことがある。1 発失敗で諦めず、短いバックオフ（2s, 4s）を
    /// 挟んで最大 [`LAUNCH_MAX_ATTEMPTS`] 回まで試す。試行ごとにプロファイルを
    /// 作り直すので、失敗した試行が残したロックを引き継がない。
    /// リトライ対象は起動（launch）だけで、起動成功後の描画エラーには適用しない。
    pub async fn launch(options: RenderOptions) -> Result<Self, RenderError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match Self::launch_once(&options).await {
                Ok(renderer) => return Ok(renderer),
                Err(err) => {
                    // 実行ファイルが無い等の恒久的な失敗はリトライしても無駄なので即返す。
                    let transient = matches!(
                        &err,
                        RenderError::Launch { source, .. } if is_transient_launch_error(source)
                    );
                    if !transient || attempt >= LAUNCH_MAX_ATTEMPTS {
                        return Err(err);
                    }
                    // 2s, 4s, ... と待ってから、新しいプロファイルでやり直す。
                    let backoff = Duration::from_secs(1u64 << attempt);
                    tracing::warn!(
                        attempt,
                        max_attempts = LAUNCH_MAX_ATTEMPTS,
                        backoff_secs = backoff.as_secs(),
                        error = %err,
                        "chromium launch failed; retrying with a fresh profile"
                    );
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    /// 1 回ぶんの起動試行。失敗はそのまま返し、リトライ判断は [`Self::launch`] に任せる。
    async fn launch_once(options: &RenderOptions) -> Result<Self, RenderError> {
        // 起動ごとに専用のユーザーデータディレクトリを与える。
        // 指定しないと chromiumoxide は固定パス（`/tmp/chromiumoxide-runner`）を使い、
        // 複数のブラウザが同時に立つと `SingletonLock` を奪い合って起動に失敗する
        // （クラッシュで残ったロックが次の起動を止めることもある）。
        let user_data_dir = tempfile::Builder::new()
            .prefix("vrt-chromium-")
            .tempdir()
            .map_err(|e| RenderError::Server(format!("create chromium profile dir: {e}")))?;

        let config = BrowserConfig::builder()
            .chrome_executable(&options.chromium_path)
            .user_data_dir(user_data_dir.path())
            // CI の並列コールドスタートでは chromiumoxide 既定の 20 秒に間に合わず
            // 起動タイムアウトを踏むため、明示的に広げる。
            .launch_timeout(LAUNCH_TIMEOUT)
            .viewport(Viewport {
                width: options.viewport_width,
                height: options.viewport_height,
                ..Default::default()
            })
            .window_size(options.viewport_width, options.viewport_height)
            // コンテナ内（root・非特権）で動かすため sandbox を落とす。
            // 描画対象は自前で展開した信頼済みバンドルのみ、かつループバック配信。
            .no_sandbox()
            .args(vec![
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--hide-scrollbars",
                "--force-device-scale-factor=1",
            ])
            .build()
            .map_err(RenderError::Server)?;

        let (browser, mut handler) =
            Browser::launch(config)
                .await
                .map_err(|source| RenderError::Launch {
                    path: options.chromium_path.clone(),
                    source,
                })?;

        // chromiumoxide は handler を回し続けないとコマンドが進まない。
        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        Ok(Self {
            browser,
            handler_task,
            options: options.clone(),
            _user_data_dir: user_data_dir,
        })
    }

    /// 1 ストーリーを撮って PNG バイト列を返す。
    ///
    /// CDP 分類の失敗（[`RenderError::Cdp`]）だけは、新しいタブで 1 回だけ
    /// やり直す（[`retry_once_on_cdp`]）。本番で「作ったタブへのコマンドが
    /// すべて 30 秒タイムアウトする」間欠故障が観測された（2026-08-24。
    /// Chromium 151 × chromiumoxide 0.9.1 で CDP メッセージのパース失敗
    /// `WS Invalid message` が常在し、タブのセッション確立に関わる
    /// メッセージを取りこぼした回だけタブが文鎮化する、が最有力の機序）。
    /// ブラウザ自体は健在でタブ単位の故障なので、環境失敗として即中断へ
    /// 倒す前に一度だけ新しいタブを試す。
    pub async fn render_story(
        &self,
        base_url: &str,
        story_id: &str,
    ) -> Result<RenderedStory, RenderError> {
        retry_once_on_cdp(story_id, || self.render_story_attempt(base_url, story_id)).await
    }

    /// [`Self::render_story`] の 1 回ぶんの試行。リトライ判断は呼び出し側
    /// （[`retry_once_on_cdp`]）に任せる。
    async fn render_story_attempt(
        &self,
        base_url: &str,
        story_id: &str,
    ) -> Result<RenderedStory, RenderError> {
        let url = story_url(base_url, story_id);

        // 空ページで開いてからフックを仕込み、そのあとで遷移する。
        // `new_page(url)` で直接開くと、フックが載る前に Storybook が
        // `storyRendered` を撃ち終えてしまう可能性がある。
        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|source| RenderError::Cdp {
                story_id: story_id.to_string(),
                source,
            })?;

        let result = async {
            // reduced-motion のエミュレーションは**ナビゲーション前**に一度
            // 設定する。撮影直前に設定すると、初期化時に一度だけ
            // `matchMedia` を読む実装（この層の主対象である rAF / canvas
            // 実装の最頻形）には見えない——OS で reduce を設定した実利用者は
            // ページ読み込みの最初から reduce で描画されるのであり、それと
            // 同じ条件で撮る。`Emulation.setEmulatedMedia` はセッション状態
            // なのでナビゲーションを跨いで効き続け、「実際に効いているか」は
            // 撮影直前に [`REDUCED_MOTION_PROBE`] で実測する（fail-closed）。
            // story のスクリプトを待たない一往復の CDP コマンドなので、
            // 失敗は `new_page` と同じ環境分類（無応答は chromiumoxide の
            // request timeout（既定 30 秒）が拾う——モジュール doc の表を参照）。
            if self.options.emulate_reduced_motion {
                page.execute(
                    SetEmulatedMediaParams::builder()
                        .feature(MediaFeature::new("prefers-reduced-motion", "reduce"))
                        .build(),
                )
                .await
                .map_err(|source| RenderError::Cdp {
                    story_id: story_id.to_string(),
                    source,
                })?;
            }
            page.evaluate_on_new_document(READY_HOOK_SCRIPT)
                .await
                .map_err(|source| RenderError::Cdp {
                    story_id: story_id.to_string(),
                    source,
                })?;
            page.goto(url.as_str())
                .await
                .map_err(|source| RenderError::Cdp {
                    story_id: story_id.to_string(),
                    source,
                })?;
            self.render_on_page(&page, story_id).await
        }
        .await;

        // 撮影の成否によらずタブは閉じる（開きっぱなしだとメモリを食う）。
        // ただし CDP 失敗（＝タブが文鎮化してコマンドが 30 秒返らない実測
        // 故障）の巡では close も同じセッション経由で 30 秒待たされるため、
        // リトライを遅らせないようバックグラウンドで閉じる。close に失敗した
        // タブはブラウザ終了（[`Self::close`]）まで残る——その費用より
        // リトライの即応を取る。
        if matches!(result, Err(RenderError::Cdp { .. })) {
            let story_id = story_id.to_string();
            tokio::spawn(async move {
                if let Err(e) = page.close().await {
                    tracing::debug!(%story_id, error = %e, "closing story page failed");
                }
            });
        } else if let Err(e) = page.close().await {
            tracing::debug!(%story_id, error = %e, "closing story page failed");
        }

        result
    }

    async fn render_on_page(
        &self,
        page: &chromiumoxide::page::Page,
        story_id: &str,
    ) -> Result<RenderedStory, RenderError> {
        let started = std::time::Instant::now();
        let deadline = started + self.options.story_timeout;

        // フォント待ち→reduced-motion 検証→静止の検証列は、**これから撮る
        // document** で成立していなければ意味がない。story が storyRendered の
        // 後に自分を reload すると、フォント待ちが前 document で ok を返し、
        // FREEZE が navigated or closed のリトライを経て後 document で成功し、
        // フォント未検証の document が撮れてしまう（cmd_659 実測。窓は
        // deadline までのリトライ全長＝数十秒になりうる）。そこで検証列の
        // 最後に軽い再確認（[`FONTS_RECHECK_PROBE`]）を置き、入れ替わり・
        // 新しい読み込み波を検知したら検証列をやり直す。
        //
        // やり直しは **READY 待ちと SETTLE_DELAY から**回す（cmd_660。
        // yupix レビューの機序）: Blink の `FontFaceSet.ready` は「load
        // イベント完了＋読み込み中フォント無し」で解決するため、reload 後の
        // document では story がまだ描画されていない時点で解決しうる。
        // やり直しを fonts 待ちからしか回さないと、fonts ok → freeze ok →
        // 再確認 ok が全て通り、`storyRendered` **前**の未描画の絵をそのまま
        // 撮ってしまう（cmd_660 実測: reload 後の再描画が遅い fixture で
        // 未描画の白い絵が撮れた）。READY 待ちから回すことで、二巡目の
        // document の `storyErrored` / `storyThrewException` もここで
        // 観測される（cmd_660 実測）。
        //
        // フォント待ちを FREEZE の後ろへ動かす形は採らない——FREEZE は最終
        // レイアウトを見る必要があり、フォント適用で始まった CSS transition
        // も静止の対象で、順序を崩すとその性質が壊れる。常に二度待つ形も
        // 採らない——再待ちが走るのは窓が実際に開いたと検知された時だけで
        // ある。停止性は READY 待ちと共有の deadline が担う。
        let font_warning = loop {
            self.wait_for_story_ready(page, story_id, deadline).await?;

            tokio::time::sleep(SETTLE_DELAY).await;

            // webfont は「時間」ではなく「状態」で待つ。storyRendered はフォントを
            // 待たずに出るため、ここで document.fonts.ready の解決（＝読み込み中の
            // フォントが無くなった状態）を実測してから先へ進む。SETTLE_DELAY の
            // **後**に置くのは、settle 窓の間に始まったフォント要求も待ちに
            // 含めるため。evaluate は READY 待ちと共有の deadline の残余に載せ、
            // ready が解決しないページは期限で Timeout に倒す（fail-closed——
            // 揺れる可能性のある絵を撮るくらいなら撮らない）。読み込みに
            // **失敗**したフォントは止めない——警告を残して代替字形のまま撮る
            // （意図した fail-open。理由はモジュール doc 失敗経路①）。警告は
            // 巡ごとに取り直す——撮影されるのは最後の巡が検証した document
            // なので、前の巡の警告を持ち越すと撮った絵と食い違う。
            let mut round_warning = None;
            if self.options.wait_for_fonts {
                let wait_result = evaluate_with_deadline_retry(
                    || page.evaluate(FONTS_WAIT_SCRIPT.as_str()),
                    deadline,
                    story_id,
                    self.options.story_timeout,
                    FONTS_PHASES,
                )
                .await?;
                round_warning = fonts_verdict(wait_result.value(), story_id)?;
                if let Some(warning) = &round_warning {
                    tracing::warn!(%story_id, %warning, "fonts failed to load; capturing with fallback glyphs");
                }
            }

            self.verify_and_freeze(page, story_id, deadline).await?;

            // 検証列がこの document で成立したまま撮影へ入れるかの再確認。
            // wait_for_fonts が無効なら印を残す層がそもそも無いので確かめない
            // （テスト専用の裏口。本番経路は常に有効）。
            if !self.options.wait_for_fonts {
                break round_warning;
            }
            let recheck_result = evaluate_with_deadline_retry(
                || page.evaluate(FONTS_RECHECK_PROBE.as_str()),
                deadline,
                story_id,
                self.options.story_timeout,
                FONTS_RECHECK_PHASES,
            )
            .await?;
            if fonts_recheck_verdict(recheck_result.value(), story_id)? {
                break round_warning;
            }
            // 未検証を検知した。やり直す前に deadline を確かめる——evaluate が
            // すべて即応するのに再確認だけ不成立が続くページ（入れ替わり・
            // 新波が止まらない）でビジーループにならないため。
            if std::time::Instant::now() + POLL_INTERVAL >= deadline {
                return Err(RenderError::Timeout {
                    story_id: story_id.to_string(),
                    timeout: self.options.story_timeout,
                    phase: FONTS_RECHECK_EXHAUSTED_PHASE,
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        };

        let png = page
            .screenshot(
                ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .full_page(false)
                    .build(),
            )
            .await
            .map_err(|source| RenderError::Cdp {
                story_id: story_id.to_string(),
                source,
            })?;
        Ok(RenderedStory { png, font_warning })
    }

    /// READY 待ち（Storybook の描画完了シグナルのポーリング）。
    /// [`Self::render_on_page`] の検証列ループの先頭から呼ばれる——初回と、
    /// 再確認が document の入れ替わりを検知したやり直しの両方。やり直しの
    /// 巡では、入れ替わった document に再注入された READY hook の状態を
    /// 読むため、二巡目の `storyErrored` / `storyThrewException` もここで
    /// 観測される。document の入れ替わりを**伴わない**再描画（同一 document
    /// で state.rendered が true のまま）は即座に通る——待ち直しのやり直しで
    /// 二重に待つことはない。
    async fn wait_for_story_ready(
        &self,
        page: &chromiumoxide::page::Page,
        story_id: &str,
        deadline: std::time::Instant,
    ) -> Result<(), RenderError> {
        // Absent（ランタイム無し）の DOM ヒューリスティックに与える
        // [`SIGNAL_GRACE`] は**この巡の開始**から測る。story 全体の開始時刻から
        // 測ると、やり直しの巡では猶予が最初から尽きており、root に子が一つ
        // 入った瞬間の**途中の絵**を ready と誤判定する（cmd_661 ③）——
        // やり直しは「入れ替わった document を最初から待ち直す」機構なのに、
        // その巡だけ判定が緩くなる。deadline は従来どおり story 全体で共有
        // （引数のまま受け取る——猶予と期限は別の時計である）。
        let round_started = std::time::Instant::now();
        loop {
            match page.evaluate(READY_PROBE).await {
                Ok(result) => match Readiness::parse(result.value()) {
                    Readiness::Ready => return Ok(()),
                    Readiness::Error(message) => {
                        return Err(RenderError::Story {
                            story_id: story_id.to_string(),
                            message,
                        });
                    }
                    // Storybook ランタイムが無いバンドルだけ、猶予を置いて DOM で判定する。
                    Readiness::Absent { dom_ready } => {
                        if dom_ready && round_started.elapsed() >= SIGNAL_GRACE {
                            tracing::debug!(
                                %story_id,
                                "no storybook render signal; falling back to the DOM heuristic"
                            );
                            return Ok(());
                        }
                    }
                    Readiness::Pending => {}
                },
                // ナビゲーション中は実行コンテキストが差し替わって一時的に失敗する。
                // タイムアウトまではリトライし続ける。
                Err(e) => {
                    tracing::trace!(%story_id, error = %e, "story readiness probe failed; retrying");
                }
            }

            if std::time::Instant::now() >= deadline {
                return Err(RenderError::Timeout {
                    story_id: story_id.to_string(),
                    timeout: self.options.story_timeout,
                    phase: "no render-completion signal arrived",
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// 検証列のうち reduced-motion 検証と静止（フォント待ちの後・撮影の前）。
    /// [`Self::render_on_page`] の再確認ループから呼ばれる。
    async fn verify_and_freeze(
        &self,
        page: &chromiumoxide::page::Page,
        story_id: &str,
        deadline: std::time::Instant,
    ) -> Result<(), RenderError> {
        // reduced-motion を要求した project では、撮影直前に「実際に効いて
        // いる」ことを実測する。setEmulatedMedia の応答が成功でも効いた
        // 証明にはならず、「reduce を要求したのに適用されなかった」が
        // 黙って通れば、静止させたと信じたまま動く絵を撮る（fail-closed）。
        // evaluate の CDP エラーは READY probe / FREEZE evaluate と同じ扱いで
        // deadline までリトライし、期限で story 分類の Timeout に倒す。
        if self.options.emulate_reduced_motion {
            let probe_result = evaluate_with_deadline_retry(
                || page.evaluate(REDUCED_MOTION_PROBE),
                deadline,
                story_id,
                self.options.story_timeout,
                REDUCED_MOTION_PHASES,
            )
            .await?;
            reduced_motion_verdict(probe_result.value(), story_id)?;
        }

        // 撮影直前にキャレットとアニメーションを決定的な座標へ固定する。
        // 静止に失敗したまま撮ると flaky な絵が baseline に混ざるので、
        // 黙って続行せず失敗として返す（fail-closed）。
        if self.options.freeze_before_capture {
            // CSP は迂回しない（本番と同じ条件で撮る）。静止 CSS の注入は
            // constructed stylesheet（CSSOM）経由で CSP style-src の管轄外
            // なので、CSP 起因の適用失敗はそもそも起きない。FREEZE_SCRIPT は
            // --vrt-frozen プローブで適用を実測し、効いていなければ
            // fail-closed で失敗を返す。
            //
            // 停止性: FREEZE_SCRIPT は rAF を 2 回待つ promise を返すため、
            // ページが requestAnimationFrame を発火させない（差し替え・
            // 停止したレンダリングパイプライン等）と evaluate は永遠に
            // 返らない。READY 待ちと**同じ deadline**（started +
            // story_timeout）の残余に載せ、時間内に静止が終わらなければ
            // 失敗として返す（fail-closed）。独立予算にすると 1 story の
            // 最悪所要が約 2×story_timeout になり、「story ごとの描画
            // タイムアウト」という README の契約を裏切るためである。
            //
            // evaluate の CDP エラーは READY probe と**同じ扱い**でリトライする。
            // 実測で確認済みの経路が二つある（cmd_632）: story 側スクリプトの
            // navigation / reload が pending evaluate の実行コンテキストを壊す
            // 「Inspected target navigated or closed」（-32000。context 破棄系）
            // と、rAF コールバックを捨てるページで pending promise が GC に
            // 回収される「Promise was collected」（-32000）。どちらも撮影対象ページの内容に起因する story 固有の
            // 失敗であり、ここで即 [`RenderError::Cdp`]（環境分類→ビルド即中断）
            // へ倒すと、その story 1 件がビルド全体を巻き添えにする——READY 側は
            // リトライして期限で Timeout（story 分類）に倒れるのに、freeze 側
            // だけ即中断という非対称だった。期限まで直らなければ READY 側と
            // 同じ [`RenderError::Timeout`] で返す。本物の環境異常（ブラウザ死）
            // でも失うのは最大 1 story ぶんの予算で、次の story の `new_page` が
            // 環境分類の Cdp で中断する（分類表はモジュール先頭を参照）。
            let freeze_result = evaluate_with_deadline_retry(
                || page.evaluate(FREEZE_SCRIPT),
                deadline,
                story_id,
                self.options.story_timeout,
                FREEZE_PHASES,
            )
            .await?;

            // FREEZE_SCRIPT は JSON 文字列を返す。`ok === true` と確かめられた
            // 場合にだけ撮影へ進む（fail-closed）。ok: false（静止に失敗）も、
            // 解析できない応答（静止できたか不明）も撮らずに失敗として返す。
            freeze_verdict(freeze_result.value(), story_id)?;
        }

        Ok(())
    }

    /// ブラウザを明示的に閉じる。正常系では必ず呼ぶこと。
    pub async fn close(mut self) {
        if let Err(e) = self.browser.close().await {
            tracing::warn!(error = %e, "closing chromium failed");
        }
        // 子プロセスを刈り取ってから drop すると Browser の Drop が警告を出さない。
        let _ = self.browser.wait().await;
        self.handler_task.abort();
    }
}

impl Drop for StoryRenderer {
    fn drop(&mut self) {
        // `close()` を通らずに落ちた経路（panic / early return）でも
        // handler タスクを残さない。子プロセスは Browser の Drop が始末する。
        self.handler_task.abort();
    }
}

/// [`evaluate_with_deadline_retry`] が失敗を名づけるための文字列一式。
///
/// **一本化するのは機構であって分類ではない**。待ち・リトライ・期限の手続きは
/// 経路をまたいで一つで足りるが、「どの段で何が起きたか」は経路ごとに別の
/// ままでなければならない——`phase` は README の failure paths 表から辿れる
/// 診断であり、二経路が同じ文字列を返した時点でログから段を復元できなくなる。
/// 経路ごとの定数（[`REDUCED_MOTION_PHASES`] / [`FREEZE_PHASES`]）に閉じ込め、
/// 混入を `phase_strings_stay_distinct_per_path` が固定している。
#[derive(Debug, Clone, Copy)]
struct EvaluatePhases {
    /// CDP エラーをリトライするときの trace ログ。
    retry_log: &'static str,
    /// deadline までに evaluate が返らなかったときの `phase`。
    timeout: &'static str,
    /// CDP エラーのリトライが deadline で尽きたときの `phase`。
    retry_exhausted: &'static str,
}

/// reduced-motion 検証（[`REDUCED_MOTION_PROBE`]）の分類。
const REDUCED_MOTION_PHASES: EvaluatePhases = EvaluatePhases {
    retry_log: "reduced-motion probe evaluate failed; retrying",
    timeout: "the reduced-motion verification never returned a verdict",
    retry_exhausted: "the reduced-motion verification evaluate kept \
                      failing until the story deadline",
};

/// フォント条件待ち（[`FONTS_WAIT_SCRIPT`]）の分類。
const FONTS_PHASES: EvaluatePhases = EvaluatePhases {
    retry_log: "fonts-ready wait evaluate failed; retrying",
    timeout: "the fonts-ready wait never finished: document.fonts.ready did \
              not resolve within the story deadline",
    retry_exhausted: "the fonts-ready wait evaluate kept failing until the \
                      story deadline",
};

/// 撮影直前の再確認（[`FONTS_RECHECK_PROBE`]）の分類。
const FONTS_RECHECK_PHASES: EvaluatePhases = EvaluatePhases {
    retry_log: "fonts recheck probe evaluate failed; retrying",
    timeout: "the fonts recheck before the capture never returned a verdict",
    retry_exhausted: "the fonts recheck evaluate kept failing until the story \
                      deadline",
};

/// 再確認が「未検証」を検知し続けたまま deadline を迎えたときの `phase`。
/// evaluate は返っている（[`FONTS_RECHECK_PHASES`] の二つとは別の段）——
/// document が入れ替わり続ける・新しいフォント波が始まり続けるページである。
const FONTS_RECHECK_EXHAUSTED_PHASE: &str = "the fonts verification could not be re-established before the capture: \
     the document kept being replaced or kept starting new font loads until \
     the story deadline";

/// 静止（[`FREEZE_SCRIPT`]）の分類。
const FREEZE_PHASES: EvaluatePhases = EvaluatePhases {
    retry_log: "freeze evaluate failed; retrying",
    timeout: "the freeze did not finish: the page never yielded a verdict \
              (requestAnimationFrame may not be firing)",
    retry_exhausted: "the freeze evaluate kept failing until the story \
                      deadline (the page may be navigating or reloading, or its \
                      pending callbacks were collected)",
};

/// READY 待ちの後に走る evaluate の**機構**——共有 deadline への載せ方・
/// CDP エラーのリトライ・期限での倒し方——を一つにまとめたもの。
///
/// reduced-motion 検証と静止は、抽出前は同型のループを別々に持っていた
/// （約 30 行ずつ）。同型のまま二箇所にあるということは、片方だけ直せば
/// 非対称が生まれるということでもある——freeze evaluate の CDP エラーを
/// 即 [`RenderError::Cdp`]（環境分類＝ビルド即中断）へ倒していた非対称を
/// cmd_632 で塞いだのが、まさにその型の事故だった。
///
/// 手続きは次の三段:
///
/// 1. `deadline` までの**残余**に evaluate を載せる（独立予算にしない。
///    1 story の最悪所要が段の数だけ膨らみ、「story ごとの描画タイムアウト」
///    という README の契約を裏切るため）
/// 2. 残余を使い切って返らなければ [`EvaluatePhases::timeout`] の
///    [`RenderError::Timeout`]
/// 3. CDP エラーは撮影対象ページの内容に起因しうる（navigation / reload に
///    よる実行コンテキスト破棄、pending promise の GC 回収——どちらも
///    cmd_632 の実測）ので [`POLL_INTERVAL`] ごとにリトライし、次の一回が
///    deadline を跨ぐなら [`EvaluatePhases::retry_exhausted`] の
///    [`RenderError::Timeout`]（story 分類）で倒す
///
/// `evaluate` はクロージャで受ける。ページを直接持たないので、両分岐
/// （期限切れ・リトライ切れ）をブラウザ無しで決定的に試験できる。
async fn evaluate_with_deadline_retry<T, F, Fut>(
    mut evaluate: F,
    deadline: std::time::Instant,
    story_id: &str,
    story_timeout: Duration,
    phases: EvaluatePhases,
) -> Result<T, RenderError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, chromiumoxide::error::CdpError>>,
{
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(remaining, evaluate()).await {
            Ok(Ok(result)) => return Ok(result),
            Err(_) => {
                return Err(RenderError::Timeout {
                    story_id: story_id.to_string(),
                    timeout: story_timeout,
                    phase: phases.timeout,
                });
            }
            Ok(Err(e)) => {
                tracing::trace!(%story_id, error = %e, "{}", phases.retry_log);
                if std::time::Instant::now() + POLL_INTERVAL >= deadline {
                    return Err(RenderError::Timeout {
                        story_id: story_id.to_string(),
                        timeout: story_timeout,
                        phase: phases.retry_exhausted,
                    });
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

/// [`FREEZE_SCRIPT`] の返り値を検分し、撮影へ進んでよいか判定する。
///
/// `ok` が `true` であると確かめられた場合にだけ `Ok(())` を返す。
/// それ以外はすべて失敗（fail-closed）だが、原因の異なる二種類を
/// メッセージで区別する:
///
/// - `ok: false` — **静止に失敗した**。running な animation が残っている
///   （`freeze failed: ... still running ...`）。
/// - 値が文字列でない／JSON として読めない／`ok` キーが無い・bool でない —
///   **静止結果を解析できなかった**。静止できたかどうか自体が不明
///   （`freeze result was unparseable: ...`）。
///
/// parse 失敗や `ok` 欠落を暗黙に成功へ倒すと、静止に失敗した絵が
/// baseline に混ざる事故が沈黙して通る。判定できなかったら撮らない。
/// どちらも既存の storyErrored と同じ [`RenderError::Story`] 経路を使う。
fn freeze_verdict(value: Option<&serde_json::Value>, story_id: &str) -> Result<(), RenderError> {
    let unparseable = |detail: String| RenderError::Story {
        story_id: story_id.to_string(),
        message: format!("freeze result was unparseable: {detail}"),
    };
    let Some(raw) = value.and_then(|v| v.as_str()) else {
        return Err(unparseable(format!(
            "expected a JSON string, got {value:?}"
        )));
    };
    let parsed = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|e| unparseable(format!("{e} (raw: {raw})")))?;
    match parsed.get("ok").and_then(|v| v.as_bool()) {
        // 静止を確かめられた。撮影へ進む。`ok` 以外のキーは検査しない
        // （将来 FREEZE_SCRIPT が返すものを増やしても正当な応答を弾かない）。
        Some(true) => Ok(()),
        // 静止に失敗した。reason があれば CSS 適用の検証失敗か freeze 層内部の
        // エラー、running があればアニメーションが残っている。errors は
        // FREEZE_SCRIPT が各層で集めた診断で、あれば原因ごとメッセージに載せる
        // ——集めた診断は判定にも表示にも使う。
        Some(false) => {
            let errors_note = parsed
                .get("errors")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    let joined = arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!(" (errors: {joined})")
                })
                .unwrap_or_default();
            if let Some(reason) = parsed.get("reason").and_then(|v| v.as_str()) {
                return Err(RenderError::Story {
                    story_id: story_id.to_string(),
                    message: format!("freeze failed: {reason}{errors_note}"),
                });
            }
            let running = parsed.get("running").and_then(|v| v.as_array());
            let names = running
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            Err(RenderError::Story {
                story_id: story_id.to_string(),
                message: format!(
                    "freeze failed: {count} animation(s) still running \
                     after {sweeps} sweep(s): [{names}]{errors_note}",
                    count = running.map(|a| a.len()).unwrap_or(0),
                    sweeps = parsed.get("sweeps").and_then(|v| v.as_u64()).unwrap_or(0),
                ),
            })
        }
        None => Err(unparseable(format!(
            "missing or non-boolean `ok` key (raw: {raw})"
        ))),
    }
}

/// [`REDUCED_MOTION_PROBE`] の返り値を検分し、撮影へ進んでよいか判定する。
///
/// [`freeze_verdict`] と同じ受理条件——`ok` が `true` であると確かめられた
/// 場合にだけ `Ok(())` を返し、それ以外はすべて失敗（fail-closed）:
///
/// - `ok: false` — **エミュレーションが効いていると確かめられなかった**。
///   `errors` にどちらの輪（CSS / matchMedia）が不成立だったかが載る
/// - 値が文字列でない／JSON として読めない／`ok` が無い・bool でない —
///   **検証結果を解析できなかった**。効いているかどうか自体が不明
///
/// どちらも既存の freeze 失敗と同じ [`RenderError::Story`] 経路を使う
/// （story 単位に隔離され、残りの story は撮り続けられる）。
fn reduced_motion_verdict(
    value: Option<&serde_json::Value>,
    story_id: &str,
) -> Result<(), RenderError> {
    let unparseable = |detail: String| RenderError::Story {
        story_id: story_id.to_string(),
        message: format!("reduced-motion verification result was unparseable: {detail}"),
    };
    let Some(raw) = value.and_then(|v| v.as_str()) else {
        return Err(unparseable(format!(
            "expected a JSON string, got {value:?}"
        )));
    };
    let parsed = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|e| unparseable(format!("{e} (raw: {raw})")))?;
    match parsed.get("ok").and_then(|v| v.as_bool()) {
        // 効いていると確かめられた。撮影へ進む。`ok` 以外のキーは検査しない
        // （将来プローブが返すものを増やしても正当な応答を弾かない）。
        Some(true) => Ok(()),
        Some(false) => {
            let errors_note = parsed
                .get("errors")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    let joined = arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!(" (errors: {joined})")
                })
                .unwrap_or_default();
            Err(RenderError::Story {
                story_id: story_id.to_string(),
                message: format!(
                    "reduced-motion emulation was requested but could not be \
                     verified as applied{errors_note}"
                ),
            })
        }
        None => Err(unparseable(format!(
            "missing or non-boolean `ok` key (raw: {raw})"
        ))),
    }
}

/// 警告に列挙する family 名の上限。超過分は「and N more」に畳む。
///
/// 根拠: この文字列は将来 story の失敗メッセージ経路（`StoryFailure.message`
/// → `summarize_story_failures` の先頭 10 件連結 → build `error_message` の
/// 2000 文字 truncate）へ載る可能性を見込んで有界にする。2000 文字を 10 件で
/// 割った 1 件あたり約 200 文字から、前置き＋対処の定型（約 130 文字）を
/// 引いた残りに、`unicode-range` 分割で最も起きやすい「1 family 名 ×20 重複」
/// を Set 一意化した後の現実的な family 名長（10〜20 文字）が 5 件で収まる。
const FONT_WARNING_MAX_FAMILIES: usize = 5;

/// [`FONTS_WAIT_SCRIPT`] の返り値を検分し、撮影へ進んでよいか判定する。
///
/// [`freeze_verdict`] / [`reduced_motion_verdict`] と同じ受理条件——`ok` が
/// `true` であると確かめられた場合にだけ `Ok(..)` を返し、それ以外は
/// すべて失敗（fail-closed）:
///
/// - `ok: true` — 揃ったと確かめられた。読み込みに**失敗**したフォントが
///   `failed`（スクリプト側で Set 一意化済みの family 一覧）に載っていれば、
///   撮影は止めずに警告文字列 `Ok(Some(..))` を返す（意図した fail-open——
///   失敗経路①。理由はモジュール doc）。一覧は
///   [`FONT_WARNING_MAX_FAMILIES`] 件で打ち切り、件数は一意化後の集合の
///   大きさで数える
/// - `ok: false` — **フォントが揃ったと確かめられなかった**。`errors` に
///   原因（`document.fonts` の欠落・形違い、`ready` の reject、列挙の
///   throw）が載る。解決後に読み込みが再開していた場合はここへ来ない——
///   スクリプト側が `ready` を待ち直す（失敗経路②。モジュール doc の表を参照）
/// - 値が文字列でない／JSON として読めない／`ok` が無い・bool でない —
///   **待ちの結果を解析できなかった**。揃ったかどうか自体が不明
///
/// 失敗はどちらも [`RenderError::Story`] 経路（story 単位に隔離。残りの
/// story は撮り続けられる）。
fn fonts_verdict(
    value: Option<&serde_json::Value>,
    story_id: &str,
) -> Result<Option<String>, RenderError> {
    let unparseable = |detail: String| RenderError::Story {
        story_id: story_id.to_string(),
        message: format!("fonts-ready wait result was unparseable: {detail}"),
    };
    let Some(raw) = value.and_then(|v| v.as_str()) else {
        return Err(unparseable(format!(
            "expected a JSON string, got {value:?}"
        )));
    };
    let parsed = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|e| unparseable(format!("{e} (raw: {raw})")))?;
    match parsed.get("ok").and_then(|v| v.as_bool()) {
        // 揃ったと確かめられた。撮影へ進む。`ok`・`failed` 以外のキーは
        // 検査しない（将来スクリプトが返すものを増やしても正当な応答を
        // 弾かない）。
        Some(true) => {
            let failed: Vec<&str> = parsed
                .get("failed")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if failed.is_empty() {
                return Ok(None);
            }
            let listed = failed
                .iter()
                .take(FONT_WARNING_MAX_FAMILIES)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            let more = failed.len().saturating_sub(FONT_WARNING_MAX_FAMILIES);
            let suffix = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            Ok(Some(format!(
                "{count} font(s) failed to load; captured with fallback glyphs: \
                 {listed}{suffix} — if the failing font is an external dependency \
                 whose responses vary between runs, the same build can produce \
                 different pictures; bundle the font files with the build or \
                 remove the @font-face reference to restore the intended glyphs",
                count = failed.len(),
            )))
        }
        Some(false) => {
            let errors_note = parsed
                .get("errors")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    let joined = arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!(" (errors: {joined})")
                })
                .unwrap_or_default();
            Err(RenderError::Story {
                story_id: story_id.to_string(),
                message: format!(
                    "fonts were not verified as loaded before the capture{errors_note}"
                ),
            })
        }
        None => Err(unparseable(format!(
            "missing or non-boolean `ok` key (raw: {raw})"
        ))),
    }
}

/// [`FONTS_RECHECK_PROBE`] の返り値を読む。
///
/// - `Ok(true)` — 検証はこの document で成立したまま（印あり・新しい波なし）。
///   撮影へ進んでよい
/// - `Ok(false)` — document が入れ替わった（印なし）か、新しい読み込み波が
///   始まっている（`status` が `'loaded'` でない）か、`document.fonts` が
///   壊れて status を読めない。**失敗ではなく「検証列をやり直せ」**——
///   確定的な判定（形チェック・待ち・警告）は次巡の [`FONTS_WAIT_SCRIPT`]
///   が同じ document に対して下す
/// - `Err` — 応答を解析できなかった（fail-closed。他の verdict と同じ扱い）
fn fonts_recheck_verdict(
    value: Option<&serde_json::Value>,
    story_id: &str,
) -> Result<bool, RenderError> {
    let unparseable = |detail: String| RenderError::Story {
        story_id: story_id.to_string(),
        message: format!("fonts recheck result was unparseable: {detail}"),
    };
    let Some(raw) = value.and_then(|v| v.as_str()) else {
        return Err(unparseable(format!(
            "expected a JSON string, got {value:?}"
        )));
    };
    let parsed = serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|e| unparseable(format!("{e} (raw: {raw})")))?;
    match parsed.get("verified").and_then(|v| v.as_bool()) {
        Some(verified) => {
            let status = parsed.get("status").and_then(|v| v.as_str());
            Ok(verified && status == Some("loaded"))
        }
        None => Err(unparseable(format!(
            "missing or non-boolean `verified` key (raw: {raw})"
        ))),
    }
}

/// 起動失敗が一時的（リトライで直りうる）かを判定する。
///
/// ブラウザプロセスの立ち上げそのものがこけた系
/// （websocket URL 解決のタイムアウト・起動直後の即死・起動時 I/O エラー）は、
/// CI の負荷やコールドスタートのゆらぎで起きうるのでリトライする。
/// 実行ファイルが見つからない等はここに当たらず（`CdpError::Io` 等）、即座に諦める。
fn is_transient_launch_error(source: &chromiumoxide::error::CdpError) -> bool {
    use chromiumoxide::error::CdpError;
    matches!(
        source,
        CdpError::LaunchTimeout(_) | CdpError::LaunchExit(..) | CdpError::LaunchIo(..)
    )
}

/// 使えそうな Chromium の実行ファイルを探す。
///
/// **本番はこれを使わない。** サーバーは `CHROMIUM_PATH` 設定だけを見る
/// （見つからない環境で暗黙にどこかの Chrome を掴むと再現性が壊れるため）。
/// これは開発とテストの利便のためのヘルパで、次の順に探す。
///
/// 1. 環境変数 `CHROMIUM_PATH`
/// 2. `PATH` 上の chromium / chrome 系
/// 3. Playwright のキャッシュ（`~/.cache/ms-playwright/chromium-*`。e2e が入れたもの）
pub fn discover_chromium() -> Option<String> {
    if let Ok(path) = std::env::var("CHROMIUM_PATH")
        && !path.trim().is_empty()
        && Path::new(&path).is_file()
    {
        return Some(path);
    }

    const NAMES: [&str; 5] = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
    ];
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate.to_str().map(str::to_string);
                }
            }
        }
    }

    playwright_chromium()
}

/// Playwright のブラウザキャッシュから chromium を探す。
///
/// ディレクトリ名にビルド番号が入る（`chromium-1234`）ので、glob 相当の走査をする。
/// 新しいビルド番号を優先したいので、名前の降順で最初に見つかったものを返す。
fn playwright_chromium() -> Option<String> {
    let base = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/ms-playwright"))
        })?;

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("chromium-"))
        })
        .collect();
    // `chromium-1234` の降順 = 新しいビルド優先。
    dirs.sort();
    dirs.reverse();

    for dir in dirs {
        for rel in ["chrome-linux64/chrome", "chrome-linux/chrome"] {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return candidate.to_str().map(str::to_string);
            }
        }
    }
    None
}

/// ストーリー 1 件を開く URL。
pub fn story_url(base_url: &str, story_id: &str) -> String {
    format!(
        "{}/iframe.html?id={}&viewMode=story",
        base_url.trim_end_matches('/'),
        urlencode(story_id)
    )
}

/// クエリ値用の最小限のパーセントエンコード（Storybook の ID は `a-z0-9-`、念のため）。
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_iframe_urls() {
        assert_eq!(
            story_url("http://127.0.0.1:1234", "components-button--primary"),
            "http://127.0.0.1:1234/iframe.html?id=components-button--primary&viewMode=story"
        );
        // 末尾スラッシュは二重にしない。
        assert_eq!(
            story_url("http://127.0.0.1:1/", "a--b"),
            "http://127.0.0.1:1/iframe.html?id=a--b&viewMode=story"
        );
    }

    #[test]
    fn escapes_unexpected_characters_in_story_ids() {
        assert_eq!(
            story_url("http://h", "a b&id=evil"),
            "http://h/iframe.html?id=a%20b%26id%3Devil&viewMode=story"
        );
    }

    fn cdp_error(story_id: &str) -> RenderError {
        RenderError::Cdp {
            story_id: story_id.to_string(),
            source: chromiumoxide::error::CdpError::Timeout,
        }
    }

    /// [`retry_once_on_cdp`] に、呼ばれるたびに用意した結果を先頭から返す
    /// 試行を渡し、（最終結果, 試行回数）を返す。
    async fn run_retry(
        script: Vec<Result<RenderedStory, RenderError>>,
    ) -> (Result<RenderedStory, RenderError>, u32) {
        let script = std::cell::RefCell::new(script);
        let calls = std::cell::Cell::new(0);
        let result = retry_once_on_cdp("s--d", || {
            calls.set(calls.get() + 1);
            let next = script.borrow_mut().remove(0);
            async move { next }
        })
        .await;
        (result, calls.get())
    }

    /// タブ単位の間欠 CDP 故障（本番実測: セッション確立メッセージの
    /// 取りこぼしでタブが文鎮化）は、新しいタブの 2 回目で回復する。
    #[tokio::test]
    async fn a_cdp_failure_is_retried_once_and_can_recover() {
        let ok = RenderedStory {
            png: vec![1, 2, 3],
            font_warning: None,
        };
        let (result, calls) = run_retry(vec![Err(cdp_error("s--d")), Ok(ok)]).await;
        assert_eq!(result.unwrap().png, vec![1, 2, 3]);
        assert_eq!(calls, 2);
    }

    /// 2 回目も CDP 失敗ならリトライを重ねず、環境失敗（即中断の分類）の
    /// まま返す——ブラウザ本体が死んでいる場合の即中断を骨抜きにしない。
    #[tokio::test]
    async fn a_second_cdp_failure_stops_retrying_and_stays_environmental() {
        let (result, calls) = run_retry(vec![Err(cdp_error("s--d")), Err(cdp_error("s--d"))]).await;
        assert!(matches!(result, Err(RenderError::Cdp { .. })));
        assert_eq!(calls, 2);
    }

    /// story 固有の失敗（Story / Timeout）はやり直しても同じ理由で落ちる
    /// だけなのでリトライしない。
    #[tokio::test]
    async fn story_scoped_failures_are_not_retried() {
        let story = RenderError::Story {
            story_id: "s--d".into(),
            message: "boom".into(),
        };
        let (result, calls) = run_retry(vec![Err(story)]).await;
        assert!(matches!(result, Err(RenderError::Story { .. })));
        assert_eq!(calls, 1);

        let timeout = RenderError::Timeout {
            story_id: "s--d".into(),
            timeout: Duration::from_secs(30),
            phase: "test",
        };
        let (result, calls) = run_retry(vec![Err(timeout)]).await;
        assert!(matches!(result, Err(RenderError::Timeout { .. })));
        assert_eq!(calls, 1);
    }

    /// 成功はそのまま返し、余計な試行をしない。
    #[tokio::test]
    async fn a_success_is_returned_without_a_second_attempt() {
        let ok = RenderedStory {
            png: vec![9],
            font_warning: None,
        };
        let (result, calls) = run_retry(vec![Ok(ok)]).await;
        assert_eq!(result.unwrap().png, vec![9]);
        assert_eq!(calls, 1);
    }

    /// 最小の「Storybook っぽい」バンドル（**ランタイム無し**）。
    ///
    /// `?id=` を読んで `#storybook-root` に何か描くだけ。シグナルを一切出さないので、
    /// DOM フォールバック経路の回帰テストになる。
    /// Chromium の起動を直列化するロック。
    ///
    /// 同じホストで複数インスタンスを同時に起動すると、まれに
    /// `LaunchExit(status 0)` で即死する（プロファイル/シングルトンの競合）。
    /// テストの本題ではないので、起動を跨がないように 1 本ずつ動かす。
    static BROWSER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// `expr`（bool を返す JS 式）が true になるまでポーリングする。
    ///
    /// 予算は 100 回 × 50ms = 5 秒。時間内に成立しなければ**その場で panic**
    /// する——旧来の同型コピー 4 箇所のうち 3 箇所はタイムアウト時に `break`
    /// で素通りし、後段の assert が「準備待ちの失敗」を別の失敗として誤解を
    /// 招くメッセージで報告していた。予算の調整もここ 1 箇所で済む。
    async fn wait_until(page: &chromiumoxide::page::Page, expr: &str) {
        for _ in 0..100 {
            if let Ok(result) = page.evaluate(expr).await
                && result.value().and_then(|v| v.as_bool()).unwrap_or(false)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("condition `{expr}` did not become true within the 5s polling budget");
    }

    fn write_fixture_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>html,body{margin:0;padding:0}</style></head>
<body><div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var colors = { 'demo-box--red': '#ff0000', 'demo-box--blue': '#0000ff' };
  var el = document.createElement('div');
  el.style.width = '100%';
  el.style.height = '100vh';
  el.style.background = colors[id] || '#00ff00';
  el.textContent = id;
  document.getElementById('storybook-root').appendChild(el);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// fake アドオンチャンネルのスキャフォールドつき story fixture を書き出す。
    ///
    /// 本物のプレビューランタイムと同じく `window.__STORYBOOK_ADDONS_CHANNEL__`
    /// を**後から**代入する形を再現する。以前は 10 個超の `write_*_bundle` が
    /// この約 12 行のスキャフォールドを各自で埋め込んでいた——差し替えるときに
    /// 直し漏れが出る形だったので 1 箇所へ寄せた。
    ///
    /// - `css`: `<style>` の中身
    /// - `root_html`: `#storybook-root` の**中**に置く初期 HTML（JS で組む fixture は空）
    /// - `on_channel_js`: チャンネル代入後（20ms 後）に実行される JS。描画を
    ///   済ませて自分で `channel.emit('storyRendered', ...)` を呼ぶ責務を持つ
    ///   （`storyErrored` や iframe load 後の emit が要る fixture もあるため
    ///   emit はスキャフォールド側で肩代わりしない）。URL の `?id=` は
    ///   `id` 変数として参照できる
    ///
    /// プロトコルや環境を意図的に壊す fixture（CSP 系 3 個・garbled・
    /// throwing・rAF 抑止）は、壊し方そのもの——CSP meta の位置や
    /// スキャフォールドより先に走る差し替え——が本題なので手書きのまま残す。
    fn write_story_html(root: &Path, css: &str, root_html: &str, on_channel_js: &str) {
        std::fs::write(
            root.join("iframe.html"),
            format!(
                r#"<!doctype html>
<html><head><style>
{css}
</style></head>
<body><div id="storybook-root">{root_html}</div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var listeners = {{}};
  var channel = {{
    on: function (event, cb) {{ (listeners[event] = listeners[event] || []).push(cb); }},
    emit: function (event, payload) {{
      (listeners[event] || []).forEach(function (cb) {{ cb(payload); }});
    }}
  }};
  // 本物のプレビューランタイムと同じく「あとから代入」する。
  setTimeout(function () {{
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {{
{on_channel_js}
    }}, 20);
  }}, 20);
</script>
</body></html>"#
            ),
        )
        .expect("write iframe.html");
    }

    /// Storybook のアドオンチャンネルを模したバンドル。
    ///
    /// - `demo-box--red`   : 塗ってから storyRendered
    /// - `demo-box--empty` : **何も描かずに** storyRendered（空ストーリーは正当）
    /// - `demo-box--boom`  : storyErrored
    fn write_storybook_runtime_bundle(root: &Path) {
        write_story_html(
            root,
            "html,body{margin:0;padding:0;background:#fff}",
            "",
            r#"      if (id === 'demo-box--boom') {
        channel.emit('storyErrored', { message: 'kaboom in the play function' });
        return;
      }
      if (id !== 'demo-box--empty') {
        var el = document.createElement('div');
        el.style.width = '100%';
        el.style.height = '100vh';
        el.style.background = '#ff0000';
        document.getElementById('storybook-root').appendChild(el);
      }
      channel.emit('storyRendered', id);"#,
        );
    }

    /// アニメーションとキャレットを持つバンドル。freeze の決定性検証用。
    ///
    /// - `demo-anim--spinner` : **無限** CSS アニメ（1.7s 周期の回転）。
    ///   周期を切りの悪い値にしてあるのは、2 回の撮影間隔が偶然 1 周期の
    ///   整数倍に一致して「未修正でも同じ絵」になる事故を避けるため
    /// - `demo-anim--caret`   : フォーカス済み入力欄。キャレットが明滅する
    /// - `demo-anim--slide`   : **有限** CSS アニメ（60s, `fill: forwards`）。
    ///   初期は赤・終端は青。freeze が「終端へシークした」ことを色で検証できる
    fn write_animated_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
  .spinner {
    width:120px;height:120px;margin:20px;
    border:24px solid #dddddd;border-top-color:#ff0000;border-radius:50%;
    animation: spin 1.7s linear infinite;
  }
  @keyframes to-blue { from { background:#ff0000; } to { background:#0000ff; } }
  .slide { width:100%;height:100vh;animation: to-blue 60s linear 1 forwards; }
  input { font:32px monospace;width:200px;margin:40px;border:1px solid #000; }"#,
            "",
            r#"      var root = document.getElementById('storybook-root');
      if (id === 'demo-anim--spinner') {
        var el = document.createElement('div');
        el.className = 'spinner';
        root.appendChild(el);
      } else if (id === 'demo-anim--caret') {
        var input = document.createElement('input');
        root.appendChild(input);
        input.focus();
      } else if (id === 'demo-anim--slide') {
        var el = document.createElement('div');
        el.className = 'slide';
        root.appendChild(el);
      }
      channel.emit('storyRendered', id);"#,
        );
    }

    /// 非同期 web フォントを使うバンドル。フォント到着と撮影タイミングの競争検証用。
    ///
    /// - `demo-font--text` : `@font-face` の webfont を使うテキスト（ラテン＋日本語）。
    ///   本物の Storybook preview と同じく、`storyRendered` は**フォントを待たずに**
    ///   出す（フォント読み込みは style/layout が要求してから走る非同期処理で、
    ///   描画完了シグナルはそれを待たない）。
    ///
    /// 右上 40x40 の marker が `document.fonts.check()` の rAF ポーリングで
    /// 赤（未着）→緑（適用済み）に変わるため、撮影された絵自身が
    /// 「撮影時点で webfont が揃っていたか」を証言する。差分の出た二枚を
    /// 見比べたとき、glyph の違いと marker の色が必ず連動する——これが
    /// 「差分はフォント差である」ことを画像内で分ける手掛かりになる。
    ///
    /// フォント本体はバンドルに置かず、テスト側の遅延配信ルート（`/font.ttf`）が
    /// 配る。到着タイミングをテストが決定的に制御するためである。
    fn write_webfont_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  @font-face { font-family: 'VrtTestFont'; src: url('font.ttf'); }
  .webfont-text { font-family: 'VrtTestFont', monospace; font-size: 40px; }
  #font-marker { position: fixed; top: 0; right: 0; width: 40px; height: 40px; background: #cc0000; }"#,
            "",
            r#"      var root = document.getElementById('storybook-root');
      var text = document.createElement('div');
      text.className = 'webfont-text';
      text.textContent = 'Hamburgefonstiv 0123456789 いろはにほへと 撮影';
      root.appendChild(text);
      var marker = document.createElement('div');
      marker.id = 'font-marker';
      root.appendChild(marker);
      (function poll() {
        if (document.fonts.check("40px 'VrtTestFont'")) {
          marker.style.background = '#00cc00';
        } else {
          requestAnimationFrame(poll);
        }
      })();
      channel.emit('storyRendered', id);"#,
        );
    }

    /// 二波でフォントを読むバンドル。`ready` 解決**後**に新たなフォント要求が
    /// 始まるページの検証用（失敗経路②）。
    ///
    /// - `demo-font--two-waves` : 一波（`VrtTestFont`）を `document.fonts.load`
    ///   で明示的に開始してから `document.fonts.ready` にハンドラを載せ、
    ///   その解決時（＝一波完了の瞬間）に二波（`VrtTestFont2` のテキスト追加＋
    ///   `document.fonts.load`）を始める。
    ///
    /// 決定性の要: ページのハンドラは [`FONTS_WAIT_SCRIPT`] より**先に**
    /// 同じ `ready` promise へ登録される（story 実行時 vs SETTLE_DELAY 後の
    /// evaluate）。promise のハンドラは登録順に走るため、FONTS_WAIT_SCRIPT が
    /// `status` を読む時点で二波の読み込みが**必ず**始まっている——
    /// 「たまたま窓に入った時だけ落ちる」flaky ではなく、毎回同じ順序で
    /// 二波が観測される。
    ///
    /// marker は二つ（右上=一波・その左=二波）。それぞれ
    /// `document.fonts.check()` で赤（未着）→緑（適用済み）に変わり、
    /// 撮影された絵自身が「どの波まで揃った時点の絵か」を証言する。
    fn write_two_wave_webfont_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  @font-face { font-family: 'VrtTestFont'; src: url('font.ttf'); }
  @font-face { font-family: 'VrtTestFont2'; src: url('font2.ttf'); }
  .webfont-text { font-family: 'VrtTestFont', monospace; font-size: 40px; }
  .webfont-text2 { font-family: 'VrtTestFont2', monospace; font-size: 40px; }
  #font-marker { position: fixed; top: 0; right: 0; width: 40px; height: 40px; background: #cc0000; }
  #font2-marker { position: fixed; top: 0; right: 48px; width: 40px; height: 40px; background: #cc0000; }"#,
            "",
            r#"      var root = document.getElementById('storybook-root');
      var text = document.createElement('div');
      text.className = 'webfont-text';
      text.textContent = 'First wave Hamburgefonstiv いろは';
      root.appendChild(text);
      var marker = document.createElement('div');
      marker.id = 'font-marker';
      root.appendChild(marker);
      var marker2 = document.createElement('div');
      marker2.id = 'font2-marker';
      root.appendChild(marker2);
      // 一波を明示的に始めてから ready にハンドラを載せる。こうすると ready は
      // 「一波の完了を待つ pending promise」で、このハンドラは後から同じ
      // promise へ載る FONTS_WAIT_SCRIPT のハンドラより先（登録順）に走る。
      document.fonts.load("40px 'VrtTestFont'");
      var secondWaveStarted = false;
      document.fonts.ready.then(function () {
        if (secondWaveStarted) return;
        secondWaveStarted = true;
        var text2 = document.createElement('div');
        text2.className = 'webfont-text2';
        text2.textContent = 'Second wave 二波';
        root.appendChild(text2);
        document.fonts.load("40px 'VrtTestFont2'");
      });
      (function poll() {
        var a = document.fonts.check("40px 'VrtTestFont'");
        var b = document.fonts.check("40px 'VrtTestFont2'");
        if (a) marker.style.background = '#00cc00';
        if (b) marker2.style.background = '#00cc00';
        if (!(a && b)) requestAnimationFrame(poll);
      })();
      channel.emit('storyRendered', id);"#,
        );
    }

    /// システムに実在する TTF を探す（webfont fixture の素材）。
    ///
    /// フォントのバイト列自体はテストの本題ではない——「fallback と字形の違う
    /// 実フォントが非同期に届く」ことだけが要る。リポジトリにフォントを
    /// 同梱しない（ライセンス表記の管理を増やさない）ため環境から探す。
    ///
    /// 探索順: `VRT_TEST_FONT` 環境変数（既知パスにも `fc-match` にも期待
    /// できない環境——macOS・fontconfig の無い slim イメージ等——の明示的な
    /// 逃げ道）→ DejaVu / Liberation / Noto の既知パス → `fc-match` に
    /// serif の実ファイルを聞く。どの経路でも先頭 4 バイトの sfnt magic で
    /// 選別する——`fc-match` は環境次第で TrueType Collection（`.ttc`。
    /// Noto CJK 等）を返すが、Chromium は `.ttc` を webfont として読めず、
    /// 掴むとフォント試験群が「届いたのに適用されない」で全滅するためである。
    ///
    /// それでも見つからない場合の扱いは [`require_test_font`] を参照
    /// （chromium がある環境では SKIP ではなく panic）。
    fn discover_test_font() -> Option<Vec<u8>> {
        // Chromium が webfont として読める単体フォントの sfnt magic:
        // TrueType（0x00010000）・OpenType/CFF（'OTTO'）・旧 Apple 形（'true'）。
        // 'ttcf'（TrueType Collection）は載せない。
        fn chromium_webfont(bytes: Vec<u8>) -> Option<Vec<u8>> {
            match bytes.get(..4) {
                Some([0x00, 0x01, 0x00, 0x00]) | Some(b"OTTO") | Some(b"true") => Some(bytes),
                _ => None,
            }
        }
        if let Ok(path) = std::env::var("VRT_TEST_FONT")
            && !path.trim().is_empty()
        {
            // 明示指定が読めない・.ttc だった場合もフォールバックへ進まず
            // None（→ require 側で panic）にする——指定が黙って無視され
            // 別のフォントで走るほうが原因に辿り着きにくい。
            return std::fs::read(path.trim()).ok().and_then(chromium_webfont);
        }
        const CANDIDATES: [&str; 8] = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
            "/usr/share/fonts/TTF/DejaVuSerif.ttf",
            "/usr/share/fonts/dejavu/DejaVuSerif.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
            "/usr/share/fonts/liberation/LiberationSerif-Regular.ttf",
            "/usr/share/fonts/TTF/LiberationSerif-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSerif-Regular.ttf",
            "/usr/share/fonts/noto/NotoSerif-Regular.ttf",
        ];
        if let Some(bytes) = CANDIDATES
            .iter()
            .find_map(|path| std::fs::read(path).ok().and_then(chromium_webfont))
        {
            return Some(bytes);
        }
        let output = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", "serif"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8(output.stdout).ok()?;
        let path = path.trim();
        if path.is_empty() {
            return None;
        }
        std::fs::read(path).ok().and_then(chromium_webfont)
    }

    /// [`discover_test_font`] の必須版。**chromium が見つかっている**文脈で
    /// 呼ぶこと——ブラウザはあるのにフォントだけ無い環境で、この PR の
    /// 証拠となるフォント試験群が全て SKIP して「フォント経路は一度も
    /// 検証されていないのにスイートは緑」になる沈黙を、SKIP ではなく
    /// panic で表面化させる。候補（環境変数・既知パス・`fc-match`）が
    /// **尽きたときだけ** panic し、メッセージは逃げ道（`VRT_TEST_FONT`）と
    /// `.ttc` を弾く理由を名指しする。
    fn require_test_font() -> Vec<u8> {
        discover_test_font().unwrap_or_else(|| {
            panic!(
                "chromium is available but no usable test font was found (checked \
                 $VRT_TEST_FONT, the DejaVu/Liberation/Noto serif paths, and \
                 `fc-match -f '%{{file}}' serif`; TrueType Collections (.ttc) are \
                 rejected because Chromium cannot load them as webfonts) — set \
                 VRT_TEST_FONT to a single-font .ttf/.otf file instead of letting \
                 the font tests silently skip"
            )
        })
    }

    /// バンドル配信 + `/font.ttf` だけ初回リクエストを `first_hit_delay` 遅らせる
    /// 使い捨てサーバー。
    ///
    /// 本番は build ごとに新規 `user_data_dir` で Chromium を起動する（キャッシュは
    /// 毎 build 冷えている）。フォントの初回取得だけが遅く、以降は
    /// ディスク/メモリキャッシュで即答になる——その冷え/温みの非対称を、
    /// 「初回だけ遅い」ルートで決定的に再現する。`Cache-Control: no-store` を
    /// 返すのはリクエスト数を実測可能にするため（ブラウザキャッシュに吸われず、
    /// 何回ネットワークまで取りに来たかを hits で数えられる）。
    async fn start_font_delay_server(
        root: &Path,
        font_bytes: Vec<u8>,
        first_hit_delay: Duration,
    ) -> (
        SocketAddr,
        JoinHandle<()>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let hits_in_route = hits.clone();
        let app = axum::Router::new()
            .route(
                "/font.ttf",
                axum::routing::get(move || {
                    let hits = hits_in_route.clone();
                    let bytes = font_bytes.clone();
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            tokio::time::sleep(first_hit_delay).await;
                        }
                        (
                            [
                                (axum::http::header::CONTENT_TYPE, "font/ttf"),
                                (axum::http::header::CACHE_CONTROL, "no-store"),
                            ],
                            bytes,
                        )
                    }
                }),
            )
            .fallback_service(tower_http::services::ServeDir::new(root));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind font delay server");
        let addr = listener.local_addr().expect("local_addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, task, hits)
    }

    /// 二波バンドル配信 + `/font.ttf`・`/font2.ttf` それぞれの初回リクエストを
    /// 独立に遅らせる使い捨てサーバー。
    ///
    /// [`start_font_delay_server`] の二路版。一波の遅延は「FONTS_WAIT_SCRIPT が
    /// ready へ載った**後**に一波が解決する」順序を作るため（遅延なしだと
    /// SETTLE_DELAY の間に二波まで済み、②の窓が閉じてしまう）。二波の遅延は
    /// 「二波が永遠に終わらないページ」（→ 共有 deadline の Timeout）を
    /// 決定的に作るために使う。
    async fn start_two_wave_font_server(
        root: &Path,
        font_bytes: Vec<u8>,
        font_first_hit_delay: Duration,
        font2_first_hit_delay: Duration,
    ) -> (SocketAddr, JoinHandle<()>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        fn delayed_font_route(
            bytes: Vec<u8>,
            first_hit_delay: Duration,
        ) -> axum::routing::MethodRouter {
            let hits = std::sync::Arc::new(AtomicUsize::new(0));
            axum::routing::get(move || {
                let hits = hits.clone();
                let bytes = bytes.clone();
                async move {
                    let n = hits.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        tokio::time::sleep(first_hit_delay).await;
                    }
                    (
                        [
                            (axum::http::header::CONTENT_TYPE, "font/ttf"),
                            (axum::http::header::CACHE_CONTROL, "no-store"),
                        ],
                        bytes,
                    )
                }
            })
        }
        let app = axum::Router::new()
            .route(
                "/font.ttf",
                delayed_font_route(font_bytes.clone(), font_first_hit_delay),
            )
            .route(
                "/font2.ttf",
                delayed_font_route(font_bytes, font2_first_hit_delay),
            )
            .fallback_service(tower_http::services::ServeDir::new(root));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind two-wave font server");
        let addr = listener.local_addr().expect("local_addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, task)
    }

    /// 【cmd_656・実測再現 / positive control】フォントを条件待ちしないと、
    /// 同じバンドル・同じブラウザで同じ story を繰り返し撮ってもバイト列が
    /// 一致しないこと。
    ///
    /// 本番の形（build ごとに冷えたプロファイルで多数の story を順に撮る）を
    /// 「初回だけ遅いフォント配信」で縮約する: 1 回目の撮影ではフォントが
    /// SETTLE_DELAY（250ms）に間に合わず、2 回目以降はキャッシュ相当で即着する。
    /// 差分は**序盤に偏り**、外れた絵はすべて marker 赤（フォント未着）——
    /// それがフォント読み込み競争の指紋である（cmd_656 の初回実測: 8 回中
    /// 1 回・run 0 のみ不一致・marker と完全連動）。
    ///
    /// `wait_for_fonts = false` はテスト専用の裏口。この試験は
    /// `waiting_for_fonts_ready_makes_repeated_captures_agree` の陽性対照で、
    /// 「fixture が本当に非同期にフォントを読み、待たなければ本当に揺れる」
    /// ことを固定する——これが落ちるなら一致側の試験は何も検証していない。
    #[tokio::test(flavor = "multi_thread")]
    async fn settle_delay_alone_lets_the_font_race_the_capture() {
        use std::sync::atomic::Ordering;

        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP settle_delay_alone_lets_the_font_race_the_capture: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_webfont_bundle(dir.path());
        let (addr, server_task, hits) =
            start_font_delay_server(dir.path(), font, Duration::from_millis(600)).await;
        let base_url = format!("http://{addr}");

        // 裏口: フォント条件待ちを切り、SETTLE_DELAY の時間待ちだけの
        // 修正前の撮影過程を再現する。
        let mut options = RenderOptions::new(chromium, 640, 360);
        options.wait_for_fonts = false;
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");

        const RUNS: usize = 8;
        let mut hashes: Vec<String> = Vec::new();
        let mut markers: Vec<(u8, u8, u8)> = Vec::new();
        for i in 0..RUNS {
            let png = renderer
                .render_story(&base_url, "demo-font--text")
                .await
                .expect("render story")
                .png;
            // PR #11 の content_hash と同じ物差しで測る。
            let hash = crate::screenshots::content_hash(&png);
            let image = image::ImageReader::with_format(
                std::io::Cursor::new(&png),
                image::ImageFormat::Png,
            )
            .decode()
            .expect("decode screenshot")
            .to_rgba8();
            let px = image.get_pixel(640 - 10, 10);
            let marker = (px[0], px[1], px[2]);
            eprintln!(
                "run {i}: hash={} marker={:?} font_fetches_so_far={}",
                &hash[..23],
                marker,
                hits.load(Ordering::SeqCst)
            );
            hashes.push(hash);
            markers.push(marker);
        }
        renderer.close().await;
        server_task.abort();

        // doc の「hits でリクエスト数を実測可能にする」を宣言で終わらせず、
        // ここで実測して判定に使う: `Cache-Control: no-store` が効いていれば
        // 全 run がネットワークまで取りに来る（run ごとに新 document で
        // フォントを要求し、キャッシュには吸われない）。これが崩れると
        // 「初回だけ遅い」フェッチ遅延が後続 run に効かなくなり、この
        // fixture の縮約（冷え/温みの非対称）自体が壊れている。
        assert!(
            hits.load(Ordering::SeqCst) >= RUNS,
            "no-store must force every run to fetch the font over the network \
             (the browser may legitimately fetch more than once per run) — a \
             count below RUNS means the browser cache absorbed requests and the \
             fixture no longer reproduces the cold/warm asymmetry, got {}",
            hits.load(Ordering::SeqCst)
        );

        // 集計: 最頻ハッシュ（=温まった安定状態）から外れた run を数える。
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for h in &hashes {
            *counts.entry(h.as_str()).or_default() += 1;
        }
        let modal = counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(h, _)| h.to_string())
            .expect("at least one hash");
        let outliers: Vec<usize> = (0..RUNS).filter(|&i| hashes[i] != modal).collect();
        eprintln!(
            "outlier runs (differ from modal hash): {:?} / total {RUNS}",
            outliers
        );

        assert!(
            !outliers.is_empty(),
            "reproduction failed: all {RUNS} captures of the same build agreed byte-for-byte"
        );
        // 序盤偏り: 外れ run はすべて「フォント未着（marker 赤）」で、
        // 安定 run はすべて「フォント適用済み（marker 緑）」——差分が
        // フォント差であることを絵の中の証言で結びつける。
        for &i in &outliers {
            assert_eq!(
                markers[i],
                (0xcc, 0x00, 0x00),
                "run {i} differs from the modal image but its marker is not red (font-pending)"
            );
        }
        for i in (0..RUNS).filter(|i| !outliers.contains(i)) {
            assert_eq!(
                markers[i],
                (0x00, 0xcc, 0x00),
                "run {i} matches the modal image but its marker is not green (font-applied)"
            );
        }
    }

    /// 【cmd_656・修正の実測】`document.fonts.ready` を条件待ちすると、
    /// 陽性対照（`settle_delay_alone_lets_the_font_race_the_capture`）と
    /// **同じ**遅延フォント配信でも、繰り返し撮った絵がバイト単位で一致する。
    ///
    /// 一致だけでなく marker が全 run で緑（フォント適用済み）であることも
    /// 確かめる——「フォントが一度も当たらないから毎回同じ」という
    /// 偽りの一致（fallback で揃っただけ）をここで弾く。
    #[tokio::test(flavor = "multi_thread")]
    async fn waiting_for_fonts_ready_makes_repeated_captures_agree() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP waiting_for_fonts_ready_makes_repeated_captures_agree: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_webfont_bundle(dir.path());
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font, Duration::from_millis(600)).await;
        let base_url = format!("http://{addr}");

        // 本番と同じ既定（wait_for_fonts = true）。
        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 640, 360))
            .await
            .expect("launch chromium");

        const RUNS: usize = 8;
        let mut hashes: Vec<String> = Vec::new();
        for i in 0..RUNS {
            let png = renderer
                .render_story(&base_url, "demo-font--text")
                .await
                .expect("render story")
                .png;
            let hash = crate::screenshots::content_hash(&png);
            let image = image::ImageReader::with_format(
                std::io::Cursor::new(&png),
                image::ImageFormat::Png,
            )
            .decode()
            .expect("decode screenshot")
            .to_rgba8();
            let px = image.get_pixel(640 - 10, 10);
            assert_eq!(
                (px[0], px[1], px[2]),
                (0x00, 0xcc, 0x00),
                "run {i}: the font marker must be green (webfont applied) — \
                 agreement via never-loading fallback would be a false pass"
            );
            hashes.push(hash);
        }
        renderer.close().await;
        server_task.abort();

        assert!(
            hashes.iter().all(|h| h == &hashes[0]),
            "all captures of the same build must agree byte-for-byte once \
             fonts are condition-waited, got: {hashes:?}"
        );
    }

    /// 【cmd_656・fail-closed の実測】フォントが期限内に届かないページは
    /// **撮らずに** [`RenderError::Timeout`]（[`FONTS_PHASES`] の phase）で
    /// 落ちる。同じページを修正前の撮影過程（`wait_for_fonts = false` の
    /// 裏口）に通すと**撮れてしまう**——これが塞いだ穴の実測である。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_font_that_never_arrives_fails_instead_of_capturing_fallback_glyphs() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_font_that_never_arrives_fails: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_webfont_bundle(dir.path());
        // 「初回リクエストを 1 時間遅らせる」＝この試験の時間内には決して
        // 届かないフォント。story_timeout（下の 5 秒）が先に尽きる。
        // 遅延は**初回ヒットにだけ**掛かるため、render ごとに独立の
        // サーバーを立てる——一つを共用すると先の render がヒットを消費し、
        // 後の render には即応してしまう（本試験の初版がその形で偽陰性を出した）。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font.clone(), Duration::from_secs(3600)).await;

        // 修正前の過程（裏口）: フォントが届かなくても代替字形のまま
        // 撮れてしまっていた——穴が実在したことの陽性対照。
        let mut unfixed = RenderOptions::new(chromium.clone(), 640, 360);
        unfixed.story_timeout = Duration::from_secs(5);
        unfixed.wait_for_fonts = false;
        let renderer = StoryRenderer::launch(unfixed)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "demo-font--text")
            .await
            .expect("without the fonts wait the capture silently proceeds")
            .png;
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        renderer.close().await;
        server_task.abort();

        // 修正後（本番既定）: 撮らずに Timeout（fail-closed）。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font, Duration::from_secs(3600)).await;
        let mut fixed = RenderOptions::new(chromium, 640, 360);
        fixed.story_timeout = Duration::from_secs(5);
        let renderer = StoryRenderer::launch(fixed).await.expect("launch chromium");
        let err = renderer
            .render_story(&format!("http://{addr}"), "demo-font--text")
            .await
            .expect_err("a never-arriving font must fail the story, not capture fallback glyphs");
        renderer.close().await;
        server_task.abort();

        match err {
            RenderError::Timeout { phase, .. } => {
                assert_eq!(
                    phase, FONTS_PHASES.timeout,
                    "the failure must be attributed to the fonts wait stage"
                );
            }
            other => panic!("expected a fonts-wait Timeout, got: {other:?}"),
        }
    }

    /// 【cmd_657・失敗経路②】`ready` 解決後に新たなフォント要求（二波）が
    /// 始まるページは、失敗ではなく**待ち直し**で収束し、繰り返し撮っても
    /// バイト単位で一致すること。
    ///
    /// 待ち直し前の実装（`ready` 解決後の `status` 単発読みで `'loading'` なら
    /// 即失敗）では、この fixture は **8/8 で毎回失敗**した（cmd_657 実測。
    /// エラーは常に「fonts.ready resolved but new font loads have already
    /// started」）——二波の開始はハンドラ登録順で FONTS_WAIT_SCRIPT の
    /// `status` 読みより必ず先に観測されるため、窓に依る flaky ではなく
    /// 決定的な誤分類だった——読み込み**中**（いずれ届く）を読み込み
    /// **失敗**と同列に扱い、フォントが問題なく届くページを恒久的に
    /// 撮影不能へ誤分類していた形である。
    ///
    /// 一致だけでなく marker が両波とも緑（フォント適用済み）であることも
    /// 確かめる——どちらかの波が一度も当たらないまま揃った偽の一致を弾く。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_font_wave_after_ready_is_awaited_not_failed() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_second_font_wave_after_ready_is_awaited: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_two_wave_webfont_bundle(dir.path());

        const RUNS: usize = 2;
        let mut hashes: Vec<String> = Vec::new();
        for i in 0..RUNS {
            // 「初回だけ遅い」一波の遅延を run ごとに効かせるため、render ごとに
            // 独立のサーバーを立てる（`a_font_that_never_arrives_*` と同じ理由）。
            let (addr, server_task) = start_two_wave_font_server(
                dir.path(),
                font.clone(),
                Duration::from_millis(600),
                Duration::ZERO,
            )
            .await;
            let renderer = StoryRenderer::launch(RenderOptions::new(chromium.clone(), 640, 360))
                .await
                .expect("launch chromium");
            let png = renderer
                .render_story(&format!("http://{addr}"), "demo-font--two-waves")
                .await
                .expect("a page that loads fonts in two waves must converge and capture")
                .png;
            let image = image::ImageReader::with_format(
                std::io::Cursor::new(&png),
                image::ImageFormat::Png,
            )
            .decode()
            .expect("decode screenshot")
            .to_rgba8();
            let wave1 = image.get_pixel(640 - 10, 10);
            let wave2 = image.get_pixel(640 - 48 - 10, 10);
            assert_eq!(
                (wave1[0], wave1[1], wave1[2]),
                (0x00, 0xcc, 0x00),
                "run {i}: the first-wave marker must be green (font applied)"
            );
            assert_eq!(
                (wave2[0], wave2[1], wave2[2]),
                (0x00, 0xcc, 0x00),
                "run {i}: the second-wave marker must be green — capturing before \
                 the second wave settles would be the pre-fix misclassification's twin"
            );
            hashes.push(crate::screenshots::content_hash(&png));
            renderer.close().await;
            server_task.abort();
        }
        assert!(
            hashes.iter().all(|h| h == &hashes[0]),
            "captures of the same two-wave build must agree byte-for-byte, got: {hashes:?}"
        );
    }

    /// 【cmd_657・失敗経路④の堅持】待ち直しを入れても fail-closed は壊れて
    /// いないこと——二波目が**永遠に来ない**ページは、撮らずに共有 deadline の
    /// [`RenderError::Timeout`]（[`FONTS_PHASES`] の phase）へ倒れる。
    ///
    /// 待ち直しに回数上限が無いことの安全弁がこの deadline である。
    /// この試験が落ちたら、待ち直しが「撮れないページを黙って撮る」方向へ
    /// 壊れたことを意味する。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_wave_that_never_ends_times_out_without_capturing() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_second_wave_that_never_ends: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_two_wave_webfont_bundle(dir.path());
        // 一波は即応・二波は 1 時間遅延＝この試験の時間内には決して届かない。
        // story_timeout（5 秒）が先に尽きる。
        let (addr, server_task) =
            start_two_wave_font_server(dir.path(), font, Duration::ZERO, Duration::from_secs(3600))
                .await;

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(5);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let err = renderer
            .render_story(&format!("http://{addr}"), "demo-font--two-waves")
            .await
            .expect_err("a never-ending second wave must time out, not capture mid-wave");
        renderer.close().await;
        server_task.abort();

        match err {
            RenderError::Timeout { phase, .. } => {
                assert_eq!(
                    phase, FONTS_PHASES.timeout,
                    "the failure must be attributed to the fonts wait stage"
                );
            }
            other => panic!("expected a fonts-wait Timeout, got: {other:?}"),
        }
    }

    /// 【cmd_658→cmd_659・失敗経路①の固定】フォントの**読み込み失敗**（404 等）
    /// は story を落とさない——代替字形のまま**撮れて**、警告が残り、絵が
    /// **決定的**（繰り返し撮ってバイト一致）であること。marker が赤のまま
    /// （フォントは本当に失敗している）であることも確かめ、「実は読めていた」
    /// への劣化を弾く。
    ///
    /// cmd_658 は (a) fail-closed（1 つでも error なら story 失敗）を選んだが、
    /// cmd_659（yupix レビュー）の新しい実証で (b) 撮って警告へ転換した——
    /// 原因の `@font-face` は preview-head 等で **project 全体に共有**され、
    /// egress の無いワーカーの外部フォント参照や `local()` 前提の宣言ひとつで、
    /// そのフォントを一切表示しない story まで全 story が落ちる（修正前は
    /// fallback で決定的に緑だった）。(b) は cmd_658 が明示的に許した道であり
    /// 後戻りではない。害の比較はモジュール doc 失敗経路①を参照。
    /// **黙って fail-closed へ戻す変更はこの試験を落とす**——戻すなら
    /// あの害の比較ごと書き直すこと。
    ///
    /// 警告の文言・一意化後の件数・上限は `fonts_verdict_turns_failed_fonts_
    /// into_a_bounded_warning`（単体）が固定する。ここでは実ブラウザ貫通で
    /// 「撮れる・決定的・フォントは本当に失敗・**警告が成功値に載る**」を
    /// 固定する（cmd_660 C/F）。
    ///
    /// 警告の assert は同時に **`failed` キーの契約の script 側**を固定する
    /// （cmd_660 F）: 警告は [`FONTS_WAIT_SCRIPT`] が実ブラウザで `failed` に
    /// family を載せて返した場合にだけ生まれる（[`fonts_verdict`] は `failed`
    /// 欠落を「旧形の応答＝警告なしの成功」として受けるため、script 側が
    /// `failed` を落とす変更を入れると警告だけが静かに消える——この試験が
    /// その変更を落とす）。受理側の契約は `fonts_verdict_turns_failed_fonts_
    /// into_a_bounded_warning`（単体）が固定し、両側で照合が閉じる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_font_load_captures_with_a_warning_and_stays_deterministic() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_failing_font_load_captures_with_a_warning: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        // バンドルは font.ttf を参照するが、ファイルは書かない——素の静的
        // 配信で /font.ttf は 404 になり、「到達不能なフォント」を決定的に
        // 再現する（遅延サーバー不要。フォント素材も不要）。
        write_webfont_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        // 本番既定（wait_for_fonts = true）: 撮れる。落ちない。
        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 640, 360))
            .await
            .expect("launch chromium");
        const RUNS: usize = 2;
        let mut hashes: Vec<String> = Vec::new();
        for i in 0..RUNS {
            let rendered = renderer
                .render_story(&server.base_url(), "demo-font--text")
                .await
                .expect("a failing font load must capture with fallback glyphs, not fail");
            // 警告が成功値に載っていること（cmd_660 C/F）。この assert が
            // 通る＝実ブラウザの FONTS_WAIT_SCRIPT が `failed` に family を
            // 載せて返した、ということ——`failed` を落とす変更はここで落ちる。
            let warning = rendered.font_warning.as_deref().unwrap_or_else(|| {
                panic!(
                    "run {i}: a failing font load must surface a warning on the \
                     success value — silence means the failed-fonts channel \
                     (FONTS_WAIT_SCRIPT's `failed` key) broke without any test noticing"
                )
            });
            assert!(
                warning.contains("VrtTestFont"),
                "run {i}: the warning must name the failing family, got: {warning}"
            );
            assert!(
                warning.contains("fallback glyphs"),
                "run {i}: the warning must say the capture used fallback glyphs, got: {warning}"
            );
            assert!(
                warning.contains("different pictures"),
                "run {i}: the warning must say varying external responses can \
                 produce different pictures from the same build, got: {warning}"
            );
            let png = rendered.png;
            let image = image::ImageReader::with_format(
                std::io::Cursor::new(&png),
                image::ImageFormat::Png,
            )
            .decode()
            .expect("decode screenshot")
            .to_rgba8();
            let px = image.get_pixel(640 - 10, 10);
            assert_eq!(
                (px[0], px[1], px[2]),
                (0xcc, 0x00, 0x00),
                "run {i}: the font marker must stay red (the font really failed) — \
                 a green marker means this test no longer exercises a load failure"
            );
            hashes.push(crate::screenshots::content_hash(&png));
        }
        renderer.close().await;

        // 決定的であること——(b) の前提「恒久的に届かないフォントは代替字形へ
        // 決定的に倒れる」を宣言で終わらせず実測で固定する。
        assert!(
            hashes.iter().all(|h| h == &hashes[0]),
            "captures with a permanently failing font must agree byte-for-byte, \
             got: {hashes:?}"
        );
    }

    /// `storyRendered` の後に一度だけ自分を reload するバンドル。
    /// フォント検証と撮影の間の窓（yupix 指摘・PR #27 三巡目）の実測用。
    ///
    /// 二相を sessionStorage で分ける（reload しても同一タブ内で持続する）:
    ///
    /// - **一巡目**: フォントを一切使わない（fonts 待ちは即 ok）。rAF を
    ///   握りつぶして freeze evaluate を pending のまま生かし、その間に
    ///   `location.reload()` で実行コンテキストごと破壊する
    /// - **二巡目**: rAF は素のまま。webfont（`/font.ttf`・テスト側が配信を
    ///   遅らせる）を要求し、右上 marker が `document.fonts.check()` で
    ///   赤（未着）→緑（適用済み）に変わる——撮られた絵自身が
    ///   「フォント未検証の document を撮ったか」を証言する
    ///
    /// 順序は決定的: fonts 待ちは一巡目で ok（READY 検出 ≤ 数百 ms + settle
    /// 250ms より reload の 2.5 秒は十分後）、freeze は rAF 不発で reload まで
    /// 必ず pending、reload 後は freeze が二巡目 document で成功する。
    fn write_reload_after_fonts_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head>
<script>
  // 一巡目だけ rAF を握りつぶす（コールバックは保持——捨てると Promise ごと
  // GC され「Promise was collected」という別経路になる）。
  if (!sessionStorage.getItem('vrtReloaded')) {
    window.__rafCallbacks = [];
    window.requestAnimationFrame = function (cb) {
      window.__rafCallbacks.push(cb);
      return window.__rafCallbacks.length;
    };
  }
</script>
<style>
  html,body{margin:0;padding:0;background:#fff}
  @font-face { font-family: 'VrtTestFont'; src: url('font.ttf'); }
  .webfont-text { font-family: 'VrtTestFont', monospace; font-size: 40px; }
  #box { width:100%;height:100vh;background:#00ff00; }
  #font-marker { position: fixed; top: 0; right: 0; width: 40px; height: 40px; background: #cc0000; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  var reloaded = !!sessionStorage.getItem('vrtReloaded');
  var root = document.getElementById('storybook-root');
  if (!reloaded) {
    // 一巡目: フォントを使わない緑のベタ塗り。fonts 待ちはここで ok を返す。
    var box = document.createElement('div');
    box.id = 'box';
    root.appendChild(box);
  } else {
    // 二巡目: webfont を要求する。marker は check() 成立まで赤のまま。
    var text = document.createElement('div');
    text.className = 'webfont-text';
    text.textContent = 'Reloaded Hamburgefonstiv いろは';
    root.appendChild(text);
    var marker = document.createElement('div');
    marker.id = 'font-marker';
    root.appendChild(marker);
    (function poll() {
      if (document.fonts.check("40px 'VrtTestFont'")) {
        marker.style.background = '#00cc00';
      } else {
        requestAnimationFrame(poll);
      }
    })();
  }
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'reload-after-fonts'); }, 20);
  }, 20);
  if (!reloaded) {
    // fonts 待ちが確実に済み、freeze evaluate が rAF 待ちで pending に
    // なっている頃合いに一度だけ reload する。
    setTimeout(function () {
      sessionStorage.setItem('vrtReloaded', '1');
      location.reload();
    }, 2500);
  }
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// 【cmd_659・検証と撮影の窓】`storyRendered` の後に自分を reload する
    /// story で、フォント未検証の document を撮らないこと。
    ///
    /// 修正前（d6aa8a3。フォント検証が FREEZE の前にあるだけの形）は、fonts
    /// 待ちが reload **前**の document で ok を返し、FREEZE が navigated or
    /// closed のリトライを経て reload **後**の document で成功し、フォント
    /// 未検証の document がそのまま撮れた（cmd_659 実測: 本番既定で
    /// `render_story` は Ok を返し、marker は赤＝二巡目のフォントは loading
    /// のままだった）。窓は evaluate 二往復ではなく **deadline までの
    /// リトライ全長**——数十秒になりうる。
    ///
    /// positive control は `wait_for_fonts = false` の裏口: 検証も再確認も
    /// 無ければ、この fixture は今も未検証 document を撮ってしまう（marker 赤）
    /// ——fixture の窓が実在し続けることの固定。これが落ちるなら本体側の
    /// 検証は何も検証していない。
    ///
    /// 本番既定では、撮影直前の再確認（[`FONTS_RECHECK_PROBE`]）が document の
    /// 入れ替わりを検知して検証列をやり直し、二巡目のフォントが届かない本
    /// fixture は撮らずに Timeout（fonts 経路）へ倒れる（fail-closed）。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_story_that_reloads_after_the_fonts_wait_is_not_captured_unverified() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_story_that_reloads_after_the_fonts_wait: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_reload_after_fonts_bundle(dir.path());

        // positive control（裏口）: フォント検証・再確認が無ければ、reload 後の
        // 未検証 document がそのまま撮れてしまう——窓の実在の固定。
        // 二巡目のフォントは 1 時間遅延＝この試験の時間内には決して届かない。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font.clone(), Duration::from_secs(3600)).await;
        let mut unfixed = RenderOptions::new(chromium.clone(), 640, 360);
        unfixed.story_timeout = Duration::from_secs(15);
        unfixed.wait_for_fonts = false;
        let renderer = StoryRenderer::launch(unfixed)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "reload-after-fonts")
            .await
            .expect("without the fonts layers the reloaded document silently captures")
            .png;
        renderer.close().await;
        server_task.abort();
        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        let px = image.get_pixel(640 - 10, 10);
        assert_eq!(
            (px[0], px[1], px[2]),
            (0xcc, 0x00, 0x00),
            "the reloaded document's font marker must be red (fonts still pending) — \
             a green marker means the fixture no longer opens the window"
        );

        // 本番既定: 撮らずに fonts 経路の Timeout（fail-closed）。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font, Duration::from_secs(3600)).await;
        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let err = renderer
            .render_story(&format!("http://{addr}"), "reload-after-fonts")
            .await
            .expect_err(
                "a story that reloads after the fonts wait must not capture the \
                 unverified document",
            );
        renderer.close().await;
        server_task.abort();

        match err {
            RenderError::Timeout { phase, .. } => {
                assert!(
                    phase.contains("fonts"),
                    "the failure must be attributed to the fonts stage, got: {phase}"
                );
            }
            other => panic!("expected a fonts-stage Timeout, got: {other:?}"),
        }
    }

    /// 【cmd_659・やり直しの収束】reload する story でも、reload 後の
    /// フォントが届くなら、再確認→検証列のやり直しを経て**検証済みの**
    /// document が撮れること（marker 緑＝webfont 適用済み）。
    ///
    /// 窓を塞いだ結果として「reload する story は恒久的に撮影不能」へ倒れて
    /// いないことの固定——それは失敗経路①(b) で退けたのと同じ害（フォントが
    /// 問題なく届くページの恒久的な誤分類）の再導入になる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reloading_story_whose_fonts_arrive_recovers_and_captures_verified() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_reloading_story_whose_fonts_arrive: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_reload_after_fonts_bundle(dir.path());
        // 二巡目のフォントは初回だけ 600ms 遅れて届く——やり直しの fonts 待ちが
        // 実際に「待つ」ことを保証しつつ、期限内には収束する。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font, Duration::from_millis(600)).await;

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "reload-after-fonts")
            .await
            .expect("a reloading story whose fonts arrive must converge and capture")
            .png;
        renderer.close().await;
        server_task.abort();

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        let px = image.get_pixel(640 - 10, 10);
        assert_eq!(
            (px[0], px[1], px[2]),
            (0x00, 0xcc, 0x00),
            "the captured document's font marker must be green — the redo must \
             re-verify the fonts of the reloaded document before the capture"
        );
    }

    /// `document.open()` で document を**window を保ったまま**差し替えるバンドル。
    /// 検証の印の置き場所（window か document か）の検証用（yupix 指摘・PR #27
    /// 四巡目・失敗経路⑤）。
    ///
    /// ナビゲーション（reload）と違い、`document.open()` / `document.write()` は
    /// グローバルオブジェクトを維持したまま document だけを作り直す——window に
    /// 置いた印は生き残り、`document.fonts` だけが新しい FontFaceSet になる。
    ///
    /// 順序は決定的（タイマー競争ではなく**検証済みの印そのもの**で駆動する）:
    ///
    /// 1. 旧 document はフォントを使わない緑のベタ塗り。story は「検証済みの
    ///    印」（[`FONTS_WAIT_SCRIPT`] が成功時に残すもの——window 側・
    ///    document 側のどちらの置き方でも拾う）を setInterval で監視する
    /// 2. 印が現れた瞬間＝検証成立の直後に `document.open()` で差し替える。
    ///    「検証した document」と「撮影される document」が確実に食い違う
    /// 3. 新 document は右上 40x40 の赤 marker を静的に持ち、**250ms 後に**
    ///    webfont（`/font2.ttf`・配信は初回 600ms 遅延）のテキストを差し込み、
    ///    生き残った channel（window 上に維持される）へ `storyRendered` を
    ///    出し直す——READY のやり直しが「新 document の描画完了」を世代印で
    ///    本当に待つようになったため（cmd_661）、再シグナルの無い差し替えは
    ///    正しく Timeout へ倒れる。この fixture は「再シグナルする真っ当な
    ///    story」の側を固定する。
    ///    marker は `document.fonts.check()` 成立で緑になる——差し替え直後の
    ///    再確認の時点では「読み込み中のフォント無し（status: loaded）」に
    ///    見える、という穴の形をそのまま再現する。フォント適用は最速でも
    ///    差し替え +850ms（挿入 250ms + 配信 600ms）なので、未検証のまま
    ///    撮った絵は screenshot の実行遅延に依らず必ず赤 marker になる
    fn write_document_open_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r##"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  var root = document.getElementById('storybook-root');
  var box = document.createElement('div');
  box.id = 'box';
  root.appendChild(box);
  // FONTS_WAIT_SCRIPT が成功時に残す「検証済みの印」を story 側から監視し、
  // 印が現れた直後（検証成立と撮影の間の窓）に document を差し替える。
  // タイマー競争ではなく印そのもので窓を狙うため決定的。印の置き場所は
  // window 側（修正前）と documentElement.dataset 側（修正後）の両方を拾う。
  var timer = setInterval(function () {
    var marked = window.__vrtFontsVerified === true ||
      (document.documentElement && document.documentElement.dataset &&
       document.documentElement.dataset.vrtFontsVerified === 'true');
    if (!marked) return;
    clearInterval(timer);
    document.open();
    document.write('<!doctype html><html><head><style>' +
      'html,body{margin:0;padding:0;background:#fff}' +
      "@font-face { font-family: 'VrtTestFont2'; src: url('font2.ttf'); }" +
      '#font-marker { position: fixed; top: 0; right: 0; width: 40px; height: 40px; background: #cc0000; }' +
      '</style></head><body><div id="font-marker"></div><script>' +
      'setTimeout(function () {' +
      '  var t = document.createElement("div");' +
      '  t.style.fontFamily = "\'VrtTestFont2\', monospace";' +
      '  t.style.fontSize = "40px";' +
      '  t.textContent = "After swap Hamburgefonstiv";' +
      '  document.body.appendChild(t);' +
      '  if (window.__STORYBOOK_ADDONS_CHANNEL__) {' +
      '    window.__STORYBOOK_ADDONS_CHANNEL__.emit("storyRendered", "doc-open");' +
      '  }' +
      '  (function poll() {' +
      '    if (document.fonts.check("40px \'VrtTestFont2\'")) {' +
      '      document.getElementById("font-marker").style.background = "#00cc00";' +
      '    } else { requestAnimationFrame(poll); }' +
      '  })();' +
      '}, 250);' +
      '<\/script><\/body><\/html>');
    document.close();
  }, 0);
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'doc-open'); }, 20);
  }, 20);
</script>
</body></html>"##,
        )
        .expect("write iframe.html");
    }

    /// reload 後の再描画が**遅い** story のバンドル。やり直しの起点
    /// （READY 待ちと `SETTLE_DELAY` を回るか）の検証用（yupix 指摘・
    /// PR #27 四巡目・失敗経路⑤のやり直し形）。
    ///
    /// [`write_reload_after_fonts_bundle`] と同じ骨格（一巡目は rAF を
    /// 握りつぶして freeze evaluate を pending のまま reload で破壊）だが、
    /// 二巡目がフォントではなく**描画そのもの**で遅い:
    ///
    /// - `slow-rerender` : reload 後、**1.5 秒後に**全画面の赤いボックスを
    ///   描いてから `storyRendered` を出す（reload 後にデータを取得してから
    ///   描画する story の縮約）。フォントは一切使わない——二巡目の
    ///   `document.fonts` は即 `loaded` なので、やり直しが fonts 待ちから
    ///   しか回らない実装では**未描画の白い絵**がそのまま撮れてしまう
    /// - `rerender-error` : reload 後、0.5 秒後に `storyThrewException` を
    ///   出す（描画はしない）。やり直しが READY 待ちを回れば二巡目でも
    ///   エラーとして観測される
    fn write_slow_rerender_reload_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head>
<script>
  // 一巡目だけ rAF を握りつぶす（コールバックは保持——捨てると Promise ごと
  // GC され「Promise was collected」という別経路になる）。
  if (!sessionStorage.getItem('vrtReloaded')) {
    window.__rafCallbacks = [];
    window.requestAnimationFrame = function (cb) {
      window.__rafCallbacks.push(cb);
      return window.__rafCallbacks.length;
    };
  }
</script>
<style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
  #late { width:100%;height:100vh;background:#ff0000; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  var reloaded = !!sessionStorage.getItem('vrtReloaded');
  var root = document.getElementById('storybook-root');
  if (!reloaded) {
    // 一巡目: 緑のベタ塗りを描いてすぐ storyRendered。
    var box = document.createElement('div');
    box.id = 'box';
    root.appendChild(box);
    setTimeout(function () {
      window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
      setTimeout(function () { channel.emit('storyRendered', id); }, 20);
    }, 20);
    // fonts 待ちが済み、freeze evaluate が rAF 待ちで pending になっている
    // 頃合いに一度だけ reload する。
    setTimeout(function () {
      sessionStorage.setItem('vrtReloaded', '1');
      location.reload();
    }, 2500);
  } else {
    // 二巡目: すぐには描かない。
    setTimeout(function () {
      window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
      if (id === 'rerender-error') {
        setTimeout(function () {
          channel.emit('storyThrewException', { message: 'boom after reload' });
        }, 500);
        return;
      }
      // データ取得後に描画する story の縮約: 1.5 秒後に描いてから
      // storyRendered を出す。
      setTimeout(function () {
        var late = document.createElement('div');
        late.id = 'late';
        root.appendChild(late);
        channel.emit('storyRendered', id);
      }, 1500);
    }, 20);
  }
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// 【cmd_660 A・陽性対照つき実測】`document.open()` で document を
    /// 差し替える story で、未検証の document を撮らないこと。
    ///
    /// 修正前（08bcaf4。印が `window.__vrtFontsVerified` にあった形）の実測:
    /// fonts 待ち ok（361ms・印は window へ）→ 差し替え → 再確認が
    /// `verified: true / status: 'loaded'`（印は window ごと生き残り、新
    /// document はまだ何も読んでいない）で素通り → **render_story は Ok を
    /// 返し、marker は赤＝新 document のフォントを待たずに撮っていた**。
    ///
    /// 印を document 側（`documentElement.dataset`）へ移した修正後は、
    /// 差し替えで documentElement ごと印が消えるため再確認が検知し、検証列を
    /// READY 待ちからやり直して新 document のフォント（600ms 遅延配信）を
    /// 待ち切ってから撮る——marker 緑（webfont 適用済み）が「撮られたのは
    /// 検証済みの新 document」であることを絵の中で証言する。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_document_swapped_via_document_open_is_recaptured_verified() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_document_swapped_via_document_open: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_document_open_bundle(dir.path());
        // 新 document のフォント（/font2.ttf）は初回 600ms 遅延——「やり直しの
        // fonts 待ちが実際に待った」ことを保証しつつ、期限内には収束する。
        let (addr, server_task) = start_two_wave_font_server(
            dir.path(),
            font,
            Duration::ZERO,
            Duration::from_millis(600),
        )
        .await;

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "doc-open")
            .await
            .expect("a story that swaps its document must converge and capture")
            .png;
        renderer.close().await;
        server_task.abort();

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        let px = image.get_pixel(640 - 10, 10);
        assert_eq!(
            (px[0], px[1], px[2]),
            (0x00, 0xcc, 0x00),
            "the captured picture must be the swapped document with its font \
             verified (marker green) — red means the recheck missed the \
             document.open() swap and captured the unverified document \
             (the pre-fix behaviour, measured with the marker on `window`)"
        );
    }

    /// 【cmd_660 B・実測】reload 後の再描画が遅い story で、未描画の絵を
    /// 撮らないこと——やり直しが READY 待ちと `SETTLE_DELAY` から回る証拠。
    ///
    /// 修正前（08bcaf4。やり直しが fonts 待ちからしか回らない形）の実測:
    /// 再確認が reload を検知（2.65s・verified: false）→ やり直しは
    /// fonts 待ち（2.76s）→ 再確認（2.80s）だけで READY 待ちを回らず →
    /// **render_story は Ok を返し、中央画素は白＝`storyRendered` 前の
    /// 未描画の document を撮っていた**（二巡目の描画は reload+1.5 秒後）。
    /// yupix 殿の機序どおり——Blink の `FontFaceSet.ready` は「load イベント
    /// 完了＋読み込み中フォント無し」で解決するため、reload 後の document
    /// では story 未描画の時点で fonts 待ちが通ってしまう。既存の reload
    /// fixture（`write_reload_after_fonts_bundle`）は reload 後 40ms で
    /// `storyRendered` を出すためこの経路を踏まなかった。
    ///
    /// 修正後は、やり直しの巡が READY 待ちから回るため、二巡目の
    /// `storyRendered`（描画完了後に出る）を待ってから撮る——中央画素が
    /// 赤（遅れて描かれたボックス）であることが証拠になる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_slowly_rerendering_reload_waits_for_its_second_ready() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_slowly_rerendering_reload: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_slow_rerender_reload_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&server.base_url(), "slow-rerender")
            .await
            .expect("a slowly re-rendering reload must converge and capture")
            .png;
        renderer.close().await;

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        let px = image.get_pixel(320, 180);
        assert_eq!(
            (px[0], px[1], px[2]),
            (0xff, 0x00, 0x00),
            "the captured picture must be the re-rendered document (red box) — \
             white means the redo skipped the READY wait and captured the blank \
             pre-render document (the pre-fix behaviour)"
        );
    }

    /// 【cmd_660 B・実測】reload 後に `storyThrewException` を出す story の
    /// エラーが、やり直しの READY 待ちで**二巡目でも観測される**こと。
    ///
    /// 修正前（08bcaf4）の実測: やり直しが fonts 待ちからしか回らないため
    /// 二巡目の READY hook 状態を誰も読まず、**render_story は Ok を返し、
    /// エラーは黙殺されて空の絵が撮れた**。修正後は READY 待ちが再注入
    /// された hook の `storyThrewException` を読み、[`RenderError::Story`]
    /// で落ちる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_story_error_after_reload_is_observed_by_the_redo() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_story_error_after_reload: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_slow_rerender_reload_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let err = renderer
            .render_story(&server.base_url(), "rerender-error")
            .await
            .expect_err(
                "a story that throws after its reload must fail — capturing \
                 silently means the redo never re-read the READY state",
            );
        renderer.close().await;

        match err {
            RenderError::Story { message, .. } => {
                assert!(
                    message.contains("storyThrewException"),
                    "the failure must carry the second round's error signal, got: {message}"
                );
            }
            other => panic!("expected a Story error from the redo's READY wait, got: {other:?}"),
        }
    }

    /// `document.open()` で差し替えた新 document の**再描画が遅い**バンドル。
    /// READY のやり直しが世代印で新 document を本当に待つかの検証用
    /// （cmd_661・失敗経路⑤の document.open() 形）。
    ///
    /// [`write_slow_rerender_reload_bundle`] の骨格を reload から
    /// `document.open()` へ移したもの。reload と違いグローバルオブジェクトが
    /// 維持されるため、`window.__VRT_READY__.rendered` は前 document の
    /// `true` のまま生き残る——世代印が無ければ、やり直しの READY 待ちは
    /// 新 document を一度も待たずに即 Ok を返す。
    ///
    /// 差し替えは検証済みの印（`documentElement.dataset.vrtFontsVerified`）で
    /// 駆動するため決定的。新 document は生き残った channel へ再シグナルを出す:
    ///
    /// - `doc-open-slow-rerender` : 差し替え後、**1.5 秒後に**全画面の赤い
    ///   ボックスを描いてから `storyRendered` を出す。フォントは使わない——
    ///   新 document の `document.fonts` は即 `loaded` なので、READY を
    ///   素通りする実装では**未描画の白い絵**がそのまま撮れてしまう
    /// - `doc-open-rerender-error` : 差し替え後、0.5 秒後に
    ///   `storyThrewException` を出す（描画はしない）。やり直しが READY 待ちを
    ///   本当に回れば、二巡目でもエラーとして観測される
    fn write_document_open_rerender_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r##"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var id = new URLSearchParams(location.search).get('id') || '';
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  var root = document.getElementById('storybook-root');
  var box = document.createElement('div');
  box.id = 'box';
  root.appendChild(box);
  // 検証済みの印そのもので窓を狙う（タイマー競争ではなく決定的）。
  var timer = setInterval(function () {
    var marked = document.documentElement && document.documentElement.dataset &&
      document.documentElement.dataset.vrtFontsVerified === 'true';
    if (!marked) return;
    clearInterval(timer);
    document.open();
    document.write('<!doctype html><html><head><style>' +
      'html,body{margin:0;padding:0;background:#fff}' +
      '#late { width:100vw;height:100vh;background:#ff0000; }' +
      '</style></head><body><script>' +
      'var id = ' + JSON.stringify(id) + ';' +
      'if (id === "doc-open-rerender-error") {' +
      '  setTimeout(function () {' +
      '    window.__STORYBOOK_ADDONS_CHANNEL__.emit("storyThrewException", { message: "boom after document.open" });' +
      '  }, 500);' +
      '} else {' +
      '  setTimeout(function () {' +
      '    var late = document.createElement("div");' +
      '    late.id = "late";' +
      '    document.body.appendChild(late);' +
      '    window.__STORYBOOK_ADDONS_CHANNEL__.emit("storyRendered", id);' +
      '  }, 1500);' +
      '}' +
      '<\/script><\/body><\/html>');
    document.close();
  }, 0);
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', id); }, 20);
  }, 20);
</script>
</body></html>"##,
        )
        .expect("write iframe.html");
    }

    /// 【cmd_661 ①・陽性対照つき実測】`document.open()` で差し替えた新
    /// document の再描画が遅い story で、未描画の絵を撮らないこと——READY の
    /// 印（`window.__VRT_READY__.rendered`）が window 上で差し替えを生き延びて
    /// も、やり直しの READY 待ちが**新 document の描画完了**を待つ証拠。
    ///
    /// 修正前（efbcf7d。READY の印に世代が無かった形）の実測は本試験の追加
    /// コミットのメッセージと報告に記録: 再確認が差し替えを検知してやり直し
    /// ても、READY 待ちは前 document の `rendered: true` で即 Ok を返し、
    /// **render_story は Ok を返して中央画素は白＝`storyRendered` 前の
    /// 未描画の document を撮っていた**（描画は差し替え +1.5 秒後）。
    ///
    /// 修正後は、印が「どの documentElement の描画完了か」を持ち、現在の
    /// document と一致するまで待つ——中央画素が赤（遅れて描かれたボックス）で
    /// あることが証拠になる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_document_open_swap_with_a_slow_rerender_waits_for_its_second_ready() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_document_open_swap_with_a_slow_rerender: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_document_open_rerender_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&server.base_url(), "doc-open-slow-rerender")
            .await
            .expect("a slowly re-rendering document.open swap must converge and capture")
            .png;
        renderer.close().await;

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        let px = image.get_pixel(320, 180);
        assert_eq!(
            (px[0], px[1], px[2]),
            (0xff, 0x00, 0x00),
            "the captured picture must be the re-rendered document (red box) — \
             white means the redo's READY wait trusted the previous document's \
             rendered flag surviving on window (the pre-fix behaviour)"
        );
    }

    /// 【cmd_661 ①・実測】`document.open()` で差し替えた後に
    /// `storyThrewException` を出す story のエラーが、やり直しの READY 待ちで
    /// **二巡目でも観測される**こと（reload 形の
    /// [`a_story_error_after_reload_is_observed_by_the_redo`] の
    /// `document.open()` 対）。
    ///
    /// 修正前（efbcf7d）は READY の印が生き残るため、やり直しの READY 待ちは
    /// 即 Ok を返してエラーを読む前に通過し、**render_story は Ok を返して
    /// エラーは黙殺された**。修正後は世代の一致まで READY 待ちが続くため、
    /// 差し替え +0.5 秒のエラーを読んで [`RenderError::Story`] で落ちる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_story_error_after_a_document_open_swap_is_observed_by_the_redo() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_story_error_after_a_document_open_swap: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_document_open_rerender_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let err = renderer
            .render_story(&server.base_url(), "doc-open-rerender-error")
            .await
            .expect_err(
                "a story that throws after its document.open swap must fail — \
                 capturing silently means the redo's READY wait never waited for \
                 the new document",
            );
        renderer.close().await;

        match err {
            RenderError::Story { message, .. } => {
                assert!(
                    message.contains("storyThrewException"),
                    "the failure must carry the second round's error signal, got: {message}"
                );
            }
            other => panic!("expected a Story error from the redo's READY wait, got: {other:?}"),
        }
    }

    /// 差し替えの**前後で二度** error を出すバンドル。error の印の世代
    /// （cmd_662 ②）の検証用。
    ///
    /// - 旧 document: 描画して `storyRendered`（READY は普通に通る）。
    ///   検証済みの印（`vrtFontsVerified`）が付いた瞬間に
    ///   `storyThrewException`（一度目）を emit してから `document.open()` で
    ///   自分を差し替える——一度目はこの巡の READY 待ちの**後**なので読まれず、
    ///   hook の state に印だけが残る
    /// - 新 document: +300ms に `storyThrewException`（二度目）を emit し、
    ///   その**直後に** `storyRendered` を emit する
    ///
    /// error → rendered の順で両方 emit してから probe が読むため、修正の
    /// 前後どちらの挙動もポーリングのタイミングに依らず決定的である。
    fn write_double_error_swap_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r##"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  var root = document.getElementById('storybook-root');
  var box = document.createElement('div');
  box.id = 'box';
  root.appendChild(box);
  // 検証済みの印そのもので窓を狙う（タイマー競争ではなく決定的）。
  var timer = setInterval(function () {
    var marked = document.documentElement && document.documentElement.dataset &&
      document.documentElement.dataset.vrtFontsVerified === 'true';
    if (!marked) return;
    clearInterval(timer);
    // 一度目の error は差し替えの前・READY 通過の後——読まれずに印だけ残る。
    channel.emit('storyThrewException', { message: 'first error before the swap' });
    document.open();
    document.write('<!doctype html><html><head><style>' +
      'html,body{margin:0;padding:0;background:#fff}' +
      '#late { width:100vw;height:100vh;background:#ff0000; }' +
      '</style></head><body><div id="storybook-root"><div id="late"></div></div><script>' +
      'setTimeout(function () {' +
      '  window.__STORYBOOK_ADDONS_CHANNEL__.emit("storyThrewException", { message: "second error in the swapped document" });' +
      '  window.__STORYBOOK_ADDONS_CHANNEL__.emit("storyRendered", "double-error");' +
      '}, 300);' +
      '<\/script><\/body><\/html>');
    document.close();
  }, 0);
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'double-error'); }, 20);
  }, 20);
</script>
</body></html>"##,
        )
        .expect("write iframe.html");
    }

    /// 【cmd_662 ②・修正前実測つき】差し替え後の document で出た**二度目**の
    /// error が、前 document の error に塞がれず記録されること。
    ///
    /// rendered の印は書く側（hook）と読む側（probe）の両方が世代を見るが、
    /// error は読む側だけが世代を照合し、書く側の先着固定
    /// （`if (!state.error)`）が世代を見ていなかった——読む側は世代不一致の
    /// 一度目を読まず、二度目は書く側が記録しないため、**新 document の
    /// エラーは誰にも観測されず**、rendered だけが新世代で立って撮影が通る
    /// （fail-open）。
    ///
    /// 修正前（b93969a）の実測は本試験の追加コミットのメッセージと報告に
    /// 記録: `render_story` は **Ok を返して二度目のエラーは黙殺された**。
    /// 修正後は、先着固定が世代の中で閉じる（`recordError`）ため二度目が
    /// 新世代で記録され、probe は rendered より先に error を読むので
    /// [`RenderError::Story`] で落ち、メッセージは二度目のものになる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_error_in_the_swapped_document_is_recorded_not_masked() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_second_error_in_the_swapped_document: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_double_error_swap_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let err = renderer
            .render_story(&server.base_url(), "double-error")
            .await
            .expect_err(
                "a second error in the swapped document must fail the story — \
                 capturing silently means the first document's error blocked \
                 the second from ever being recorded (the pre-fix behaviour)",
            );
        renderer.close().await;

        match err {
            RenderError::Story { message, .. } => {
                assert!(
                    message.contains("second error in the swapped document"),
                    "the failure must carry the swapped document's own error, \
                     not the previous document's, got: {message}"
                );
            }
            other => panic!("expected a Story error from the redo's READY wait, got: {other:?}"),
        }
    }

    /// **同一 document の中で**二度 error を出すバンドル。`recordError` の
    /// 「同一世代内は先着固定」の検証用（cmd_663 ⑨）。
    ///
    /// document の差し替えは行わない——二つの `storyThrewException` は同じ
    /// documentElement の世代で撃たれる。
    fn write_double_error_same_document_bundle(root: &Path) {
        write_story_html(
            root,
            "  html,body{margin:0;padding:0;background:#fff}",
            "",
            r#"      channel.emit('storyThrewException', { message: 'first-error: the root cause' });
      channel.emit('storyThrewException', { message: 'second-error: collateral damage' });"#,
        );
    }

    /// 【cmd_663 ⑨】同一 document 内で二度出た error は**先着が固定**され、
    /// 失敗メッセージは一度目のものになること。
    ///
    /// `recordError` の先着固定は「最初の失敗が根本原因で、続く失敗は
    /// 巻き添えのことが多い」という診断上の判断（cmd_662 ② で世代の中へ
    /// 閉じた半分の、**残り半分**の性質）だが、これまでどの試験も固定して
    /// いなかった——先着固定を消して常時上書きにしても全試験が緑のまま
    /// だった。本試験は upgrade 側（常時上書き＝二度目のメッセージが出る）を
    /// 赤くする: 上書きに変えるとこの assert が 'second-error' を検出して落ちる。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_error_in_the_same_document_keeps_the_first() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_second_error_in_the_same_document: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_double_error_same_document_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let err = renderer
            .render_story(&server.base_url(), "double-error-same-doc")
            .await
            .expect_err("a story that throws twice must fail with the first error");
        renderer.close().await;

        match err {
            RenderError::Story { message, .. } => {
                assert!(
                    message.contains("first-error"),
                    "the failure must carry the first error (the root cause), got: {message}"
                );
                assert!(
                    !message.contains("second-error"),
                    "the first-arrival pin within a generation must hold — seeing \
                     the second error means recordError now always overwrites, \
                     got: {message}"
                );
            }
            other => panic!("expected a Story error, got: {other:?}"),
        }
    }

    /// `window.__STORYBOOK_PREVIEW__.storyRenders` の保険経路**だけ**で ready を
    /// 判定させるバンドル（channel は一切置かない）。差し替え後、前 document の
    /// `phase: 'completed'` が window 上で生き残ることの検証用（cmd_661 ①で
    /// READY の印と同型と特定した第二の生存状態）。
    ///
    /// 新 document は `#storybook-root` を持たず、シグナルも出さない——
    /// storyRenders の完了 phase を現在の document の DOM と突き合わせる修正の
    /// 後は、この差し替えは「撮影可能」と判定できず Timeout（fail-closed）へ
    /// 倒れるのが正しい。
    fn write_stale_storyrenders_swap_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r##"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"></div>
<script>
  var root = document.getElementById('storybook-root');
  var box = document.createElement('div');
  box.id = 'box';
  root.appendChild(box);
  // channel は置かず、プレビュー内部形（storyRenders）だけを再現する。
  window.__STORYBOOK_PREVIEW__ = { storyRenders: [{ phase: 'completed' }] };
  var timer = setInterval(function () {
    var marked = document.documentElement && document.documentElement.dataset &&
      document.documentElement.dataset.vrtFontsVerified === 'true';
    if (!marked) return;
    clearInterval(timer);
    document.open();
    document.write('<!doctype html><html><head><style>' +
      'html,body{margin:0;padding:0;background:#fff}' +
      '#late { width:100vw;height:100vh;background:#ff0000; }' +
      '</style></head><body><script>' +
      'setTimeout(function () {' +
      '  var late = document.createElement("div");' +
      '  late.id = "late";' +
      '  document.body.appendChild(late);' +
      '}, 1500);' +
      '<\/script><\/body><\/html>');
    document.close();
  }, 0);
</script>
</body></html>"##,
        )
        .expect("write iframe.html");
    }

    /// 【cmd_661 ①・実測】前 document の `storyRenders` 完了 phase（window 上で
    /// 差し替えを生き延びる）が、差し替え後の document を ready と誤判定しない
    /// こと。
    ///
    /// 修正前（efbcf7d）の実測は本試験の追加コミットのメッセージと報告に記録:
    /// やり直しの READY 待ちは storyRenders 保険が前 document の
    /// `phase: 'completed'` を返すため即 Ok となり、**render_story は Ok を
    /// 返して描画前（白）の差し替え document を撮っていた**。
    ///
    /// 修正後は、保険経路が「現在の document の root に描画結果がある」ことを
    /// 併せて要求するため、root を持たずシグナルも出さない差し替え document は
    /// 撮影可能と判定されず、共有 deadline の Timeout（fail-closed）へ倒れる。
    /// storyRenders 側には世代を刻む口が無いので、これが到達できる最善である
    /// （root つきの内容へ差し替える形は依然すり抜ける——モジュール doc の
    /// READY probe 行に限界として明記）。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_storyrenders_phase_does_not_ready_the_swapped_document() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_stale_storyrenders_phase: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_stale_storyrenders_swap_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(8);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let err = renderer
            .render_story(&server.base_url(), "stale-renders")
            .await
            .expect_err(
                "a swapped document that neither re-signals nor contains a \
                 populated storybook root must not be judged ready by the \
                 previous document's completed phase",
            );
        renderer.close().await;

        match err {
            RenderError::Timeout { phase, .. } => {
                assert!(
                    phase.contains("render-completion"),
                    "the failure must come from the READY wait, got: {phase}"
                );
            }
            other => panic!("expected a READY-wait Timeout, got: {other:?}"),
        }
    }

    /// Storybook ランタイムを一切持たないバンドルが、検証成立の直後に
    /// `document.open()` で自分を差し替える形。DOM ヒューリスティックの
    /// [`SIGNAL_GRACE`] が**巡ごと**に測られるかの検証用（cmd_661 ③）。
    ///
    /// - 旧 document: 緑のベタ塗り。ランタイム無しなので READY は Absent の
    ///   DOM ヒューリスティック（猶予 [`SIGNAL_GRACE`]）で成立する
    /// - 新 document: `#storybook-root` は空で始まり、**+100ms** に暫定の
    ///   赤いボックス、**+1000ms** に最終の青いボックスへ置き換わる
    ///   （データ取得後に描き直す runtime 無し story の縮約）
    ///
    /// 猶予が story 全体の開始時刻から測られる（巡ごとに更新されない）と、
    /// やり直しの巡では猶予が既に尽きており、root に子が入った瞬間（+100ms・
    /// 暫定の赤）で ready と判定して**途中の絵**を撮る。巡ごとに測れば、
    /// 猶予 1.5 秒の間に最終の青へ達した絵を撮る。
    fn write_absent_runtime_swap_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r##"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  var timer = setInterval(function () {
    var marked = document.documentElement && document.documentElement.dataset &&
      document.documentElement.dataset.vrtFontsVerified === 'true';
    if (!marked) return;
    clearInterval(timer);
    document.open();
    document.write('<!doctype html><html><head><style>' +
      'html,body{margin:0;padding:0;background:#fff}' +
      '#interim { width:100vw;height:100vh;background:#ff0000; }' +
      '#final { width:100vw;height:100vh;background:#0000ff; }' +
      '</style></head><body><div id="storybook-root"></div><script>' +
      'setTimeout(function () {' +
      '  var d = document.createElement("div");' +
      '  d.id = "interim";' +
      '  document.getElementById("storybook-root").appendChild(d);' +
      '}, 100);' +
      'setTimeout(function () {' +
      '  var r = document.getElementById("storybook-root");' +
      '  r.innerHTML = "";' +
      '  var d = document.createElement("div");' +
      '  d.id = "final";' +
      '  r.appendChild(d);' +
      '}, 1000);' +
      '<\/script><\/body><\/html>');
    document.close();
  }, 0);
</script>
</body></html>"##,
        )
        .expect("write iframe.html");
    }

    /// 【cmd_661 ③・実測】やり直しの巡でも DOM ヒューリスティックの
    /// [`SIGNAL_GRACE`] が**その巡の開始から**測り直されること。
    ///
    /// 修正前（efbcf7d。猶予が story 全体の `started` から測られていた形）の
    /// 実測は本試験の追加コミットのメッセージと報告に記録: やり直しの巡では
    /// `started.elapsed() >= SIGNAL_GRACE` が最初から成立しており、root に
    /// 子が入った瞬間（+100ms・暫定の赤いボックス）で ready と判定し、
    /// **render_story は Ok を返して中央画素は赤＝描き直しの途中の絵を撮って
    /// いた**。
    ///
    /// 修正後は、猶予が巡の開始時刻から測り直されるため、+1000ms の最終の
    /// 青いボックスへ達してから撮る。deadline は従来どおり story 全体で
    /// 共有される（この試験も `story_timeout` 内に完走する）。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_redo_round_regrants_the_dom_heuristic_its_signal_grace() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_redo_round_regrants_the_dom_heuristic: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_absent_runtime_swap_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&server.base_url(), "absent-swap")
            .await
            .expect("a runtime-less swapped story must converge and capture")
            .png;
        renderer.close().await;

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        let px = image.get_pixel(320, 180);
        assert_eq!(
            (px[0], px[1], px[2]),
            (0x00, 0x00, 0xff),
            "the captured picture must be the final blue box — red means the \
             redo round inherited an already-elapsed SIGNAL_GRACE and captured \
             the interim render (the pre-fix behaviour)"
        );
    }

    /// [`write_iframe_webfont_bundle`] と [`write_shadow_iframe_webfont_bundle`]
    /// が共有する frame.html——**中だけ**が webfont を使う同一オリジン iframe
    /// の中身。右上 40x40 の marker が `document.fonts.check()` で
    /// 赤（未着）→緑（適用済み）に変わる。
    fn write_webfont_frame_html(root: &Path) {
        std::fs::write(
            root.join("frame.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  @font-face { font-family: 'VrtTestFont'; src: url('font.ttf'); }
  .webfont-text { font-family: 'VrtTestFont', monospace; font-size: 40px; }
  #font-marker { position: fixed; top: 0; right: 0; width: 40px; height: 40px; background: #cc0000; }
</style></head>
<body>
<div id="font-marker"></div>
<script>
  // webfont を使うテキストは frame 自身の load **後**に挿す——load 前に
  // 要求を始めると、進行中のフォント取得が load イベントを遅らせ、親の
  // storyRendered（iframe load 駆動）がフォント到着後になってしまい、
  // 「settle だけでは間に合わない」競争が消える。frame の load ハンドラは
  // 親の iframe load ハンドラより先に走るので、フォント要求の開始は
  // 親の storyRendered より必ず前になる。
  window.addEventListener('load', function () {
    var text = document.createElement('div');
    text.className = 'webfont-text';
    text.textContent = 'Framed Hamburgefonstiv いろは';
    document.body.appendChild(text);
    (function poll() {
      if (document.fonts.check("40px 'VrtTestFont'")) {
        document.getElementById('font-marker').style.background = '#00cc00';
      } else {
        requestAnimationFrame(poll);
      }
    })();
  });
</script>
</body></html>"#,
        )
        .expect("write frame.html");
    }

    /// 同一オリジン iframe の**中だけ**が webfont を使うバンドル。
    /// フォント待ち・再確認の iframe 再帰（cmd_660 D）の検証用。
    ///
    /// top document はフォントを使わない。iframe（`frame.html`）が
    /// `@font-face` の webfont を要求し、右上 40x40 の marker が
    /// `document.fonts.check()` で赤（未着）→緑（適用済み）に変わる。
    /// iframe は viewport いっぱいに重なるので、iframe 内の marker は
    /// スクリーンショット上でも右上に写る——top の `document.fonts` だけを
    /// 見る実装では iframe のフォントを誰も待たず、赤 marker の絵が撮れる。
    fn write_iframe_webfont_bundle(root: &Path) {
        write_webfont_frame_html(root);
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  iframe { position:fixed; inset:0; width:100%; height:100vh; border:0; }"#,
            "",
            r#"      var frame = document.createElement('iframe');
      frame.src = 'frame.html';
      document.getElementById('storybook-root').appendChild(frame);
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
      });"#,
        );
    }

    /// 【cmd_660 D・陽性対照つき実測】同一オリジン iframe の中のフォントも
    /// フォント待ちの対象であること。
    ///
    /// positive control（`wait_for_fonts = false` の裏口）: フォント層が
    /// 無ければ iframe のフォント（600ms 遅延配信）は `SETTLE_DELAY`
    /// （250ms）に間に合わず、marker 赤＝未着のままの絵が撮れてしまう——
    /// fixture が本当に「iframe の中でしかフォントを読まない」ことの固定。
    ///
    /// 本番既定: [`FONTS_WAIT_SCRIPT`] が同一オリジン iframe を再帰して
    /// 各 `contentDocument.fonts` を待つため、marker 緑（適用済み）の絵に
    /// なる。FREEZE が既に同一オリジン iframe を対応範囲としており、
    /// フォント検証はその鏡である（クロスオリジン iframe は
    /// `contentDocument` が null で観測不能——README「届かない範囲」）。
    #[tokio::test(flavor = "multi_thread")]
    async fn fonts_inside_a_same_origin_iframe_are_awaited_before_the_capture() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP fonts_inside_a_same_origin_iframe: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_iframe_webfont_bundle(dir.path());

        let marker_of = |png: &[u8]| {
            let image =
                image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
                    .decode()
                    .expect("decode screenshot")
                    .to_rgba8();
            let px = image.get_pixel(640 - 10, 10);
            (px[0], px[1], px[2])
        };

        // positive control（裏口）: フォント層が無ければ iframe のフォントは
        // 誰も待たず、未着（marker 赤）の絵が撮れてしまう。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font.clone(), Duration::from_millis(600)).await;
        let mut unfixed = RenderOptions::new(chromium.clone(), 640, 360);
        unfixed.story_timeout = Duration::from_secs(15);
        unfixed.wait_for_fonts = false;
        let renderer = StoryRenderer::launch(unfixed)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "iframe-font")
            .await
            .expect("without the fonts layers the iframe capture silently proceeds")
            .png;
        renderer.close().await;
        server_task.abort();
        assert_eq!(
            marker_of(&png),
            (0xcc, 0x00, 0x00),
            "the iframe's font marker must be red without the fonts wait — a \
             green marker means the fixture no longer races the iframe font"
        );

        // 本番既定: iframe の contentDocument.fonts まで待ってから撮る。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font, Duration::from_millis(600)).await;
        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "iframe-font")
            .await
            .expect("the iframe font arrives within the deadline and must capture")
            .png;
        renderer.close().await;
        server_task.abort();
        assert_eq!(
            marker_of(&png),
            (0x00, 0xcc, 0x00),
            "the iframe's font marker must be green — the fonts wait must \
             recurse into reachable same-origin iframes (cmd_660 D)"
        );
    }

    /// open shadow root の**中**の同一オリジン iframe だけが webfont を使う
    /// バンドル。フォント待ち・再確認の走査範囲を FREEZE の `freezeRoot` と
    /// 揃える検証用（cmd_661 ②）。
    ///
    /// [`write_iframe_webfont_bundle`] の iframe を open shadow root の中へ
    /// 移したもの。`querySelectorAll` は shadow 境界を越えない（FREEZE 内の
    /// 注記のとおり）ため、素の `querySelectorAll('iframe')` の走査では
    /// この iframe は誰にも待たれない——静止（FREEZE）だけが shadow へ潜って
    /// 到達し、「静止したが未検証」の document になる。
    fn write_shadow_iframe_webfont_bundle(root: &Path) {
        write_webfont_frame_html(root);
        write_story_html(
            root,
            "  html,body{margin:0;padding:0;background:#fff}",
            "",
            r#"      var host = document.createElement('div');
      document.getElementById('storybook-root').appendChild(host);
      var shadow = host.attachShadow({ mode: 'open' });
      var frame = document.createElement('iframe');
      frame.src = 'frame.html';
      frame.style.position = 'fixed';
      frame.style.top = '0';
      frame.style.left = '0';
      frame.style.width = '100%';
      frame.style.height = '100vh';
      frame.style.border = '0';
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
      });
      shadow.appendChild(frame);"#,
        );
    }

    /// 【cmd_661 ②・陽性対照つき実測】open shadow root の中の同一オリジン
    /// iframe のフォントも、フォント待ち・再確認の対象であること——走査範囲が
    /// FREEZE の `freezeRoot` と同じ（`shadowRoot` へ潜り `localName` で
    /// `iframe` と `frame` を見る）ことの固定。
    ///
    /// positive control（`wait_for_fonts = false` の裏口）: フォント層が
    /// 無ければ shadow 内 iframe のフォント（600ms 遅延配信）は
    /// [`SETTLE_DELAY`]（250ms）に間に合わず、marker 赤＝未着の絵が撮れて
    /// しまう——fixture が本当に「shadow 内 iframe の中でしかフォントを
    /// 読まない」ことの固定。
    ///
    /// 修正前（efbcf7d。走査が素の `querySelectorAll('iframe')` だった形）の
    /// 実測は本試験の追加コミットのメッセージと報告に記録: 本番既定でも
    /// marker 赤の絵が撮れていた——走査が shadow 境界で止まり、shadow 内
    /// iframe のフォントを誰も待っていなかった。
    #[tokio::test(flavor = "multi_thread")]
    async fn fonts_inside_a_shadow_dom_iframe_are_awaited_before_the_capture() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP fonts_inside_a_shadow_dom_iframe: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_iframe_webfont_bundle(dir.path());

        let marker_of = |png: &[u8]| {
            let image =
                image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
                    .decode()
                    .expect("decode screenshot")
                    .to_rgba8();
            let px = image.get_pixel(640 - 10, 10);
            (px[0], px[1], px[2])
        };

        // positive control（裏口）: フォント層が無ければ shadow 内 iframe の
        // フォントは誰も待たず、未着（marker 赤）の絵が撮れてしまう。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font.clone(), Duration::from_millis(600)).await;
        let mut unfixed = RenderOptions::new(chromium.clone(), 640, 360);
        unfixed.story_timeout = Duration::from_secs(15);
        unfixed.wait_for_fonts = false;
        let renderer = StoryRenderer::launch(unfixed)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "shadow-iframe-font")
            .await
            .expect("without the fonts layers the shadow iframe capture silently proceeds")
            .png;
        renderer.close().await;
        server_task.abort();
        assert_eq!(
            marker_of(&png),
            (0xcc, 0x00, 0x00),
            "the shadow iframe's font marker must be red without the fonts wait — \
             a green marker means the fixture no longer races the iframe font"
        );

        // 本番既定: shadow root へ潜って iframe の contentDocument.fonts まで
        // 待ってから撮る。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font, Duration::from_millis(600)).await;
        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "shadow-iframe-font")
            .await
            .expect("the shadow iframe font arrives within the deadline and must capture")
            .png;
        renderer.close().await;
        server_task.abort();
        assert_eq!(
            marker_of(&png),
            (0x00, 0xcc, 0x00),
            "the shadow iframe's font marker must be green — the fonts wait must \
             descend into open shadow roots like FREEZE's freezeRoot (cmd_661 2)"
        );
    }

    /// 同一オリジン iframe の中が **frameset**（`<frame>`）で、その frame の
    /// 中だけが webfont を使うバンドル。走査の `localName === 'frame'` 分岐
    /// （[`COLLECT_DOCUMENTS_JS`]・FREEZE の同型）に実 fixture を与える
    /// 検証用（cmd_662 ⑤——README の「`<iframe>` と `<frame>` の両方を見る」
    /// 宣言と試験の釣り合い）。
    ///
    /// `<frame>` は frameset document の中でしか描画されず、frameset
    /// document は body を持てないため `#storybook-root` を置けない——story の
    /// top document には**なれない**。ゆえに「story → iframe → frameset →
    /// frame → webfont」が、撮影対象の中に `<frame>` が実際に現れる最小の
    /// 形である。
    fn write_frameset_webfont_bundle(root: &Path) {
        write_webfont_frame_html(root);
        std::fs::write(
            root.join("frameset.html"),
            r#"<!doctype html>
<html><frameset cols="100%"><frame src="frame.html"></frameset></html>"#,
        )
        .expect("write frameset.html");
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  iframe { position:fixed; inset:0; width:100%; height:100vh; border:0; }"#,
            "",
            r#"      var frame = document.createElement('iframe');
      frame.src = 'frameset.html';
      document.getElementById('storybook-root').appendChild(frame);
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
      });"#,
        );
    }

    /// 【cmd_662 ⑤・陽性対照つき】`<frame>`（frameset）の中のフォントも
    /// フォント待ちの対象であること——走査が `localName` で `iframe` と
    /// `frame` の両方を見る、という README・doc の宣言に対する実 fixture。
    ///
    /// positive control（`wait_for_fonts = false` の裏口）: フォント層が
    /// 無ければ frame の中のフォント（600ms 遅延配信）は [`SETTLE_DELAY`]
    /// （250ms）に間に合わず、marker 赤＝未着の絵が撮れてしまう——fixture が
    /// 本当に「frameset の frame の中でしかフォントを読まない」ことの固定。
    ///
    /// 本番既定: 走査（[`COLLECT_DOCUMENTS_JS`]）が iframe → frameset
    /// document → `frame` → その `contentDocument` と降りて fonts を待つため、
    /// marker 緑の絵になる。`frame` 分岐を落とす退行はこの試験が赤くする。
    #[tokio::test(flavor = "multi_thread")]
    async fn fonts_inside_a_frameset_frame_are_awaited_before_the_capture() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP fonts_inside_a_frameset_frame: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_frameset_webfont_bundle(dir.path());

        let marker_of = |png: &[u8]| {
            let image =
                image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
                    .decode()
                    .expect("decode screenshot")
                    .to_rgba8();
            let px = image.get_pixel(640 - 10, 10);
            (px[0], px[1], px[2])
        };

        // positive control（裏口）: フォント層が無ければ frame の中のフォントは
        // 誰も待たず、未着（marker 赤）の絵が撮れてしまう。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font.clone(), Duration::from_millis(600)).await;
        let mut unfixed = RenderOptions::new(chromium.clone(), 640, 360);
        unfixed.story_timeout = Duration::from_secs(15);
        unfixed.wait_for_fonts = false;
        let renderer = StoryRenderer::launch(unfixed)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "frameset-font")
            .await
            .expect("without the fonts layers the frameset capture silently proceeds")
            .png;
        renderer.close().await;
        server_task.abort();
        assert_eq!(
            marker_of(&png),
            (0xcc, 0x00, 0x00),
            "the frame's font marker must be red without the fonts wait — a \
             green marker means the fixture no longer races the frame font"
        );

        // 本番既定: frameset の frame の contentDocument.fonts まで待ってから撮る。
        let (addr, server_task, _hits) =
            start_font_delay_server(dir.path(), font, Duration::from_millis(600)).await;
        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&format!("http://{addr}"), "frameset-font")
            .await
            .expect("the frame font arrives within the deadline and must capture")
            .png;
        renderer.close().await;
        server_task.abort();
        assert_eq!(
            marker_of(&png),
            (0x00, 0xcc, 0x00),
            "the frame's font marker must be green — the fonts wait must \
             descend through frameset `frame` elements (cmd_662 5)"
        );
    }

    /// 同一オリジン iframe に**素の XML document**（feed.xml）を読むバンドル。
    /// 「印を刻む口（`dataset`）を持たない document」の扱いの検証用
    /// （cmd_661 追送・yupix レビュー）。
    ///
    /// 素の XML document の documentElement は HTMLOrSVGElement mixin を
    /// 実装せず `dataset` が undefined——印の代入は TypeError で throw する。
    fn write_xml_iframe_bundle(root: &Path) {
        std::fs::write(
            root.join("feed.xml"),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed><title>vrt fixture feed</title></feed>\n",
        )
        .expect("write feed.xml");
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%; height:100vh; background:#00ff00; }
  iframe { position:fixed; top:0; left:0; width:200px; height:100px; border:0; }"#,
            r#"<div id="box"></div>"#,
            r#"      var frame = document.createElement('iframe');
      frame.src = 'feed.xml';
      document.getElementById('storybook-root').appendChild(frame);
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
      });"#,
        );
    }

    /// 【cmd_661 追送・実測】素の XML document を持つ同一オリジン iframe が
    /// フォント待ちを壊さないこと。
    ///
    /// 修正前（efbcf7d）は、検証済みの印を書くループだけが try/catch の外に
    /// あった——`gather()` とフォント列挙は `ok: false` に倒すのに、この代入
    /// だけが then ハンドラ内で裸。XML document の documentElement は
    /// `dataset` を持たず、代入が TypeError で promise ごと reject し、
    /// [`evaluate_with_deadline_retry`] が deadline まで 100ms 間隔で
    /// リトライして、約 `story_timeout` 消費の末に「the fonts-ready wait
    /// evaluate kept failing until the story deadline」という**原因と
    /// 食い違う phase** の Timeout で落ちた（実測は本試験の追加コミットの
    /// メッセージと報告に記録）。
    ///
    /// 修正後は、印を刻む口（`dataset`）が無い document をスキップし
    /// （再確認側も同じ条件で印を要求しない——両側で揃った形）、story は
    /// 普通に撮れる。XML document にはそのぶん「差し替わっても印では検知
    /// できない」という限界が残る（`fonts.status` の検査は行われる。
    /// [`FONTS_RECHECK_PROBE`] の doc に明記）。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_same_origin_xml_iframe_does_not_break_the_fonts_wait() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_same_origin_xml_iframe: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_xml_iframe_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 640, 360);
        options.story_timeout = Duration::from_secs(8);
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch chromium");
        let png = renderer
            .render_story(&server.base_url(), "xml-iframe")
            .await
            .expect(
                "a story embedding a plain-XML same-origin iframe must capture — \
                 a Timeout here means the verified-mark write threw on a \
                 documentElement without dataset (the pre-fix behaviour)",
            )
            .png;
        renderer.close().await;

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        let px = image.get_pixel(320, 300);
        assert_eq!(
            (px[0], px[1], px[2]),
            (0x00, 0xff, 0x00),
            "the story behind the XML iframe must be captured normally"
        );
    }

    /// 同一オリジン iframe のフォントが**届かないまま**、フォント待ちの最中に
    /// iframe が DOM から外されるバンドル。切り離された document の待ちの
    /// 検証用（cmd_663 ②・yupix レビュー）。
    ///
    /// storyRendered（iframe load 駆動）の後 900ms で iframe を remove する。
    /// フォント待ちは [`SETTLE_DELAY`]（250ms）後に始まり、iframe の
    /// `font.ttf` は試験時間内に決して届かない（3600s 遅延配信）ので、待ちの
    /// `Promise.all` は iframe の `fonts.ready` を掴んだまま切り離しを迎える。
    /// frame.html はフォントを使うテキストを自分の load **後**に挿す
    /// （[`write_webfont_frame_html`] の注記どおり）ため、iframe の load——
    /// 親の storyRendered の引き金——はフォントに塞がれない。
    fn write_detachable_iframe_webfont_bundle(root: &Path) {
        write_webfont_frame_html(root);
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%; height:100vh; background:#00ff00; }
  iframe { position:fixed; top:0; left:0; width:300px; height:200px; border:0; }"#,
            r#"<div id="box"></div>"#,
            r#"      var frame = document.createElement('iframe');
      frame.src = 'frame.html';
      document.getElementById('storybook-root').appendChild(frame);
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
        // フォント待ち（SETTLE_DELAY 250ms の後に開始）が iframe の
        // fonts.ready を掴んだ後に外れるよう、900ms 置いてから remove する。
        setTimeout(function () { frame.remove(); }, 900);
      });"#,
        );
    }

    /// 【cmd_663 ②・実測】フォント待ちの最中に切り離された同一オリジン iframe
    /// が story を Timeout させないこと——撮れて、絵（iframe の外れた後の
    /// top document）が決定的であること。
    ///
    /// 修正前（6ca460f）は、`sets` が待ち始めの時点の document 集合で固定され、
    /// 切り離された document の `fonts.ready` は二度と settle しないため
    /// `Promise.all` が永久に pending になり、共有 deadline が尽きて
    /// [`FONTS_PHASES`] の timeout phase で落ちた（実測は本試験の追加
    /// コミットのメッセージと報告に記録。既定の story_timeout では 30 秒）。
    /// FREEZE の `freezeRoot` は `defaultView` の無い document を対象外とする
    /// 門を持つのに、揃えたはずの fonts 走査にはその門が写されていなかった。
    ///
    /// 修正後は、走査（[`COLLECT_DOCUMENTS_JS`]）が browsing context の無い
    /// document を集めず、待ちの最中の切り離しは ready との race が検知して
    /// その document を待ちからも判定からも捨てる——描画されない document の
    /// ために story を落とさない。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_detached_iframe_mid_fonts_wait_does_not_time_out_the_story() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_detached_iframe_mid_fonts_wait: no chromium");
            return;
        };
        let font = require_test_font();
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_detachable_iframe_webfont_bundle(dir.path());

        const RUNS: usize = 2;
        let mut hashes: Vec<String> = Vec::new();
        for i in 0..RUNS {
            // 「初回だけ遅い」を run ごとに効かせるため、render ごとに独立の
            // サーバーを立てる（`a_font_that_never_arrives_*` と同じ理由）。
            let (addr, server_task, _hits) =
                start_font_delay_server(dir.path(), font.clone(), Duration::from_secs(3600)).await;
            let mut options = RenderOptions::new(chromium.clone(), 640, 360);
            options.story_timeout = Duration::from_secs(12);
            let renderer = StoryRenderer::launch(options)
                .await
                .expect("launch chromium");
            let png = renderer
                .render_story(&format!("http://{addr}"), "detached-iframe-font")
                .await
                .expect(
                    "a story whose iframe is detached mid fonts-wait must capture — \
                     a Timeout here means the wait kept holding the detached \
                     document's never-settling fonts.ready (the pre-fix behaviour)",
                )
                .png;
            renderer.close().await;
            server_task.abort();

            let image = image::ImageReader::with_format(
                std::io::Cursor::new(&png),
                image::ImageFormat::Png,
            )
            .decode()
            .expect("decode screenshot")
            .to_rgba8();
            // iframe は撮影前に外れているので、絵は top document の緑一色。
            // iframe が覆っていた左上も緑であることを確かめる——赤 marker が
            // 見えるなら「外れたはずの iframe が写った」不定の絵である。
            for (x, y) in [(320u32, 300u32), (150u32, 100u32)] {
                let px = image.get_pixel(x, y);
                assert_eq!(
                    (px[0], px[1], px[2]),
                    (0x00, 0xff, 0x00),
                    "run {i}: the capture must show only the top document after \
                     the iframe was detached (pixel at {x},{y})"
                );
            }
            hashes.push(crate::screenshots::content_hash(&png));
        }
        assert!(
            hashes.iter().all(|h| h == &hashes[0]),
            "captures after a mid-wait iframe detach must be deterministic, got: {hashes:?}"
        );
    }

    /// `document.fonts` を **stateful getter** の偽物へ差し替えるバンドル。
    /// 「形チェック（`gather`）は通るが、settle の**二度目以降の読み**で
    /// throw する」getter の検証用（cmd_663 ③・yupix レビュー）。
    ///
    /// 読みの回数は現行実装の読み順に合わせてある（変わると陽性対照の
    /// 意味が変わるので注記）:
    ///
    /// - `throwing-fonts--ready` : `ready` は 1 evaluate につき gather の
    ///   形チェックで 2 回（truthiness と `.then` の typeof）、settle の
    ///   `Promise.resolve(s.fonts.ready)` で 3 回目が読まれる——3 の倍数の
    ///   読みだけ throw することで、gather は毎回通り settle 側だけが落ちる
    /// - `throwing-fonts--status` : `status` は gather の typeof 検査で 1 回、
    ///   settle の `'loaded'` 比較で 2 回目が読まれる——偶数回の読みだけ
    ///   throw する
    ///
    /// evaluate はリトライごとにスクリプトを**再実行**するため、単純な
    /// 「N 回目以降ずっと throw」だと 2 回目の evaluate では gather の
    /// 形チェック自体が throw して（既存の try に拾われ）即失敗になり、
    /// 「裸の読みがリトライへ化けて deadline まで落ちない」という修正前の
    /// 症状が再現できない。周期で throw させることで、どの evaluate でも
    /// gather は通り settle の二度目の読みだけが落ち続ける。
    fn write_stateful_fonts_getter_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%; height:100vh; background:#00ff00; }"#,
            r#"<div id="box"></div>"#,
            r#"      var readyReads = 0;
      var statusReads = 0;
      var realReady = Promise.resolve();
      var fake;
      if (id === 'throwing-fonts--ready') {
        fake = {
          get status() { return 'loaded'; },
          get ready() {
            readyReads++;
            if (readyReads % 3 === 0) {
              throw new Error('stateful fonts.ready getter threw on a later read');
            }
            return realReady;
          }
        };
      } else {
        fake = {
          get status() {
            statusReads++;
            if (statusReads % 2 === 0) {
              throw new Error('stateful fonts.status getter threw on a later read');
            }
            return 'loaded';
          },
          ready: realReady
        };
      }
      Object.defineProperty(document, 'fonts', {
        configurable: true,
        get: function () { return fake; }
      });
      channel.emit('storyRendered', id);"#,
        );
    }

    /// 【cmd_663 ③・実測】`fonts.ready` / `fonts.status` の**二度目の読み**で
    /// throw する stateful getter が、原因つきの即時 [`RenderError::Story`] に
    /// 倒れること——deadline まで待った原因不明の Timeout にならないこと。
    ///
    /// 修正前（6ca460f）は、settle の `sets.map((s) => ...s.fonts.ready...)` と
    /// `sets.some((s) => s.fonts.status !== 'loaded')` だけが try の外にあった
    /// ——印の書き込み（1015 行）には「ここだけ裸だと、promise の reject が
    /// evaluate のリトライへ化けて、原因と食い違う phase の Timeout になる」と
    /// 正しく書いて囲ってあるのに、同じ露出の読みが二つ残っていた。throw は
    /// evaluate の例外→ [`evaluate_with_deadline_retry`] のリトライへ化け、
    /// story_timeout を丸ごと消費して [`FONTS_PHASES`] の retry_exhausted
    /// phase で落ちた（実測は本試験の追加コミットのメッセージと報告に記録。
    /// 既定の story_timeout では 30 秒）。
    ///
    /// 修正後は他の読み（gather・列挙・印）と同じく try → `ok:false` +
    /// `errors` に倒れ、即座に理由（getter の throw 文言）つきで失敗する。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stateful_fonts_getter_fails_fast_with_the_reason() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_stateful_fonts_getter_fails_fast: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_stateful_fonts_getter_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        for (story_id, getter) in [
            ("throwing-fonts--ready", "fonts.ready"),
            ("throwing-fonts--status", "fonts.status"),
        ] {
            let mut options = RenderOptions::new(chromium.clone(), 640, 360);
            // 「即座に」を時間で検証する: story_timeout（20s）を丸ごと待つ
            // 修正前の挙動なら下の elapsed assert が落ちる。
            options.story_timeout = Duration::from_secs(20);
            let renderer = StoryRenderer::launch(options)
                .await
                .expect("launch chromium");
            let started = std::time::Instant::now();
            let err = renderer
                .render_story(&server.base_url(), story_id)
                .await
                .expect_err("a throwing stateful fonts getter must fail the story");
            let elapsed = started.elapsed();
            renderer.close().await;

            match err {
                RenderError::Story { message, .. } => {
                    assert!(
                        message.contains("stateful") && message.contains("getter threw"),
                        "{story_id}: the failure must carry the getter's own \
                         throw message (the reason), got: {message:?}"
                    );
                }
                other => panic!(
                    "{story_id}: expected an immediate reasoned Story failure for \
                     a throwing {getter} getter, got: {other:?}"
                ),
            }
            assert!(
                elapsed < Duration::from_secs(10),
                "{story_id}: the failure must be immediate, not a deadline \
                 exhaustion — took {elapsed:?} (pre-fix this burned the whole \
                 story_timeout as evaluate retries)"
            );
        }
    }

    /// open shadow root の内側にアニメ源を持つバンドル。root 単位の静止検証用。
    ///
    /// - `demo-shadow--caret` : shadow 内のスタイルが `caret-color` を**明示**した
    ///   フォーカス済み入力欄。document からの継承（transparent）は明示宣言に
    ///   負けるので、shadow root への直接注入がなければキャレットは明滅し続ける
    /// - `demo-shadow--caret-hidden` : 同じ入力欄だが shadow 側の宣言が
    ///   `caret-color: transparent`。「注入がキャレットを本当に消した」ことを
    ///   絵の一致で証明するための対照
    /// - `demo-shadow--transition` : shadow 内の要素が自分の computed
    ///   `transition-duration` を毎フレーム色に変換して表示する（`0s` なら緑、
    ///   それ以外は赤）。`transition-duration` は**非継承**なので、これも
    ///   root への直接注入がなければ `60s` のまま（赤）になる
    /// - `demo-shadow--frame` : shadow 内の同一オリジン iframe の中の
    ///   フォーカス済み入力欄。`querySelectorAll('iframe')` は shadow 境界を
    ///   越えないので、root 単位の再帰探索がなければ届かない
    fn write_shadow_animated_bundle(root: &Path) {
        std::fs::write(
            root.join("frame.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  input { font:24px monospace;width:200px;margin:20px;border:1px solid #000; }
</style></head>
<body><input>
<script>
  document.querySelector('input').focus();
</script>
</body></html>"#,
        )
        .expect("write frame.html");
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  x-caret, x-trans, x-frame { display:block; }"#,
            "",
            r#"      var root = document.getElementById('storybook-root');
      if (id === 'demo-shadow--caret' || id === 'demo-shadow--caret-hidden') {
        var host = document.createElement('x-caret');
        var sr = host.attachShadow({ mode: 'open' });
        var color = id === 'demo-shadow--caret' ? '#cc0000' : 'transparent';
        sr.innerHTML =
          '<style>input { caret-color:' + color + '; font:32px monospace; width:200px; margin:40px; border:1px solid #000; }</style>' +
          '<input>';
        root.appendChild(host);
        sr.querySelector('input').focus();
        channel.emit('storyRendered', id);
      } else if (id === 'demo-shadow--transition') {
        var host = document.createElement('x-trans');
        var sr = host.attachShadow({ mode: 'open' });
        sr.innerHTML =
          '<style>div { width:100vw; height:100vh; transition: opacity 60s linear; }</style>' +
          '<div></div>';
        root.appendChild(host);
        var el = sr.querySelector('div');
        (function poll() {
          var d = getComputedStyle(el).transitionDuration;
          el.style.background = d === '0s' ? '#00ff00' : '#ff0000';
          requestAnimationFrame(poll);
        })();
        channel.emit('storyRendered', id);
      } else if (id === 'demo-shadow--frame') {
        var host = document.createElement('x-frame');
        var sr = host.attachShadow({ mode: 'open' });
        sr.innerHTML =
          '<style>iframe { width:280px; height:160px; margin:20px; border:1px solid #000; }</style>' +
          '<iframe src="frame.html"></iframe>';
        root.appendChild(host);
        var frame = sr.querySelector('iframe');
        frame.addEventListener('load', function () {
          try { frame.contentDocument.querySelector('input').focus(); } catch (e) {}
          channel.emit('storyRendered', id);
        });
      }"#,
        );
    }

    /// transition イベントの発火有無を数えるバンドル。freeze のイベント境界検証用。
    ///
    /// - `#fast` : `background-color 0.1s` の transition。freeze なしの
    ///   positive control（イベントが実際に届くこと）と、freeze 下で
    ///   **これから起こす** transition の不発火検証に使う
    /// - `#slow` : `background-color 60s` の transition。freeze が走る時点で
    ///   **すでに走っている** transition が、終端へシークされて完了扱いとなり
    ///   `transitionend` を発火することの検証に使う
    ///
    /// 4 種の transition イベントを capture 段階で window に数え、
    /// `JSON.stringify` で取り出す。
    fn write_transition_event_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #fast { width:100px;height:100px;background:#ff0000;
          transition: background-color 0.1s linear; }
  #slow { width:100px;height:100px;background:#ff0000;
          transition: background-color 60s linear; }
</style></head>
<body><div id="fast"></div><div id="slow"></div>
<script>
  window.__TRANSITION_EVENTS__ =
    { transitionrun: 0, transitionstart: 0, transitionend: 0, transitioncancel: 0 };
  ['transitionrun', 'transitionstart', 'transitionend', 'transitioncancel']
    .forEach(function (name) {
      window.addEventListener(name, function () {
        window.__TRANSITION_EVENTS__[name]++;
      }, true);
    });
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    #[test]
    fn readiness_parses_probe_results() {
        let probe = |raw: &str| Readiness::parse(Some(&serde_json::Value::String(raw.to_string())));

        assert_eq!(probe(r#"{"state":"ready"}"#), Readiness::Ready);
        assert_eq!(
            probe(r#"{"state":"error","message":"storyErrored: boom"}"#),
            Readiness::Error("storyErrored: boom".to_string())
        );
        assert_eq!(
            probe(r#"{"state":"absent","dom_ready":true}"#),
            Readiness::Absent { dom_ready: true }
        );
        assert_eq!(
            probe(r#"{"state":"pending","dom_ready":false}"#),
            Readiness::Pending
        );
        // 壊れた値は「まだ待つ」。誤って白紙を撮らない。
        assert_eq!(probe("not json"), Readiness::Pending);
        assert_eq!(Readiness::parse(None), Readiness::Pending);
    }

    /// `ok === true` と確かめられたときだけ成功。`ok` 以外のキーが
    /// 増えても正当な応答を弾かないこと（受理を狭めすぎない）。
    ///
    /// 証明する: `freeze_verdict` の受理条件のみ。証明しない: 実ブラウザで
    /// FREEZE_SCRIPT がこの形の JSON を返すこと（実ブラウザ系テストが担う）。
    #[test]
    fn freeze_verdict_accepts_only_a_verified_ok_true() {
        let json = |raw: &str| serde_json::Value::String(raw.to_string());

        assert!(freeze_verdict(Some(&json(r#"{"ok":true}"#)), "s").is_ok());
        // 将来 FREEZE_SCRIPT の戻り値を拡張しても ok:true なら通る。
        assert!(
            freeze_verdict(
                Some(&json(
                    r#"{"ok":true,"sweeps":3,"running":[],"errors":[],"extra":1}"#
                )),
                "s"
            )
            .is_ok()
        );
    }

    /// `ok: false` は「静止に失敗した」。running の中身と sweep 数まで
    /// メッセージに出て、利用者が原因へ辿り着ける。
    ///
    /// 証明する: `freeze_verdict` の失敗メッセージ整形のみ（実ブラウザ不使用）。
    #[test]
    fn freeze_verdict_reports_remaining_animations_as_a_freeze_failure() {
        let raw = serde_json::Value::String(
            r#"{"ok":false,"sweeps":10,"running":["blink-a","blink-b"]}"#.to_string(),
        );
        let message = freeze_verdict(Some(&raw), "s")
            .expect_err("ok:false must fail")
            .to_string();
        assert!(
            message.contains("freeze failed")
                && message.contains("2 animation(s) still running")
                && message.contains("10 sweep(s)")
                && message.contains("blink-a, blink-b"),
            "freeze failure must name the surviving animations, got {message:?}"
        );
        // 「解析できなかった」とは違う失敗として区別されること。
        assert!(!message.contains("unparseable"));
    }

    /// `ok: false` + `reason` は CSS 適用失敗。`running` ではなく `reason` が
    /// メッセージに出ること。
    ///
    /// 証明する: `freeze_verdict` の失敗種別の区別のみ（実ブラウザ不使用）。
    #[test]
    fn freeze_verdict_reports_css_verification_failure() {
        let raw = serde_json::Value::String(
            r#"{"ok":false,"sweeps":2,"reason":"CSS not applied: caret-color=rgb(204, 0, 0) transition-duration=60s"}"#
                .to_string(),
        );
        let message = freeze_verdict(Some(&raw), "s")
            .expect_err("ok:false with reason must fail")
            .to_string();
        assert!(
            message.contains("freeze failed") && message.contains("CSS not applied"),
            "CSS verification failure must include the reason, got {message:?}"
        );
        assert!(
            !message.contains("still running"),
            "a CSS failure must not be reported as an animation failure"
        );
    }

    /// **positive control**: 解析できない応答——文字列でない・JSON でない・
    /// `ok` が無い・bool でない——はすべて「静止結果を解析できなかった」
    /// 失敗になる。修正前のコード（`ok == Some(false)` のときだけ失敗）は
    /// これら全部を暗黙に成功として撮影へ通していた。
    ///
    /// 証明する: `freeze_verdict` が解析不能応答を拒むこと（実ブラウザ不使用。
    /// 実経路での同じ性質は `garbled_freeze_result_fails_instead_of_silently_succeeding` が担う）。
    #[test]
    fn freeze_verdict_rejects_a_response_it_cannot_interpret() {
        let json = |raw: &str| serde_json::Value::String(raw.to_string());
        let cases: Vec<(&str, Option<serde_json::Value>)> = vec![
            ("evaluate returned no value", None),
            ("non-string value", Some(serde_json::json!(42))),
            ("non-JSON string", Some(json("not json"))),
            ("missing ok key", Some(json(r#"{"sweeps":2,"running":[]}"#))),
            ("non-boolean ok", Some(json(r#"{"ok":"true"}"#))),
            ("null ok", Some(json(r#"{"ok":null}"#))),
        ];
        for (label, value) in &cases {
            let err = freeze_verdict(value.as_ref(), "s").expect_err(&format!(
                "{label}: an uninterpretable freeze result must fail"
            ));
            let message = err.to_string();
            assert!(
                message.contains("freeze result was unparseable"),
                "{label}: the error must say the result could not be parsed, got {message:?}"
            );
            // 「静止に失敗した」とは違う失敗として区別されること。
            assert!(
                !message.contains("freeze failed"),
                "{label}: a parse failure must not be reported as a freeze failure"
            );
        }
    }

    /// cmd_632 で実測した CDP エラー（-32000。navigation / reload が pending
    /// evaluate の実行コンテキストを壊したときの文言）を模す。
    fn cdp_context_destroyed() -> chromiumoxide::error::CdpError {
        chromiumoxide::error::CdpError::ChromeMessage(
            "Inspected target navigated or closed".to_string(),
        )
    }

    /// **機構を一本化しても分類は経路ごとに別のまま**であること。
    ///
    /// [`evaluate_with_deadline_retry`] が共有するのは待ち・リトライ・期限
    /// だけで、`phase` はログから「どの段で落ちたか」を復元するための診断
    /// （README の failure paths 表への入口）である。二経路が同じ文字列を
    /// 返した時点でその復元ができなくなる——抽出の副作用として最も起きやすい
    /// 事故がこれなので、定数の段階で固定しておく。
    ///
    /// 証明する: 二経路の文字列が一つも重ならず、経路名と段が文言から読めること。
    /// 証明しない: 呼び出し側がそれぞれ正しい定数を渡していること（実ブラウザ系の
    /// `*_fails_story_scoped` と `*_never_returns_a_verdict` が担う）。
    #[test]
    fn phase_strings_stay_distinct_per_path() {
        let reduced = [
            REDUCED_MOTION_PHASES.retry_log,
            REDUCED_MOTION_PHASES.timeout,
            REDUCED_MOTION_PHASES.retry_exhausted,
        ];
        let freeze = [
            FREEZE_PHASES.retry_log,
            FREEZE_PHASES.timeout,
            FREEZE_PHASES.retry_exhausted,
        ];
        let fonts = [
            FONTS_PHASES.retry_log,
            FONTS_PHASES.timeout,
            FONTS_PHASES.retry_exhausted,
        ];
        let recheck = [
            FONTS_RECHECK_PHASES.retry_log,
            FONTS_RECHECK_PHASES.timeout,
            FONTS_RECHECK_PHASES.retry_exhausted,
        ];
        let paths = [reduced, freeze, fonts, recheck];
        for (i, a) in paths.iter().enumerate() {
            for b in paths.iter().skip(i + 1) {
                for s_a in a {
                    for s_b in b {
                        assert_ne!(
                            s_a, s_b,
                            "two paths must not share a diagnostic string — a shared \
                             phase makes the failing stage unrecoverable from the logs"
                        );
                    }
                }
            }
        }

        // 経路の中でも「返らなかった」と「リトライが尽きた」は別の段である。
        assert_ne!(
            REDUCED_MOTION_PHASES.timeout, REDUCED_MOTION_PHASES.retry_exhausted,
            "a timeout and an exhausted retry are different stages"
        );
        assert_ne!(
            FREEZE_PHASES.timeout, FREEZE_PHASES.retry_exhausted,
            "a timeout and an exhausted retry are different stages"
        );
        assert_ne!(
            FONTS_PHASES.timeout, FONTS_PHASES.retry_exhausted,
            "a timeout and an exhausted retry are different stages"
        );

        // どの経路の失敗かが文言そのものから読めること。
        for phase in reduced {
            assert!(
                phase.contains("reduced-motion"),
                "the reduced-motion path must name itself, got {phase:?}"
            );
        }
        for phase in freeze {
            assert!(
                phase.contains("freeze"),
                "the freeze path must name itself, got {phase:?}"
            );
        }
        for phase in fonts {
            assert!(
                phase.contains("fonts"),
                "the fonts path must name itself, got {phase:?}"
            );
        }
        for phase in recheck {
            assert!(
                phase.contains("fonts recheck"),
                "the fonts recheck path must name itself, got {phase:?}"
            );
        }
        // ループ側の deadline 到達も、evaluate 系のどの段とも別の文字列で
        // 名乗ること（fonts の語は含む——ログの grep で経路に集まるため）。
        for other in reduced.iter().chain(&freeze).chain(&fonts).chain(&recheck) {
            assert_ne!(FONTS_RECHECK_EXHAUSTED_PHASE, *other);
        }
        assert!(FONTS_RECHECK_EXHAUSTED_PHASE.contains("fonts"));
    }

    /// [`fonts_verdict`] は `ok` が `true` と確かめられた場合だけ撮影へ通す
    /// （fail-closed）。
    ///
    /// 証明する: `ok: false` は診断つきの [`RenderError::Story`] に、解析できない
    /// 応答（文字列でない・壊れた JSON・`ok` 欠落・応答なし）は unparseable の
    /// [`RenderError::Story`] に倒れ、どれも黙って撮影へ進まないこと。
    /// 証明しない: 実ブラウザで [`FONTS_WAIT_SCRIPT`] がこれらの形を実際に
    /// 返すこと（実ブラウザ系の試験が担う）。
    #[test]
    fn fonts_verdict_accepts_only_a_verified_ok_true() {
        let ok = serde_json::json!(r#"{"ok":true,"status":"loaded","errors":[]}"#);
        let warning = fonts_verdict(Some(&ok), "s").expect("a verified ok:true must pass");
        assert!(
            warning.is_none(),
            "no failed fonts means no warning, got {warning:?}"
        );

        let not_ok = serde_json::json!(
            r#"{"ok":false,"errors":["document.fonts is missing or does not look like a FontFaceSet (status: undefined)"]}"#
        );
        match fonts_verdict(Some(&not_ok), "s").expect_err("ok:false must fail") {
            RenderError::Story { message, .. } => {
                assert!(
                    message.contains("not verified as loaded"),
                    "the failure must say what could not be verified, got: {message}"
                );
                assert!(
                    message.contains("document.fonts is missing"),
                    "the collected diagnostics must reach the message, got: {message}"
                );
            }
            other => panic!("expected a story-scoped failure, got: {other:?}"),
        }
    }

    /// 【cmd_659・失敗経路①(b)】読み込みに失敗したフォントは撮影を止めず、
    /// **有界の**警告になる。
    ///
    /// 証明する: `failed` の family が警告に載り、件数は一意化後の集合の
    /// 大きさで数えられ、[`FONT_WARNING_MAX_FAMILIES`] 件で打ち切られて
    /// 超過は「and N more」に畳まれること（`unicode-range` 分割の 1 family
    /// ×20 重複が warning を専有しない——一意化はスクリプト側 Set の責務で、
    /// ここでは「渡された一覧をそのまま数え、上限で切る」ことを固定する）。
    /// 証明しない: 実ブラウザで Set の一意化が効くこと（実ブラウザ系の
    /// `a_failing_font_load_captures_with_a_warning_and_stays_deterministic`
    /// が fixture の family 名到達まで担う）。
    #[test]
    fn fonts_verdict_turns_failed_fonts_into_a_bounded_warning() {
        let json = |raw: &str| serde_json::Value::String(raw.to_string());

        // 1 件: 名前と対処が載り、「and N more」は出ない。
        let warning = fonts_verdict(
            Some(&json(
                r#"{"ok":true,"status":"loaded","failed":["Roboto"],"errors":[]}"#,
            )),
            "s",
        )
        .expect("ok:true with failures must still pass")
        .expect("failed fonts must produce a warning");
        assert!(
            warning.contains("1 font(s) failed to load")
                && warning.contains("captured with fallback glyphs")
                && warning.contains("Roboto")
                && warning.contains("different pictures")
                && warning.contains("bundle the font files with the build"),
            "the warning must count, name the family, warn that varying external \
             responses can yield different pictures, and name the remedy, got: {warning}"
        );
        assert!(
            !warning.contains("more"),
            "one family must not be folded, got: {warning}"
        );

        // 上限超過: 先頭 FONT_WARNING_MAX_FAMILIES 件＋「and N more」で有界。
        let families: Vec<String> = (0..8).map(|i| format!("Family{i}")).collect();
        let raw = format!(
            r#"{{"ok":true,"status":"loaded","failed":{},"errors":[]}}"#,
            serde_json::to_string(&families).unwrap()
        );
        let warning = fonts_verdict(Some(&json(&raw)), "s")
            .expect("ok:true with failures must still pass")
            .expect("failed fonts must produce a warning");
        assert!(
            warning.contains("8 font(s) failed to load") && warning.contains("and 3 more"),
            "the count must be the set size and the overflow must fold, got: {warning}"
        );
        assert!(
            warning.contains("Family4") && !warning.contains("Family5"),
            "only the first {FONT_WARNING_MAX_FAMILIES} families may be listed, got: {warning}"
        );

        // `failed` 欠落は旧形の応答——警告なしの成功として受ける。
        assert!(
            fonts_verdict(
                Some(&json(r#"{"ok":true,"status":"loaded","errors":[]}"#)),
                "s"
            )
            .expect("ok:true without a failed key must pass")
            .is_none()
        );
    }

    /// [`fonts_recheck_verdict`] は「印あり・status loaded」の時だけ撮影を許す。
    ///
    /// 証明する: 印なし（document 入れ替わり）・status が `'loading'`（新しい
    /// 波）・status null（fonts 破壊）はやり直し（`Ok(false)`）に、解析できない
    /// 応答は [`RenderError::Story`]（fail-closed）に倒れること。
    /// 証明しない: 実ブラウザで [`FONTS_RECHECK_PROBE`] がこれらの形を返す
    /// こと（実ブラウザ系の reload 試験群が担う）。
    #[test]
    fn fonts_recheck_verdict_allows_capture_only_when_still_verified() {
        let json = |raw: &str| serde_json::Value::String(raw.to_string());

        assert!(
            fonts_recheck_verdict(Some(&json(r#"{"verified":true,"status":"loaded"}"#)), "s")
                .expect("a verified loaded document must pass")
        );
        // document が入れ替わった（印なし）——やり直し。
        assert!(
            !fonts_recheck_verdict(Some(&json(r#"{"verified":false,"status":"loaded"}"#)), "s")
                .expect("an unverified document must ask for a redo, not error")
        );
        // 新しい読み込み波——やり直し。
        assert!(
            !fonts_recheck_verdict(Some(&json(r#"{"verified":true,"status":"loading"}"#)), "s")
                .expect("a new font wave must ask for a redo, not error")
        );
        // fonts が壊れて status を読めない——やり直し（確定判定は次巡の
        // FONTS_WAIT_SCRIPT の形チェックが下す）。
        assert!(
            !fonts_recheck_verdict(Some(&json(r#"{"verified":true,"status":null}"#)), "s")
                .expect("an unreadable status must ask for a redo, not error")
        );
        // 解析できない応答は fail-closed。
        for garbled in [
            serde_json::json!(42),
            serde_json::Value::String("not json".into()),
            serde_json::Value::String(r#"{"status":"loaded"}"#.into()),
        ] {
            match fonts_recheck_verdict(Some(&garbled), "s")
                .expect_err("garbled recheck results must fail")
            {
                RenderError::Story { message, .. } => assert!(
                    message.contains("fonts recheck result was unparseable"),
                    "the failure must say the recheck was unparseable, got: {message}"
                ),
                other => panic!("expected a story-scoped failure, got: {other:?}"),
            }
        }
    }

    /// [`fonts_verdict`] は解析できない応答を成功へ倒さない（fail-closed）。
    #[test]
    fn fonts_verdict_rejects_unparseable_results() {
        let garbled = [
            serde_json::json!(42),
            serde_json::json!("this is not json"),
            serde_json::json!(r#"{"status":"loaded"}"#),
            serde_json::json!(r#"{"ok":"yes"}"#),
        ];
        for value in &garbled {
            match fonts_verdict(Some(value), "s").expect_err("garbled results must fail") {
                RenderError::Story { message, .. } => assert!(
                    message.contains("unparseable"),
                    "the failure must say the result was unparseable, got: {message}"
                ),
                other => panic!("expected a story-scoped failure, got: {other:?}"),
            }
        }
        match fonts_verdict(None, "s").expect_err("a missing result must fail") {
            RenderError::Story { message, .. } => assert!(message.contains("unparseable")),
            other => panic!("expected a story-scoped failure, got: {other:?}"),
        }
    }

    /// **リトライ切れ**が、経路自身の `retry_exhausted` を積んだ
    /// [`RenderError::Timeout`] へ倒れること（両経路）。
    ///
    /// 期待値は定数を引かずに文字列リテラルで書く——定数を引くと、二経路の
    /// 分類が入れ替わっても等式が保たれてしまい、この試験が何も検知しなく
    /// なるためである。
    ///
    /// 証明する: 消えない CDP エラーが（即 [`RenderError::Cdp`] ではなく）
    /// リトライされ、deadline で経路ごとの Timeout に倒れること。
    /// 証明しない: 実ブラウザでその CDP エラーが実際に起きること
    /// （`reloading_page_during_freeze_fails_story_scoped` 他が担う）。
    #[tokio::test]
    async fn evaluate_retry_exhaustion_falls_to_the_paths_own_phase() {
        let cases: [(EvaluatePhases, &str); 2] = [
            (
                REDUCED_MOTION_PHASES,
                "the reduced-motion verification evaluate kept failing until the story deadline",
            ),
            (
                FREEZE_PHASES,
                "the freeze evaluate kept failing until the story deadline (the page may be \
                 navigating or reloading, or its pending callbacks were collected)",
            ),
        ];

        for (phases, expected) in cases {
            let attempts = std::cell::Cell::new(0usize);
            let deadline = std::time::Instant::now() + Duration::from_millis(250);
            let err = evaluate_with_deadline_retry::<(), _, _>(
                || {
                    attempts.set(attempts.get() + 1);
                    std::future::ready(Err(cdp_context_destroyed()))
                },
                deadline,
                "story-x",
                Duration::from_secs(7),
                phases,
            )
            .await
            .expect_err("a CDP error that never clears must fail at the deadline");

            assert!(
                attempts.get() > 1,
                "the mechanism must retry a CDP error instead of failing on the first \
                 one, got {} attempt(s)",
                attempts.get()
            );
            match err {
                RenderError::Timeout {
                    story_id,
                    timeout,
                    phase,
                } => {
                    assert_eq!(phase, expected, "the phase must name this path's stage");
                    assert_eq!(timeout, Duration::from_secs(7));
                    assert_eq!(story_id, "story-x");
                }
                other => panic!("an exhausted retry must be story-scoped Timeout, got {other:?}"),
            }
        }
    }

    /// **期限切れ**（evaluate がそもそも返らない）が、経路自身の `timeout` を
    /// 積んだ [`RenderError::Timeout`] へ倒れること（両経路）。
    ///
    /// 期待値を文字列リテラルで書く理由は上の試験と同じ。
    ///
    /// 証明する: 返らない evaluate が deadline で打ち切られ、経路ごとの phase
    /// で報告されること。証明しない: 実ページが返らなくなる条件
    /// （`raf_suppressed_page_fails_within_the_story_timeout` 他が担う）。
    #[tokio::test]
    async fn evaluate_that_never_returns_falls_to_the_paths_own_timeout_phase() {
        let cases: [(EvaluatePhases, &str); 2] = [
            (
                REDUCED_MOTION_PHASES,
                "the reduced-motion verification never returned a verdict",
            ),
            (
                FREEZE_PHASES,
                "the freeze did not finish: the page never yielded a verdict \
                 (requestAnimationFrame may not be firing)",
            ),
        ];

        for (phases, expected) in cases {
            let deadline = std::time::Instant::now() + Duration::from_millis(200);
            let err = evaluate_with_deadline_retry(
                std::future::pending::<Result<(), chromiumoxide::error::CdpError>>,
                deadline,
                "story-y",
                Duration::from_secs(9),
                phases,
            )
            .await
            .expect_err("an evaluate that never returns must be cut at the deadline");

            match err {
                RenderError::Timeout {
                    story_id,
                    timeout,
                    phase,
                } => {
                    assert_eq!(phase, expected, "the phase must name this path's stage");
                    assert_eq!(timeout, Duration::from_secs(9));
                    assert_eq!(story_id, "story-y");
                }
                other => panic!("a deadline overrun must be story-scoped Timeout, got {other:?}"),
            }
        }
    }

    /// リトライは**回復もする**こと——抽出で「一度でも失敗したら倒す」に
    /// 変わっていないことの対照（この試験が無いと、上の二つは「常に失敗
    /// させる実装」でも通ってしまう）。
    #[tokio::test]
    async fn evaluate_retries_until_the_error_clears() {
        let attempts = std::cell::Cell::new(0usize);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let value = evaluate_with_deadline_retry(
            || {
                attempts.set(attempts.get() + 1);
                let attempt = attempts.get();
                std::future::ready(if attempt < 3 {
                    Err(cdp_context_destroyed())
                } else {
                    Ok("verdict")
                })
            },
            deadline,
            "story-z",
            Duration::from_secs(5),
            FREEZE_PHASES,
        )
        .await
        .expect("a transient CDP error must not be fatal");

        assert_eq!(value, "verdict");
        assert_eq!(attempts.get(), 3, "the first two failures must be retried");
    }

    /// Chromium を実際に起動する煙テスト。実行ファイルが無ければスキップする。
    #[tokio::test(flavor = "multi_thread")]
    async fn renders_a_story_to_a_png_with_the_requested_viewport() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP renders_a_story_to_a_png_with_the_requested_viewport: \
                 no chromium found (set CHROMIUM_PATH, install chromium, \
                 or run `pnpm exec playwright install chromium`)"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture_bundle(dir.path());

        let server = StaticServer::start(dir.path()).await.expect("start server");
        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 320, 240))
            .await
            .expect("launch chromium");

        let png = renderer
            .render_story(&server.base_url(), "demo-box--red")
            .await
            .expect("render story")
            .png;
        renderer.close().await;

        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
            "screenshot must be a real PNG"
        );

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();

        assert_eq!(
            image.dimensions(),
            (320, 240),
            "screenshot must match the requested viewport"
        );
        // fixture は id ごとに全面を塗る。真ん中は必ず赤。
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (255, 0, 0),
            "story content must actually be painted"
        );
    }

    /// **中身が空のストーリーでも撮れる**こと（30 秒タイムアウトの回帰テスト）。
    ///
    /// `#storybook-root` が空のままでも Storybook が `storyRendered` を出したなら
    /// 描画は完了している。旧実装（DOM ヒューリスティックのみ）はここでタイムアウトし、
    /// 実バンドルの `auth-passwordstrengthbar--empty`（`v-if` で何も描かない）で
    /// ビルドを丸ごと落としていた。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_story_that_renders_nothing_still_produces_a_screenshot() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_story_that_renders_nothing_still_produces_a_screenshot: no chromium");
            return;
        };

        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_storybook_runtime_bundle(dir.path());

        let server = StaticServer::start(dir.path()).await.expect("start server");
        let mut options = RenderOptions::new(chromium, 320, 240);
        // シグナル経路が効いていれば猶予もタイムアウトも待たずに戻るはず。
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let started = std::time::Instant::now();
        let png = renderer
            .render_story(&server.base_url(), "demo-box--empty")
            .await
            .expect("an empty story is a legitimate blank screenshot")
            .png;
        let elapsed = started.elapsed();

        // 塗る側も同じ経路で撮れること（シグナル経路の正常系）。
        let painted = renderer
            .render_story(&server.base_url(), "demo-box--red")
            .await
            .expect("render painted story")
            .png;
        renderer.close().await;

        assert!(
            elapsed < SIGNAL_GRACE,
            "the render signal must be used instead of the DOM fallback grace, took {elapsed:?}"
        );

        let image =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
                .decode()
                .expect("decode screenshot")
                .to_rgba8();
        assert_eq!(image.dimensions(), (320, 240));
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (255, 255, 255),
            "an empty story must screenshot as the blank viewport"
        );

        let painted = image::ImageReader::with_format(
            std::io::Cursor::new(&painted),
            image::ImageFormat::Png,
        )
        .decode()
        .expect("decode screenshot")
        .to_rgba8();
        let center = painted.get_pixel(160, 120);
        assert_eq!((center[0], center[1], center[2]), (255, 0, 0));
    }

    /// `storyErrored` は 30 秒待たずに、理由つきで失敗すること。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_story_error_signal_fails_fast_with_the_reason() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP a_story_error_signal_fails_fast_with_the_reason: no chromium");
            return;
        };

        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_storybook_runtime_bundle(dir.path());

        let server = StaticServer::start(dir.path()).await.expect("start server");
        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let started = std::time::Instant::now();
        let err = renderer
            .render_story(&server.base_url(), "demo-box--boom")
            .await
            .expect_err("a story error must fail the story");
        let elapsed = started.elapsed();
        renderer.close().await;

        let message = err.to_string();
        assert!(
            message.contains("kaboom in the play function") && message.contains("storyErrored"),
            "the error must carry the reason, got {message:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "an error signal must not wait for the timeout, took {elapsed:?}"
        );
    }

    /// Chromium が無い環境では「ハングせずにエラーを返す」こと。
    ///
    /// ここが固まると `render_build` ワーカーがジョブを掴んだまま死に、
    /// ビルドが `rendering` から永久に出られなくなる。
    #[tokio::test(flavor = "multi_thread")]
    async fn launching_a_missing_chromium_fails_fast() {
        let missing = std::env::temp_dir().join("vrt-no-such-chromium-binary");
        assert!(!missing.exists(), "fixture path must not exist");

        let options = RenderOptions::new(missing.to_str().unwrap(), 320, 240);
        let result = tokio::time::timeout(Duration::from_secs(20), StoryRenderer::launch(options))
            .await
            .expect("launch must not hang when chromium is missing");

        let err = result.err().expect("launch must fail");
        // ジョブはこの文字列をそのまま build.error_message に載せる。
        let message = err.to_string();
        assert!(
            message.contains("vrt-no-such-chromium-binary"),
            "the error must name the binary it tried, got {message:?}"
        );
    }

    fn decode_png(png: &[u8]) -> image::RgbaImage {
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
            .decode()
            .expect("decode screenshot")
            .to_rgba8()
    }

    /// **同じ story を 2 回撮ると PNG がバイト単位で一致する**こと（決定性の本丸）。
    ///
    /// - 無限アニメ（spinner）はタイムライン座標 0 に固定される
    /// - フォーカスされた入力欄のキャレットは不可視になる
    /// - 有限アニメ（slide）は**終端**に固定される——初期（赤）へ巻き戻したのでは
    ///   なく終端（青）へシークしたことを、中心ピクセルの色でも検証する
    ///
    /// 証明する: `render_story`（freeze 込み）の二回撮り決定性。証明しない:
    /// freeze なしなら揺れること（対照は `unfrozen_captures_differ_from_frozen_ones`）。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_captures_are_byte_identical_across_runs() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_captures_are_byte_identical_across_runs: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        for story in ["demo-anim--spinner", "demo-anim--caret", "demo-anim--slide"] {
            let first = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("first frozen capture")
                .png;
            let second = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("second frozen capture")
                .png;
            assert_eq!(
                first, second,
                "story {story}: two frozen captures must be byte-identical"
            );

            if story == "demo-anim--slide" {
                // 60s の有限アニメを実時間 1 秒未満で撮って終端色（青）が出るのは、
                // freeze が endTime へシークしたときだけ。paused 止まりや初期への
                // 巻き戻しでは赤系になる。
                let image = decode_png(&first);
                let center = image.get_pixel(160, 120);
                assert_eq!(
                    (center[0], center[1], center[2]),
                    (0, 0, 255),
                    "a finite animation must be frozen at its end state"
                );
            }
        }

        renderer.close().await;
    }

    /// **freeze を切ると絵が揺れる**こと（対照群 =「修正前は一致しない」の固定化）。
    ///
    /// freeze ありの決定的な絵を基準に、freeze なしの撮影が異なることを確認する。
    ///
    /// - spinner: 撮影は描画完了通知から最低でも SETTLE_DELAY（250ms）後なので、
    ///   1.7s 周期の回転は 50° 以上進んでおり、座標 0 の絵と一致しえない
    /// - caret : Chromium のキャレットはフォーカス直後は可視で、撮影は通常その
    ///   窓内（〜500ms）に行われる。負荷で明滅の消灯位相にずれた場合に備えて
    ///   数回リトライする
    ///
    /// ここで差が出ないなら freeze が効いたのではなく、fixture がそもそも
    /// アニメ／キャレットを描いていない——計測系が死んでいるということになる。
    ///
    /// 証明する: fixture が実際に動いていること（freeze なしの `render_story` で
    /// 絵が揺れる）。証明しない: freeze の決定性そのもの（上のテストが担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfrozen_captures_differ_from_frozen_ones() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfrozen_captures_differ_from_frozen_ones: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium.clone(), 320, 240);
        options.story_timeout = Duration::from_secs(10);

        // 基準となる決定的な絵（freeze あり）。
        let renderer = StoryRenderer::launch(options.clone())
            .await
            .expect("launch");
        let frozen_spinner = renderer
            .render_story(&server.base_url(), "demo-anim--spinner")
            .await
            .expect("frozen spinner")
            .png;
        let frozen_caret = renderer
            .render_story(&server.base_url(), "demo-anim--caret")
            .await
            .expect("frozen caret")
            .png;
        renderer.close().await;

        // freeze なし（= 修正前のレンダラ相当）で撮り直す。
        options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch unfrozen");
        for (story, frozen) in [
            ("demo-anim--spinner", &frozen_spinner),
            ("demo-anim--caret", &frozen_caret),
        ] {
            let mut differed = false;
            for _attempt in 0..5 {
                let unfrozen = renderer
                    .render_story(&server.base_url(), story)
                    .await
                    .expect("unfrozen capture")
                    .png;
                if &unfrozen != frozen {
                    differed = true;
                    break;
                }
            }
            assert!(
                differed,
                "story {story}: captures without the freeze must differ from the \
                 deterministic frozen capture — otherwise the fixture animates nothing \
                 and this whole test proves nothing"
            );
        }
        renderer.close().await;
    }

    /// open shadow root の**中**まで静止が届くこと（root 単位注入・再帰の検証）。
    ///
    /// 3 story とも旧実装（document.head へのみ CSS 注入）ではこのテストは落ちる:
    /// caret は shadow 内の明示宣言が継承 transparent に勝って明滅し、
    /// transition は非継承の duration が 60s のままで赤が出て、
    /// frame は shadow 境界の向こうの iframe に到達すらしない。
    ///
    /// 証明する: `render_story`（freeze 込み）の静止が shadow root の中まで
    /// 届くこと。証明しない: closed shadow root への到達（原理的に不可）。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_shadow_captures_are_byte_identical_across_runs() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_shadow_captures_are_byte_identical_across_runs: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        for story in [
            "demo-shadow--caret",
            "demo-shadow--transition",
            "demo-shadow--frame",
        ] {
            let first = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("first frozen capture")
                .png;
            let second = renderer
                .render_story(&server.base_url(), story)
                .await
                .expect("second frozen capture")
                .png;
            assert_eq!(
                first, second,
                "story {story}: two frozen captures must be byte-identical"
            );

            if story == "demo-shadow--transition" {
                // shadow 内の要素は自分の computed transition-duration を色で
                // 表示している。緑（0s）が出るのは、注入 CSS が shadow root の
                // 中に入って非継承プロパティを上書きしたときだけ。
                let image = decode_png(&first);
                let center = image.get_pixel(160, 120);
                assert_eq!(
                    (center[0], center[1], center[2]),
                    (0, 255, 0),
                    "transition-duration inside the shadow root must be forced to 0s"
                );
            }
        }

        renderer.close().await;
    }

    /// `caret-color` が shadow の中で**本当に効いた**ことの直接証拠。
    ///
    /// 2 回撮って一致するだけでは「キャレットが毎回同じ位相で写っている」
    /// 可能性を排除できない。shadow 側で `caret-color: transparent` を明示した
    /// 対照 story と絵が一致するなら、キャレットは確かに不可視である。
    /// 旧実装（document へのみ注入）では、shadow 内の明示 `caret-color` が
    /// 継承の transparent に勝ち、フォーカス直後の可視位相のキャレットが
    /// 写り込んで一致しない。
    ///
    /// 証明する: `render_story`（freeze 込み）で shadow 内キャレットが本当に
    /// 不可視になること。証明しない: fixture が動いていること（対照群が担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_shadow_caret_matches_an_explicitly_hidden_caret() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_shadow_caret_matches_an_explicitly_hidden_caret: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let caret = renderer
            .render_story(&server.base_url(), "demo-shadow--caret")
            .await
            .expect("frozen shadow caret")
            .png;
        let hidden = renderer
            .render_story(&server.base_url(), "demo-shadow--caret-hidden")
            .await
            .expect("frozen shadow caret-hidden")
            .png;
        assert_eq!(
            caret, hidden,
            "a frozen caret inside a shadow root must be indistinguishable from an \
             explicitly transparent caret — otherwise the injected caret-color did \
             not reach the shadow root"
        );

        renderer.close().await;
    }

    /// shadow バンドルの対照群（=「静止なしでは一致しない」の固定化）。
    ///
    /// ここで差が出ないなら fixture がそもそも shadow の中で何も動かして
    /// いないということであり、上の一致テストは何も証明しなくなる。
    ///
    /// 証明する: shadow fixture が実際に動いていること（freeze なしの
    /// `render_story` で絵が揺れる）。証明しない: freeze の決定性そのもの。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfrozen_shadow_captures_differ_from_frozen_ones() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfrozen_shadow_captures_differ_from_frozen_ones: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium.clone(), 320, 240);
        options.story_timeout = Duration::from_secs(10);

        let renderer = StoryRenderer::launch(options.clone())
            .await
            .expect("launch");
        let frozen_caret = renderer
            .render_story(&server.base_url(), "demo-shadow--caret")
            .await
            .expect("frozen shadow caret")
            .png;
        let frozen_transition = renderer
            .render_story(&server.base_url(), "demo-shadow--transition")
            .await
            .expect("frozen shadow transition")
            .png;
        let frozen_frame = renderer
            .render_story(&server.base_url(), "demo-shadow--frame")
            .await
            .expect("frozen shadow frame")
            .png;
        renderer.close().await;

        options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch unfrozen");

        // transition: 静止なしの computed duration は 60s のままなので赤が出る。
        // 差分の存在だけでなく「fixture が生きている」ことまで色で確認する。
        let unfrozen_transition = renderer
            .render_story(&server.base_url(), "demo-shadow--transition")
            .await
            .expect("unfrozen shadow transition")
            .png;
        assert_ne!(
            unfrozen_transition, frozen_transition,
            "the shadow transition story must differ without the freeze"
        );
        let image = decode_png(&unfrozen_transition);
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (255, 0, 0),
            "without the freeze the shadow element must still see its 60s duration"
        );

        // caret / frame: キャレットの明滅は位相依存なので、消灯位相を引いた
        // 場合に備えて数回リトライする（既存の対照群テストと同じ扱い）。
        for (story, frozen) in [
            ("demo-shadow--caret", &frozen_caret),
            ("demo-shadow--frame", &frozen_frame),
        ] {
            let mut differed = false;
            for _attempt in 0..5 {
                let unfrozen = renderer
                    .render_story(&server.base_url(), story)
                    .await
                    .expect("unfrozen capture")
                    .png;
                if &unfrozen != frozen {
                    differed = true;
                    break;
                }
            }
            assert!(
                differed,
                "story {story}: captures without the freeze must differ from the \
                 deterministic frozen capture — otherwise the fixture animates nothing \
                 inside the shadow root and the identity test above proves nothing"
            );
        }
        renderer.close().await;
    }

    /// [`write_transition_event_bundle`] のページを開き、イベントカウンタが
    /// 載るまで待つ。
    async fn open_transition_page(
        renderer: &StoryRenderer,
        url: &str,
    ) -> chromiumoxide::page::Page {
        let page = renderer.browser.new_page(url).await.expect("open page");
        wait_until(
            &page,
            "document.readyState !== 'loading' && !!window.__TRANSITION_EVENTS__",
        )
        .await;
        page
    }

    /// 現在のイベントカウンタを取り出す。
    async fn transition_event_counts(page: &chromiumoxide::page::Page) -> serde_json::Value {
        let raw = page
            .evaluate("JSON.stringify(window.__TRANSITION_EVENTS__)")
            .await
            .expect("read event counters");
        serde_json::from_str(raw.value().and_then(|v| v.as_str()).expect("counter json"))
            .expect("parse counter json")
    }

    /// **freeze 後に始まる transition はイベントを発火しない**こと（両方向の証拠）。
    ///
    /// FREEZE_SCRIPT は `transition-duration: 0s` / `transition-delay: 0s` を
    /// 注入する。CSS Transitions Level 1 §3 は combined duration
    /// （= max(duration, 0s) + delay）が 0s より大きいときだけ transition を
    /// 開始すると定めるので、freeze 後のスタイル変化は transition を生成せず、
    /// `transitionrun` / `transitionstart` / `transitionend` は一切発火しない。
    /// これは「無いことの確認」なので、判定窓 [`EVENT_WAIT`] の十分性を
    /// 思い込みにしないため、次の 3 段で確かめる。
    ///
    /// 1. **positive control（freeze なし）**: 同じ fixture・同じ環境で
    ///    transition を起こし、`transitionend` が EVENT_WAIT 内に届くことを実測する。
    ///    ここが通らなければ fixture かカウンタが死んでおり、以降の 0 は無意味
    /// 2. **freeze 後に起こす transition**: freeze → スタイル変更 → EVENT_WAIT
    ///    待って全イベント 0 を確認。値そのものは即座に終値へ変わっていることも見る
    /// 3. **freeze 時点で走っている transition**: 60s の transition を開始させ
    ///    （`transitionrun` の実測で開始を確認）、freeze が終端へシークすると
    ///    その transition は**完了として `transitionend` を発火する**ことを確認。
    ///    発火しないのは手順 2 の「freeze 後に始まるはずだった transition」であり、
    ///    走行中のものは完了イベントつきで終端に確定する——という境界を固定する
    ///
    /// 証明する: FREEZE_SCRIPT **単体**のイベント境界（`new_page` 上で直接
    /// evaluate）。証明しない: `render_story` 経路・`freeze_verdict` の判定——
    /// このテストはどちらも通っていない（そちらは frozen_* 系が担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn transitions_under_freeze_fire_no_events() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP transitions_under_freeze_fire_no_events: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        // 「発火しなかった」と判定するまでの待ち時間。fixture の速い transition は
        // duration 0.1s・delay 0s なので、transition が生成されていれば
        // `transitionend` は開始から約 100ms 後に届く。その 20 倍を窓に取り、
        // さらに手順 1 の positive control が**同じ窓・同じ環境**で実際に
        // 発火することを測って、窓の長さを推測でなく実測で裏づける。
        const EVENT_WAIT: Duration = Duration::from_secs(2);
        const POLL: Duration = Duration::from_millis(50);

        let dir = tempfile::tempdir().expect("tempdir");
        write_transition_event_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");
        let url = format!("{}/iframe.html", server.base_url());

        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 320, 240))
            .await
            .expect("launch");

        // ── 1. positive control: freeze なしなら transitionend は EVENT_WAIT 内に届く
        let page = open_transition_page(&renderer, &url).await;
        page.evaluate("document.getElementById('fast').style.backgroundColor = '#0000ff'")
            .await
            .expect("trigger fast transition");
        let started = std::time::Instant::now();
        let mut fired = false;
        while started.elapsed() < EVENT_WAIT {
            let counts = transition_event_counts(&page).await;
            if counts["transitionend"].as_u64().unwrap_or(0) >= 1 {
                fired = true;
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        assert!(
            fired,
            "positive control: without the freeze, transitionend must arrive within \
             {EVENT_WAIT:?} — otherwise the fixture or the counters are broken and \
             the zero counts below would prove nothing"
        );
        let _ = page.close().await;

        // ── 2. freeze 後に起こした transition はイベントを一切出さない
        let page = open_transition_page(&renderer, &url).await;
        page.evaluate(FREEZE_SCRIPT).await.expect("freeze");
        // 注入が効いたこと（computed が 0s）を先に確認する。ここが 0s でないなら
        // 「イベント 0」は freeze の証明ではなくただの取りこぼしになる。
        let duration = page
            .evaluate("getComputedStyle(document.getElementById('fast')).transitionDuration")
            .await
            .expect("read computed duration");
        assert_eq!(
            duration.value().and_then(|v| v.as_str()),
            Some("0s"),
            "the injected freeze CSS must zero out transition-duration first"
        );
        page.evaluate("document.getElementById('fast').style.backgroundColor = '#0000ff'")
            .await
            .expect("trigger fast transition under freeze");
        tokio::time::sleep(EVENT_WAIT).await;
        let counts = transition_event_counts(&page).await;
        for event in [
            "transitionrun",
            "transitionstart",
            "transitionend",
            "transitioncancel",
        ] {
            assert_eq!(
                counts[event].as_u64(),
                Some(0),
                "no transition is created under the freeze, so `{event}` must never fire"
            );
        }
        // transition が生成されないだけで、値そのものは即座に終値へ変わる。
        let color = page
            .evaluate("getComputedStyle(document.getElementById('fast')).backgroundColor")
            .await
            .expect("read computed color");
        assert_eq!(
            color.value().and_then(|v| v.as_str()),
            Some("rgb(0, 0, 255)"),
            "the property change itself must still apply instantly"
        );
        let _ = page.close().await;

        // ── 3. freeze 時点で走っていた transition は終端へシークされ transitionend を出す
        let page = open_transition_page(&renderer, &url).await;
        page.evaluate("document.getElementById('slow').style.backgroundColor = '#0000ff'")
            .await
            .expect("trigger slow transition");
        // transition が実際に生成・開始されたことを実測してから freeze する。
        let started = std::time::Instant::now();
        loop {
            let counts = transition_event_counts(&page).await;
            if counts["transitionrun"].as_u64().unwrap_or(0) >= 1 {
                break;
            }
            assert!(
                started.elapsed() < EVENT_WAIT,
                "the 60s transition must actually start (transitionrun) before the freeze"
            );
            tokio::time::sleep(POLL).await;
        }
        page.evaluate(FREEZE_SCRIPT).await.expect("freeze");
        // freeze は走行中の transition を終端へシークして pause する。
        // 終端の絵（青）が出ていることを確認した上で、この完了に伴い
        // `transitionend` が発火する（Chromium 実測）ことを固定する。
        let color = page
            .evaluate("getComputedStyle(document.getElementById('slow')).backgroundColor")
            .await
            .expect("read computed color");
        assert_eq!(
            color.value().and_then(|v| v.as_str()),
            Some("rgb(0, 0, 255)"),
            "the freeze must seek the running transition to its end state"
        );
        tokio::time::sleep(EVENT_WAIT).await;
        let counts = transition_event_counts(&page).await;
        assert_eq!(
            counts["transitionend"].as_u64(),
            Some(1),
            "a transition already running when the freeze hits completes at its end \
             state and does fire transitionend — only transitions that would start \
             after the freeze fire nothing"
        );
        let _ = page.close().await;

        renderer.close().await;
    }

    /// progress-based timeline (`animation-timeline: scroll()`) を持つバンドル。
    ///
    /// `endTime` が `CSSUnitValue(100, "percent")` になるため、
    /// `Number.isFinite` では false と判定されて数値 0 を代入しようとし
    /// `TypeError` になる。修正前は catch で握りつぶされ running のまま成功扱い。
    fn write_scroll_timeline_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  #scroller { width:100%;height:200px;overflow-y:scroll; }
  #content { height:1000px; }
  @keyframes scroll-fade { from { opacity:1; } to { opacity:0; } }
  #target {
    width:100px;height:100px;background:#ff0000;
    animation: scroll-fade linear;
    animation-timeline: scroll(nearest block);
  }"#,
            r#"
  <div id="scroller">
    <div id="target"></div>
    <div id="content"></div>
  </div>
"#,
            "      channel.emit('storyRendered', 'scroll-timeline');",
        );
    }

    /// `animationend` で次のアニメーションを連鎖的に開始するバンドル。
    ///
    /// p1(50ms) → animationend → p2(50ms) → animationend → p3(50ms)。
    /// 2 巡固定の sweep では p3 が running のまま残り得る。
    fn write_animationend_chain_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  @keyframes phase1 { from { background:#ff0000; } to { background:#cc0000; } }
  @keyframes phase2 { from { background:#00ff00; } to { background:#00cc00; } }
  @keyframes phase3 { from { background:#0000ff; } to { background:#0000cc; } }
  #box { width:100%;height:100vh; }"#,
            r#"<div id="box"></div>"#,
            r#"      var box = document.getElementById('box');
      box.addEventListener('animationend', function handler(e) {
        if (e.animationName === 'phase1') {
          box.style.animation = 'phase2 0.05s linear 1 forwards';
        } else if (e.animationName === 'phase2') {
          box.style.animation = 'phase3 0.05s linear 1 forwards';
        }
      });
      box.style.animation = 'phase1 0.05s linear 1 forwards';
      channel.emit('storyRendered', 'chain');"#,
        );
    }

    /// `animationend` で 4 段連鎖（p1→p2→p3→p4, 各 60s）するバンドル。
    ///
    /// 修正前の件数ベース判定では、各巡で残数が 1 のまま identity が入れ替わる
    /// ため「進捗なし」と誤判定し 3 巡目で打ち切っていた。集合ベースの判定では
    /// 各巡で animation 名が変わる（p1→p2→p3→p4）ため進行と判じ、全段を
    /// 止めきって成功する。
    fn write_long_chain_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  @keyframes p1 { from { background:#ff0000; } to { background:#cc0000; } }
  @keyframes p2 { from { background:#00ff00; } to { background:#00cc00; } }
  @keyframes p3 { from { background:#0000ff; } to { background:#0000cc; } }
  @keyframes p4 { from { background:#ffff00; } to { background:#cccc00; } }
  #box { width:100%;height:100vh; }"#,
            r#"<div id="box"></div>"#,
            r#"      var box = document.getElementById('box');
      box.addEventListener('animationend', function handler(e) {
        if (e.animationName === 'p1') {
          box.style.animation = 'p2 60s linear 1 forwards';
        } else if (e.animationName === 'p2') {
          box.style.animation = 'p3 60s linear 1 forwards';
        } else if (e.animationName === 'p3') {
          box.style.animation = 'p4 60s linear 1 forwards';
        }
      });
      box.style.animation = 'p1 60s linear 1 forwards';
      channel.emit('storyRendered', 'long-chain');"#,
        );
    }

    /// CSP `style-src 'self'` を持つバンドル。
    ///
    /// `<style>` 要素の注入は CSP に拒否されるが例外は出ない（この検知不能な
    /// 黙殺が、注入を constructed stylesheet へ移した理由である——CSSOM 操作は
    /// CSP style-src の管轄外なので拒否されない）。`caret` で入力欄の
    /// `caret-color` を変えられる: 可視色と `transparent` の対照を同じ CSP 下で
    /// 撮り比べることで、「静止 CSS が CSP を越えて効いた」ことを絵で証明する。
    fn write_strict_csp_bundle(root: &Path, caret: &str) {
        std::fs::write(
            root.join("styles.css"),
            format!(
                "html,body{{margin:0;padding:0;background:#fff}}\n\
                 input{{font:32px monospace;width:200px;margin:40px;border:1px solid #000;\
                 caret-color:{caret};transition:opacity 60s linear}}\n"
            ),
        )
        .expect("write styles.css");
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head>
<meta http-equiv="Content-Security-Policy" content="style-src 'self'">
<link rel="stylesheet" href="styles.css">
</head>
<body><div id="storybook-root"><input></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {
      document.querySelector('input').focus();
      channel.emit('storyRendered', 'strict-csp');
    }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// CSP `script-src 'none'` を持つバンドル。
    ///
    /// `setBypassCSP` を使っていた頃は、CSP 全体が迂回されるため
    /// `script-src 'none'` でも inline script が実行されてしまっていた。
    /// bypass を外した現在は、CSP がそのまま効くので inline script は
    /// 実行されないことを検証する。
    fn write_script_csp_bundle(root: &Path) {
        std::fs::write(
            root.join("styles.css"),
            "html,body{margin:0;padding:0;background:#fff}\n\
             #box{width:100%;height:100vh;background:#00ff00}\n",
        )
        .expect("write styles.css");
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head>
<meta http-equiv="Content-Security-Policy" content="script-src 'none'; style-src 'self'">
<link rel="stylesheet" href="styles.css">
</head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  document.getElementById('box').style.background = '#ff0000';
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// 同一 keyframes が 4 つの**別要素**へ順移動するバンドル（各 60s）。
    ///
    /// el1 の animationend で el2 へ、el2 → el3、el3 → el4。
    /// identity proxy（animation 名 + tagName）は全巡で同一文字列になるため、
    /// 修正前の identity ベース早期停止では 3 巡目で「停滞」と誤判定していた。
    /// MAX_SWEEPS まで回す現行コードでは全段を止めきって成功する。
    fn write_roaming_keyframes_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  @keyframes pulse { from { background:#ff0000; } to { background:#0000ff; } }
  .target { width:80px;height:80px;display:inline-block;background:#cccccc; }"#,
            r#"
  <div class="target" id="el1"></div>
  <div class="target" id="el2"></div>
  <div class="target" id="el3"></div>
  <div class="target" id="el4"></div>
"#,
            r#"      var els = [
        document.getElementById('el1'),
        document.getElementById('el2'),
        document.getElementById('el3'),
        document.getElementById('el4')
      ];
      function startOn(idx) {
        els[idx].style.animation = 'pulse 60s linear 1 forwards';
        els[idx].addEventListener('animationend', function handler() {
          els[idx].removeEventListener('animationend', handler);
          if (idx + 1 < els.length) startOn(idx + 1);
        });
      }
      startOn(0);
      channel.emit('storyRendered', 'roaming');"#,
        );
    }

    /// Web Animations API で id を持たない animation を連鎖するバンドル。
    ///
    /// `el.animate()` が返す Animation には `animationName` も `id` も無い。
    /// collectRunning が `''` を出す generic なケースの回帰テスト。
    fn write_waapi_chain_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#ff0000; }"#,
            r#"<div id="box"></div>"#,
            r#"      var box = document.getElementById('box');
      var phases = [
        [{ background: '#ff0000' }, { background: '#00ff00' }],
        [{ background: '#00ff00' }, { background: '#0000ff' }],
        [{ background: '#0000ff' }, { background: '#ffff00' }],
      ];
      var i = 0;
      function runNext() {
        if (i >= phases.length) return;
        var anim = box.animate(phases[i], { duration: 60000, fill: 'forwards' });
        i++;
        anim.onfinish = function () { runNext(); };
      }
      runNext();
      channel.emit('storyRendered', 'waapi-chain');"#,
        );
    }

    /// **わざと凍らせられないページ**。`animationend` で無限に連鎖し続ける。
    ///
    /// 静止の反復上限を超えて running が残り続けるため、fail-closed な
    /// レンダラは失敗を返さなければならない。修正前のコードでは成功扱いになる。
    fn write_unfreezable_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  @keyframes blink-a { from { opacity:1; } to { opacity:0.5; } }
  @keyframes blink-b { from { opacity:0.5; } to { opacity:1; } }
  #box { width:100%;height:100vh;background:#ff0000; }"#,
            r#"<div id="box"></div>"#,
            r#"      var box = document.getElementById('box');
      box.addEventListener('animationend', function handler(e) {
        // 無限連鎖: 一方が終わったら他方を開始する。
        if (e.animationName === 'blink-a') {
          box.style.animation = 'blink-b 0.03s linear 1 forwards';
        } else {
          box.style.animation = 'blink-a 0.03s linear 1 forwards';
        }
      });
      box.style.animation = 'blink-a 0.03s linear 1 forwards';
      channel.emit('storyRendered', 'unfreezable');"#,
        );
    }

    /// scroll timeline の二回撮り一致（progress-based timeline の回帰テスト）。
    ///
    /// `endTime` が `CSSUnitValue(100, "percent")` のとき、型を保って
    /// `currentTime` へ渡すことで `TypeError` を回避し、pause まで到達する。
    /// 修正前は catch 握りつぶしで running のまま成功扱いになっていた。
    ///
    /// 証明する: `render_story`（freeze 込み）が progress-based timeline でも
    /// 決定的に撮れること。証明しない: freeze なしで揺れること（対照が別にある）。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_scroll_timeline_captures_are_byte_identical() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_scroll_timeline_captures_are_byte_identical: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_scroll_timeline_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("first scroll timeline capture")
            .png;
        let second = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("second scroll timeline capture")
            .png;
        assert_eq!(
            first, second,
            "scroll timeline: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// animationend 連鎖（p1→p2→p3）の二回撮り一致（収束反復の回帰テスト）。
    ///
    /// 2 巡固定では p3 が running のまま残り得る。収束ループが全段を
    /// 止めきることで、決定的な絵が得られる。
    ///
    /// 証明する: `render_story`（freeze 込み）が短い連鎖を収束させること。
    /// 証明しない: 収束不能ページの fail-closed（unfreezable 系が担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_animationend_chain_captures_are_byte_identical() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP frozen_animationend_chain_captures_are_byte_identical: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_animationend_chain_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "chain")
            .await
            .expect("first chain capture")
            .png;
        let second = renderer
            .render_story(&server.base_url(), "chain")
            .await
            .expect("second chain capture")
            .png;
        assert_eq!(
            first, second,
            "animationend chain: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// **positive control（中心の証明）**: わざと凍らせられないページで
    /// レンダラが失敗を返すこと。
    ///
    /// `animationend` で無限に連鎖し続けるページは、収束反復の上限を
    /// 超えても running が残るため、fail-closed なレンダラは `RenderError::Story`
    /// を返さなければならない。修正前のコードでは `true` を返して成功扱いになる。
    ///
    /// 証明する: `render_story`（freeze 込み）が凍結不能ページで失敗を返すこと。
    /// 証明しない: この失敗が freeze 由来であること単体——それは下の対照が担う。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfreezable_page_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfreezable_page_fails_instead_of_silently_succeeding: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_unfreezable_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(30);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "unfreezable")
            .await
            .expect_err("an unfreezable page must fail — not silently succeed");

        let message = err.to_string();
        assert!(
            message.contains("freeze failed") && message.contains("still running"),
            "the error must describe what could not be frozen, got {message:?}"
        );

        renderer.close().await;
    }

    /// **修正前との対比（positive control の逆方向）**: freeze を無効にした
    /// レンダラでは、凍結不能ページでも成功する（旧挙動の固定）。
    ///
    /// 上の `unfreezable_page_fails` と対で、修正前のコードでは成功として
    /// 返ってしまう（= fail-open だった）ことを証明する。
    ///
    /// 証明する: 上の失敗が freeze 由来であること（freeze を切れば同じページで
    /// 成功する）。証明しない: freeze なしの撮影が決定的であること。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfreezable_page_succeeds_without_freeze() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfreezable_page_succeeds_without_freeze: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_unfreezable_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(30);
        options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        renderer
            .render_story(&server.base_url(), "unfreezable")
            .await
            .expect("without the freeze, the unfreezable page must succeed (old behavior)");

        renderer.close().await;
    }

    /// **わざと凍らせられない**アニメーション（animationend の無限連鎖）を
    /// open shadow root の**中**に置くバンドル。running の収集
    /// （`collectRunning`）の走査が shadow へ潜ることの片側検証用（cmd_662 ①
    /// ——対③「走査を複数箇所へ写したなら各々に片側だけ壊すと赤くなる試験が
    /// あるか」）。
    fn write_shadow_unfreezable_bundle(root: &Path) {
        write_story_html(
            root,
            "  html,body{margin:0;padding:0;background:#fff}",
            "",
            r#"      var host = document.createElement('div');
      document.getElementById('storybook-root').appendChild(host);
      var shadow = host.attachShadow({ mode: 'open' });
      shadow.innerHTML = '<style>' +
        '@keyframes blink-a { from { opacity:1; } to { opacity:0.5; } }' +
        '@keyframes blink-b { from { opacity:0.5; } to { opacity:1; } }' +
        '#box { width:100vw; height:100vh; background:#ff0000; }' +
        '</style><div id="box"></div>';
      var box = shadow.getElementById('box');
      box.addEventListener('animationend', function (e) {
        // 無限連鎖: 一方が終わったら他方を開始する（top-level 版と同型）。
        if (e.animationName === 'blink-a') {
          box.style.animation = 'blink-b 0.03s linear 1 forwards';
        } else {
          box.style.animation = 'blink-a 0.03s linear 1 forwards';
        }
      });
      box.style.animation = 'blink-a 0.03s linear 1 forwards';
      channel.emit('storyRendered', id);"#,
        );
    }

    /// 【cmd_662 ①・対③】open shadow root の**中**の凍結不能アニメーションも
    /// 「running が残った」として検知され、失敗を返すこと。
    ///
    /// freezeRoot（凍らせる側）と collectRunning（数える側）は walk を各自に
    /// 持つ写しである。**凍らせる側**だけ shadow 降下を失っても running は
    /// 残って失敗する（fail-closed——本試験は緑のまま。凍結の実効は
    /// `frozen_shadow_captures_are_byte_identical_across_runs` が固定）が、
    /// **数える側**だけ失うと shadow 内の running を見逃して `ok: true` を
    /// 返す fail-open になり、それを赤くする試験が無かった。本試験は
    /// 数える側の shadow 降下を固定する——collectRunning の走査を狭めると、
    /// このバンドルが失敗せず撮れてしまい、本試験が落ちる。
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unfreezable_animation_inside_a_shadow_root_is_still_detected() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP an_unfreezable_animation_inside_a_shadow_root: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_unfreezable_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(30);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "shadow-unfreezable")
            .await
            .expect_err(
                "an unfreezable animation inside a shadow root must fail — \
                 succeeding means collectRunning's walk no longer descends \
                 into shadow roots (a one-sided scan regression)",
            );

        let message = err.to_string();
        assert!(
            message.contains("freeze failed") && message.contains("still running"),
            "the error must describe what could not be frozen, got {message:?}"
        );

        renderer.close().await;
    }

    /// 凍結不能アニメーション（animationend の無限連鎖）を持つ frame.html を
    /// 書き出す共通素材。iframe 直下（[`write_iframe_unfreezable_bundle`]）と
    /// frameset 経由（[`write_frameset_unfreezable_bundle`]）が共有する。
    fn write_unfreezable_frame_html(root: &Path) {
        std::fs::write(
            root.join("unfreezable-frame.html"),
            r#"<!doctype html>
<html><head><style>
  @keyframes blink-a { from { opacity:1; } to { opacity:0.5; } }
  @keyframes blink-b { from { opacity:0.5; } to { opacity:1; } }
  #box { width:100vw; height:100vh; background:#ff0000; }
</style></head>
<body><div id="box"></div>
<script>
  var box = document.getElementById('box');
  box.addEventListener('animationend', function (e) {
    // 無限連鎖: 一方が終わったら他方を開始する（top-level 版と同型）。
    if (e.animationName === 'blink-a') {
      box.style.animation = 'blink-b 0.03s linear 1 forwards';
    } else {
      box.style.animation = 'blink-a 0.03s linear 1 forwards';
    }
  });
  box.style.animation = 'blink-a 0.03s linear 1 forwards';
</script>
</body></html>"#,
        )
        .expect("write unfreezable-frame.html");
    }

    /// **わざと凍らせられない**アニメーションを同一オリジン iframe の**中**に
    /// 置くバンドル。走査（[`FREEZE_SCRIPT`] の `walkRoots`）の iframe 分岐の
    /// 検知側検証用（cmd_663 ⑦——shadow 版
    /// [`write_shadow_unfreezable_bundle`] と同型の空欄埋め）。
    fn write_iframe_unfreezable_bundle(root: &Path) {
        write_unfreezable_frame_html(root);
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  iframe { position:fixed; inset:0; width:100%; height:100vh; border:0; }"#,
            "",
            r#"      var frame = document.createElement('iframe');
      frame.src = 'unfreezable-frame.html';
      document.getElementById('storybook-root').appendChild(frame);
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
      });"#,
        );
    }

    /// 【cmd_663 ⑦】同一オリジン iframe の**中**の凍結不能アニメーションも
    /// 「running が残った」として検知され、失敗を返すこと。
    ///
    /// shadow 版（`an_unfreezable_animation_inside_a_shadow_root_is_still_
    /// detected`）と同じ理屈: 凍らせる側だけ iframe 降下を失っても running は
    /// 残って失敗する（fail-closed）が、**数える側**だけ失うと iframe 内の
    /// running を見逃して `ok: true` を返す fail-open になる——それを赤く
    /// する試験が iframe 分岐には無かった。走査を `walkRoots` へ一本化した
    /// 後は、この試験は共有走査そのものの iframe 分岐を（凍らせる側・数える側
    /// まとめて）固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unfreezable_animation_inside_a_same_origin_iframe_is_still_detected() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP an_unfreezable_animation_inside_a_same_origin_iframe: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_iframe_unfreezable_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(30);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "iframe-unfreezable")
            .await
            .expect_err(
                "an unfreezable animation inside a same-origin iframe must fail — \
                 succeeding means the shared walk no longer descends into iframes",
            );

        let message = err.to_string();
        assert!(
            message.contains("freeze failed") && message.contains("still running"),
            "the error must describe what could not be frozen, got {message:?}"
        );

        renderer.close().await;
    }

    /// **わざと凍らせられない**アニメーションを frameset の `<frame>` の中に
    /// 置くバンドル。走査の `frame` 分岐の検知側検証用（cmd_663 ⑦）。
    /// frameset document は story の top にはなれない（body を持てず
    /// `#storybook-root` を置けない）ため、iframe → frameset → frame の形は
    /// [`write_frameset_webfont_bundle`] と同じ。
    fn write_frameset_unfreezable_bundle(root: &Path) {
        write_unfreezable_frame_html(root);
        std::fs::write(
            root.join("frameset.html"),
            r#"<!doctype html>
<html><frameset cols="100%"><frame src="unfreezable-frame.html"></frameset></html>"#,
        )
        .expect("write frameset.html");
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  iframe { position:fixed; inset:0; width:100%; height:100vh; border:0; }"#,
            "",
            r#"      var frame = document.createElement('iframe');
      frame.src = 'frameset.html';
      document.getElementById('storybook-root').appendChild(frame);
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
      });"#,
        );
    }

    /// 【cmd_663 ⑦】frameset の `<frame>` の中の凍結不能アニメーションも
    /// 検知され、失敗を返すこと。
    ///
    /// 修正前の走査（freezeRoot / collectRunning が各自に持つ写し）では、
    /// `frame` 分岐を**両側から**消しても既存のどの試験も赤くならなかった
    /// ——frameset fixture はフォント待ち側（`fonts_inside_a_frameset_frame_
    /// are_awaited_before_the_capture`）にしか無く、FREEZE の `frame` 分岐は
    /// 宣言（README「`<iframe>` と `<frame>` の両方を見る」）だけで試験の
    /// 裏づけが無かった。本試験は走査の `frame` 分岐を検知側から固定する。
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unfreezable_animation_inside_a_frameset_frame_is_still_detected() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP an_unfreezable_animation_inside_a_frameset_frame: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_frameset_unfreezable_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(30);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "frameset-unfreezable")
            .await
            .expect_err(
                "an unfreezable animation inside a frameset frame must fail — \
                 succeeding means the shared walk no longer descends through \
                 `frame` elements",
            );

        let message = err.to_string();
        assert!(
            message.contains("freeze failed") && message.contains("still running"),
            "the error must describe what could not be frozen, got {message:?}"
        );

        renderer.close().await;
    }

    /// 無限スピナーを frameset の `<frame>` の中に置くバンドル。凍らせる側が
    /// `frame` へ届くことの検証用（cmd_663 ⑦——検知側の
    /// [`write_frameset_unfreezable_bundle`] と対）。
    fn write_frameset_animated_bundle(root: &Path) {
        std::fs::write(
            root.join("spinner-frame.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
  .spinner {
    width:120px;height:120px;margin:20px;
    border:24px solid #dddddd;border-top-color:#ff0000;border-radius:50%;
    animation: spin 1.7s linear infinite;
  }
</style></head>
<body><div class="spinner"></div></body></html>"#,
        )
        .expect("write spinner-frame.html");
        std::fs::write(
            root.join("frameset.html"),
            r#"<!doctype html>
<html><frameset cols="100%"><frame src="spinner-frame.html"></frameset></html>"#,
        )
        .expect("write frameset.html");
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  iframe { position:fixed; inset:0; width:100%; height:100vh; border:0; }"#,
            "",
            r#"      var frame = document.createElement('iframe');
      frame.src = 'frameset.html';
      document.getElementById('storybook-root').appendChild(frame);
      frame.addEventListener('load', function () {
        channel.emit('storyRendered', id);
      });"#,
        );
    }

    /// 【cmd_663 ⑦・凍らせる側】frameset の `<frame>` の中の無限アニメーション
    /// が座標 0 で静止され、二回撮って同じ絵になること。
    ///
    /// 凍らせる側だけが `frame` へ届かない退行では、running が残って
    /// 数える側が失敗させる（この試験は expect が落ちて赤）。両側まとめて
    /// 届かない退行は上の検知側試験が赤くする——対で `frame` 分岐の
    /// 全断面を覆う。
    #[tokio::test(flavor = "multi_thread")]
    async fn an_animation_inside_a_frameset_frame_freezes_deterministically() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP an_animation_inside_a_frameset_frame_freezes: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_frameset_animated_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "frameset-spinner")
            .await
            .expect(
                "first frameset spinner capture — a freeze failure means \
                     the freezing side reaches the frame but could not settle it",
            )
            .png;
        let second = renderer
            .render_story(&server.base_url(), "frameset-spinner")
            .await
            .expect("second frameset spinner capture")
            .png;
        assert_eq!(
            first, second,
            "frameset spinner: two frozen captures must be byte-identical — \
             a mismatch means the freezing side no longer reaches `frame` \
             documents while the counting side also misses them"
        );

        renderer.close().await;
    }

    /// 4 段の長い連鎖が打ち切られず収束すること（MAX_SWEEPS 反復の回帰テスト）。
    ///
    /// p1→p2→p3→p4（各 60s）の animationend 連鎖。MAX_SWEEPS まで
    /// 反復を続け、全段を止めきって成功する。
    ///
    /// 証明する: `render_story`（freeze 込み）が旧・件数ベース判定で打ち切られた
    /// 長さの連鎖も収束させること。証明しない: MAX_SWEEPS 超の連鎖の挙動。
    #[tokio::test(flavor = "multi_thread")]
    async fn long_animation_chain_converges_without_being_cut_short() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP long_animation_chain_converges_without_being_cut_short: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_long_chain_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "long-chain")
            .await
            .expect("a 4-step chain must converge — not be cut short")
            .png;
        let second = renderer
            .render_story(&server.base_url(), "long-chain")
            .await
            .expect("second long-chain capture")
            .png;
        assert_eq!(
            first, second,
            "long-chain: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// 同一 keyframes が 4 つの別要素へ順移動する連鎖が収束すること。
    ///
    /// el1→el2→el3→el4 で animation 名（pulse）と tagName（DIV）は全巡同一。
    /// 修正前の identity proxy（名前+tagName）ベースの早期停止では、全巡で
    /// 同一文字列になるため 3 巡目で「停滞」と誤判定していた。
    /// MAX_SWEEPS まで回す現行コードでは全段を止めきって成功する。
    ///
    /// 証明する: `render_story`（freeze 込み）が要素間を移る同名アニメでも
    /// 収束すること。証明しない: freeze なしで揺れること（対照が別にある）。
    #[tokio::test(flavor = "multi_thread")]
    async fn roaming_keyframes_across_elements_converge() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP roaming_keyframes_across_elements_converge: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_roaming_keyframes_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "roaming")
            .await
            .expect(
                "same keyframes roaming across 4 elements must converge \
                 — identity proxy would have cut this short at sweep 3",
            )
            .png;
        let second = renderer
            .render_story(&server.base_url(), "roaming")
            .await
            .expect("second roaming capture")
            .png;
        assert_eq!(
            first, second,
            "roaming keyframes: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// WAAPI（Web Animations API）の id 無し連鎖が収束すること。
    ///
    /// `el.animate()` が返す Animation は `animationName` も `id` も空文字列。
    /// collectRunning は `'':DIV` を出し、全巡で同一文字列になる generic なケース。
    ///
    /// 証明する: `render_story`（freeze 込み）が WAAPI の無名連鎖でも収束する
    /// こと。証明しない: freeze なしで揺れること（対照が別にある）。
    #[tokio::test(flavor = "multi_thread")]
    async fn waapi_chain_without_ids_converges() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP waapi_chain_without_ids_converges: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_waapi_chain_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(15);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "waapi-chain")
            .await
            .expect("a WAAPI chain with no ids must converge")
            .png;
        let second = renderer
            .render_story(&server.base_url(), "waapi-chain")
            .await
            .expect("second waapi-chain capture")
            .png;
        assert_eq!(
            first, second,
            "waapi-chain: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// **strict-CSP のページでも静止が成立する**こと（constructed stylesheet の本丸）。
    ///
    /// 旧実装（`<style>` の appendChild）は CSP `style-src 'self'` に例外なく
    /// 黙殺され、検証層が fail-closed で落とすしかなかった——正当な story が
    /// CSP という理由だけで撮れず、README は利用者へ `unsafe-inline` の追加を
    /// 求めていた。注入を constructed stylesheet（CSSOM）へ移した現在、CSSOM
    /// 操作は CSP style-src の管轄外なので、適用失敗そのものが構造的に起きない。
    ///
    /// 「撮れた」を「効いた」の証明にしないため、二段で確かめる:
    /// 1. 可視キャレット（`caret-color:#cc0000`）の story を 2 回撮って一致
    /// 2. その絵が、同じ CSP 下で `caret-color:transparent` を明示した対照と
    ///    一致する——注入がキャレットを本当に不可視にしたときだけ成立する
    ///
    /// 証明する: `render_story`（freeze 込み）が style CSP 下でも静止 CSS を
    /// 適用し決定的に撮れること。証明しない: CSP が `<style>` 注入を拒むこと
    /// 単体——それは下の positive control が担う。
    #[tokio::test(flavor = "multi_thread")]
    async fn strict_csp_page_freezes_via_the_constructed_stylesheet() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP strict_csp_page_freezes_via_the_constructed_stylesheet: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let visible_dir = tempfile::tempdir().expect("tempdir");
        write_strict_csp_bundle(visible_dir.path(), "#cc0000");
        let visible_server = StaticServer::start(visible_dir.path())
            .await
            .expect("start server");

        let hidden_dir = tempfile::tempdir().expect("tempdir");
        write_strict_csp_bundle(hidden_dir.path(), "transparent");
        let hidden_server = StaticServer::start(hidden_dir.path())
            .await
            .expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&visible_server.base_url(), "strict-csp")
            .await
            .expect(
                "a strict-CSP page is a legitimate story — the constructed \
                 stylesheet must apply the freeze CSS despite style-src 'self'",
            )
            .png;
        let second = renderer
            .render_story(&visible_server.base_url(), "strict-csp")
            .await
            .expect("second strict-csp capture")
            .png;
        assert_eq!(
            first, second,
            "strict-csp: two frozen captures must be byte-identical"
        );

        let hidden = renderer
            .render_story(&hidden_server.base_url(), "strict-csp")
            .await
            .expect("strict-csp capture with an explicitly transparent caret")
            .png;
        assert_eq!(
            first, hidden,
            "under CSP the frozen caret must be indistinguishable from an \
             explicitly transparent caret — otherwise the constructed stylesheet \
             did not actually apply and the identity above proves nothing"
        );

        renderer.close().await;
    }

    /// **positive control（CSP）**: `<style>` の appendChild による注入は CSP に
    /// 拒否されることの直接証拠——注入を constructed stylesheet へ移した理由の固定。
    ///
    /// `Page.setBypassCSP` を呼ばずに `<style>` を注入し、`getComputedStyle` で
    /// CSS が効いていないことを確認する。ここが通らなくなったら（= CSP が
    /// `<style>` 注入を拒まなくなったら）、constructed stylesheet を選んだ根拠が
    /// 崩れているということであり、上の CSP 下静止テストの意味も変わる。
    ///
    /// 証明する: fixture の CSP が `<style>` 注入を本当に拒むこと（`new_page`
    /// 直・手動 evaluate）。証明しない: freeze・`render_story` 経路——この
    /// テストはどちらも通っていない（そちらは上の CSP 下静止テストが担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn strict_csp_blocks_style_element_injection() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP strict_csp_blocks_style_element_injection: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_strict_csp_bundle(dir.path(), "#cc0000");
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 320, 240))
            .await
            .expect("launch");
        let url = format!(
            "{}/iframe.html?id=strict-csp&viewMode=story",
            server.base_url()
        );
        let page = renderer.browser.new_page(&url).await.expect("open page");
        wait_until(&page, "document.readyState !== 'loading'").await;

        // bypass なし: inline style を注入しても CSP に拒否される
        page.evaluate(
            r#"(() => {
              const s = document.createElement('style');
              s.textContent = '* { caret-color: transparent !important; transition-duration: 0s !important; }';
              document.head.appendChild(s);
            })()"#,
        )
        .await
        .expect("inject style");

        let caret = page
            .evaluate("getComputedStyle(document.querySelector('input')).caretColor")
            .await
            .expect("read caret-color");
        assert_ne!(
            caret.value().and_then(|v| v.as_str()),
            Some("transparent"),
            "without bypass, CSP must block the injected caret-color — \
             if this fails, the CSP fixture is broken and the freeze test proves nothing"
        );

        let _ = page.close().await;
        renderer.close().await;
    }

    /// **script-src 'none' のページでは inline script が実行されない**こと。
    ///
    /// `setBypassCSP` は CSP 全体を迂回するため、`script-src 'none'` も
    /// 無効化されて inline script が実行されてしまっていた。bypass を外した
    /// 現在は CSP がそのまま効くため、inline script は実行されない。
    /// fixture は script 実行時に背景を赤に変えるので、緑のままなら不実行。
    ///
    /// 証明する: `new_page` で直接開いたページに bypass 副作用が無いこと。
    /// 証明しない: freeze・`render_story` 経路での CSP 維持——このテストは
    /// どちらも通っていない（旧名 `..._with_freeze_enabled` は虚偽だった）。
    /// そちらは `render_story_with_freeze_keeps_script_csp_enforced` が担う。
    #[tokio::test(flavor = "multi_thread")]
    async fn script_csp_blocks_inline_scripts_on_a_directly_opened_page() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP script_csp_blocks_inline_scripts_on_a_directly_opened_page: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_script_csp_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 320, 240))
            .await
            .expect("launch");

        let url = format!("{}/iframe.html", server.base_url());
        let page = renderer.browser.new_page(&url).await.expect("open page");
        wait_until(&page, "document.readyState !== 'loading'").await;

        let color = page
            .evaluate("getComputedStyle(document.getElementById('box')).backgroundColor")
            .await
            .expect("read background-color");
        assert_eq!(
            color.value().and_then(|v| v.as_str()),
            Some("rgb(0, 255, 0)"),
            "with script-src 'none', the inline script must not execute — \
             the box must stay green. If it turned red, CSP was bypassed"
        );

        let _ = page.close().await;
        renderer.close().await;
    }

    /// **positive control（script-src 'none' の逆方向）**: CSP がなければ
    /// 同じ fixture の inline script は実行されて背景が赤になること。
    ///
    /// 証明する: fixture が「script が動けば赤になる」形で生きていること
    /// （`new_page` 直——freeze は通っていない）。証明しない: CSP の維持。
    #[tokio::test(flavor = "multi_thread")]
    async fn inline_script_runs_without_csp() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP inline_script_runs_without_csp: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        // CSP meta タグなしの同等ページを書く
        std::fs::write(
            dir.path().join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box{width:100%;height:100vh;background:#00ff00}
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  document.getElementById('box').style.background = '#ff0000';
</script>
</body></html>"#,
        )
        .expect("write iframe.html");

        let server = StaticServer::start(dir.path()).await.expect("start server");
        let renderer = StoryRenderer::launch(RenderOptions::new(chromium, 320, 240))
            .await
            .expect("launch");

        let url = format!("{}/iframe.html", server.base_url());
        let page = renderer.browser.new_page(&url).await.expect("open page");
        wait_until(&page, "document.readyState !== 'loading'").await;

        let color = page
            .evaluate("getComputedStyle(document.getElementById('box')).backgroundColor")
            .await
            .expect("read background-color");
        assert_eq!(
            color.value().and_then(|v| v.as_str()),
            Some("rgb(255, 0, 0)"),
            "positive control: without CSP the inline script must run and turn the box red"
        );

        let _ = page.close().await;
        renderer.close().await;
    }

    /// `script-src 'none'` **のみ**（style-src 制約なし）のバンドル。
    ///
    /// [`write_script_csp_bundle`] と違い style-src を課さないのは、freeze の
    /// 静止 CSS 注入を通して**本番経路（`render_story` → FREEZE_SCRIPT →
    /// `freeze_verdict` → 撮影）を最後まで到達させる**ため。script CSP の
    /// 維持だけを分離して観測する。静的 CSS で `#box` は緑、inline script は
    /// 実行されると `#box` を赤へ書き換える。撮った絵が緑のままなら
    /// script は実行されていない（DOM は不変）。
    fn write_script_csp_render_bundle(root: &Path, with_csp: bool) {
        let csp_meta = if with_csp {
            "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'none'\">\n"
        } else {
            ""
        };
        std::fs::write(
            root.join("iframe.html"),
            format!(
                r#"<!doctype html>
<html><head>
{csp_meta}<style>
  html,body{{margin:0;padding:0;background:#fff}}
  #box{{width:100%;height:100vh;background:#00ff00}}
</style>
</head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  document.getElementById('box').style.background = '#ff0000';
</script>
</body></html>"#
            ),
        )
        .expect("write iframe.html");
    }

    /// **freeze を通した script CSP 維持の担保（レビュアーが求めた本丸）**。
    ///
    /// `render_story` は本番と同じく READY 判定 → FREEZE_SCRIPT 注入 →
    /// `freeze_verdict` → 撮影まで進む。その全経路を通ってなお
    /// `script-src 'none'` が効き続け、inline script は実行されない
    /// （DOM は書き換わらず、絵は緑のまま）——`setBypassCSP` のような
    /// CSP を殺す副作用がレンダラのどこにも無いことの直接証拠。
    ///
    /// 証明する: 本番撮影経路（freeze 込み）が script CSP を維持すること。
    /// 証明しない: CSP なしでも緑になる可能性——それは下の positive control が潰す。
    #[tokio::test(flavor = "multi_thread")]
    async fn render_story_with_freeze_keeps_script_csp_enforced() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP render_story_with_freeze_keeps_script_csp_enforced: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_script_csp_render_bundle(dir.path(), true);
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        // script-src 'none' では Storybook ランタイムの script も動かないため、
        // READY 判定は DOM フォールバック経路（SIGNAL_GRACE 後）で成立する。
        let png = renderer
            .render_story(&server.base_url(), "script-csp")
            .await
            .expect(
                "the freeze injects only CSS (style is not restricted here), \
                 so the full production path must reach the screenshot",
            )
            .png;
        renderer.close().await;

        let image = decode_png(&png);
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (0, 255, 0),
            "through the full render_story + freeze path, script-src 'none' must \
             keep the inline script from running — the box must stay green. \
             Red means the renderer bypassed CSP somewhere"
        );
    }

    /// **positive control（上の逆方向）**: CSP を外した同一 fixture を同じ
    /// `render_story` 経路で撮ると、inline script が実行されて赤が出ること。
    ///
    /// 証明する: fixture と撮影経路が「script が動けば赤になる」形で生きている
    /// こと——上の緑が「script が動けない」ことの証拠として成立する前提。
    /// 証明しない: CSP の維持そのもの——それは上のテストが担う。
    #[tokio::test(flavor = "multi_thread")]
    async fn render_story_executes_inline_scripts_without_csp() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP render_story_executes_inline_scripts_without_csp: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_script_csp_render_bundle(dir.path(), false);
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let png = renderer
            .render_story(&server.base_url(), "script-csp")
            .await
            .expect("without CSP the same path must also reach the screenshot")
            .png;
        renderer.close().await;

        let image = decode_png(&png);
        let center = image.get_pixel(160, 120);
        assert_eq!(
            (center[0], center[1], center[2]),
            (255, 0, 0),
            "positive control: without CSP the inline script must run and turn \
             the box red — otherwise the green above proves nothing"
        );
    }

    /// 静止は問題なく済むが、FREEZE_SCRIPT の返り値が JSON にならないページ。
    ///
    /// ページ側で `JSON.stringify` を「`ok` キーを持つオブジェクトのときだけ」
    /// 壊す。FREEZE_SCRIPT は結果全体を `JSON.stringify({ok: ...})` で返すので
    /// 解析不能な文字列になる一方、READY_PROBE の `{state: ...}` は無傷で
    /// 描画完了の検知はそのまま通る。
    fn write_garbled_freeze_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  var origStringify = JSON.stringify.bind(JSON);
  JSON.stringify = function (value) {
    if (value && typeof value === 'object' && 'ok' in value) { return 'not json'; }
    return origStringify.apply(JSON, arguments);
  };
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'garbled'); }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// **positive control（解析不能応答）**: FREEZE_SCRIPT の返り値を読めない
    /// とき、レンダラは撮影せず「解析できなかった」失敗を返すこと。
    ///
    /// 修正前のコード（`ok == Some(false)` のときだけ失敗）では、この
    /// ページは黙って成功として撮影まで通っていた。
    ///
    /// 証明する: `render_story`（freeze 込み）が解析不能な freeze 応答で
    /// 撮らずに失敗すること。証明しない: 静止そのものの成否。
    #[tokio::test(flavor = "multi_thread")]
    async fn garbled_freeze_result_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP garbled_freeze_result_fails_instead_of_silently_succeeding: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_garbled_freeze_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        // フォント条件待ちも同じ JSON.stringify プロトコルを使うため、この
        // garble はまず fonts 層で fail-closed になる（それ自体は
        // `garbled_fonts_result_fails_instead_of_silently_succeeding` が固定）。
        // ここでは freeze 層の verdict を隔離して検証したいので裏口で切る。
        options.wait_for_fonts = false;
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "garbled")
            .await
            .expect_err("an unreadable freeze result must fail — not silently succeed");

        let message = err.to_string();
        assert!(
            message.contains("freeze result was unparseable"),
            "the error must say the freeze result could not be parsed, got {message:?}"
        );
        // 「静止に失敗した」とは別の失敗として届くこと。
        assert!(
            !message.contains("freeze failed"),
            "a parse failure must not masquerade as a freeze failure, got {message:?}"
        );

        renderer.close().await;
    }

    /// **positive control（解析不能応答・fonts 層）**: [`FONTS_WAIT_SCRIPT`] の
    /// 返り値を読めないとき、レンダラは撮影せず「解析できなかった」失敗を
    /// 返すこと（[`fonts_verdict`] の実ブラウザ貫通）。
    ///
    /// バンドルは freeze 用の garbled と同じもの——`ok` キーを持つ
    /// オブジェクトの `JSON.stringify` だけを壊すページは、同じプロトコルを
    /// 使う fonts 層にまず捕まる。
    #[tokio::test(flavor = "multi_thread")]
    async fn garbled_fonts_result_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP garbled_fonts_result_fails_instead_of_silently_succeeding: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_garbled_freeze_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "garbled")
            .await
            .expect_err("an unreadable fonts-wait result must fail — not silently succeed");

        let message = err.to_string();
        assert!(
            message.contains("fonts-ready wait result was unparseable"),
            "the error must say the fonts-wait result could not be parsed, got {message:?}"
        );

        renderer.close().await;
    }

    /// `document.fonts` を FontFaceSet に見えない値へ差し替えるバンドル。
    /// [`FONTS_WAIT_SCRIPT`] の形チェック分岐（失敗経路③）の実ブラウザ検証用。
    ///
    /// garbled 系と違い JSON プロトコルは無傷なので、fonts 層の**形チェック**
    /// そのものが `ok: false` を返し、その `errors` 文言が Rust 側まで届く
    /// ことを貫通で確かめられる。
    ///
    /// `replacement_js` が差し替え後の値の式。形チェックは status の型と
    /// `ready.then` の**両方**を見るため、片方だけ通る値（`status` は文字列
    /// だが `ready.then` が関数でない等）も呼び出し側が変えて検証できる——
    /// `{ status: 42 }` 一種だけでは `ready` 側の条件が一度も貫通しない。
    fn write_fonts_replacing_bundle(root: &Path, replacement_js: &str) {
        std::fs::write(
            root.join("iframe.html"),
            format!(
                r#"<!doctype html>
<html><head><style>
  html,body{{margin:0;padding:0;background:#fff}}
  #box {{ width:100%;height:100vh;background:#00ff00; }}
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  Object.defineProperty(document, 'fonts', {{ value: {replacement_js} }});
  var listeners = {{}};
  var channel = {{
    on: function (event, cb) {{ (listeners[event] = listeners[event] || []).push(cb); }},
    emit: function (event, payload) {{
      (listeners[event] || []).forEach(function (cb) {{ cb(payload); }});
    }}
  }};
  setTimeout(function () {{
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {{ channel.emit('storyRendered', 'fonts-replaced'); }}, 20);
  }}, 20);
</script>
</body></html>"#
            ),
        )
        .expect("write iframe.html");
    }

    /// 【cmd_658・失敗経路③の実ブラウザ貫通】`document.fonts` が FontFaceSet に
    /// 見えない形へ差し替えられたページは、**待たずに・撮らずに**
    /// [`RenderError::Story`] で落ち、[`FONTS_WAIT_SCRIPT`] の形チェックが積んだ
    /// `errors` の文言が Rust 側の失敗メッセージまで届くこと。
    ///
    /// 単体の `fonts_verdict_*` 群は手書き JSON への受理条件しか固定しない——
    /// スクリプト側の形チェックを実行時に貫通するのはこの試験だけである。
    /// 形チェックを削る・`ok: true` へ倒す変更はこの試験を落とす。
    ///
    /// 差し替えは二通り: `status` が文字列でない形と、`status` は文字列だが
    /// `ready.then` が関数でない形。前者だけでは形チェックの `ready` 側の
    /// 条件（`typeof fonts.ready.then !== 'function'`）が一度も貫通しない
    /// ——`{status: 42}` は最初の条件で落ち、`ready` の検査へ到達しない。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_that_replaces_document_fonts_fails_the_shape_check() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP a_page_that_replaces_document_fonts_fails_the_shape_check: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        for (label, replacement_js) in [
            ("non-string status", "{ status: 42 }"),
            (
                "non-function ready.then",
                "{ status: 'loaded', ready: { then: 42 } }",
            ),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            write_fonts_replacing_bundle(dir.path(), replacement_js);
            let server = StaticServer::start(dir.path()).await.expect("start server");

            let mut options = RenderOptions::new(chromium.clone(), 320, 240);
            options.story_timeout = Duration::from_secs(10);
            let renderer = StoryRenderer::launch(options).await.expect("launch");

            let err = renderer
                .render_story(&server.base_url(), "fonts-replaced")
                .await
                .expect_err(&format!(
                    "{label}: a non-FontFaceSet document.fonts must fail — not wait, not capture"
                ));
            renderer.close().await;

            match err {
                RenderError::Story { message, .. } => {
                    assert!(
                        message.contains("fonts were not verified as loaded"),
                        "{label}: the failure must come from the fonts verdict, got: {message}"
                    );
                    assert!(
                        message.contains("does not look like a FontFaceSet"),
                        "{label}: the shape-check diagnostics must reach the message, got: {message}"
                    );
                }
                other => panic!("{label}: expected a story-scoped fonts failure, got: {other:?}"),
            }
        }
    }

    /// 検証層自身を失敗させるバンドル。preview の iframe（iframe.html）の
    /// main world で `window.getComputedStyle` を throw する関数へ差し替える。
    /// [`FREEZE_SCRIPT`] は同じ main world で評価されるため、CSS 適用検証の
    /// `getComputedStyle(el)` 呼び出しがこの差し替えを踏んで throw する。
    fn write_throwing_verification_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head><style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  window.getComputedStyle = function () {
    throw new Error('getComputedStyle is broken on this page');
  };
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'throwing'); }, 20);
  }, 20);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// **検証層の失敗の fail-closed**: CSS 適用検証の `getComputedStyle` が
    /// throw するページで、レンダラは撮影せず失敗を返すこと。
    ///
    /// 修正前のコード（検証の catch が `errors` に積むだけで `ok: true` へ
    /// 到達する）では、このページは freeze 失敗にならず PNG 取得まで黙って
    /// 成功していた（positive control として実測済み）。レビュアーが
    /// preview の iframe（iframe.html）内で `window.getComputedStyle` を
    /// throw する関数へ差し替えて再現したのと同じ形である。
    ///
    /// 証明する: 検証層自身の失敗が fail-closed に倒れ、集めた `errors` が
    /// 判定に使われること。証明しない: 静止そのもの・CSS 適用の成否。
    #[tokio::test(flavor = "multi_thread")]
    async fn throwing_verification_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP throwing_verification_fails_instead_of_silently_succeeding: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_throwing_verification_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "throwing")
            .await
            .expect_err("a failing verification layer must fail — not silently capture a PNG");

        let message = err.to_string();
        assert!(
            message.contains("freeze failed"),
            "the error must go through the freeze-failure path, got {message:?}"
        );
        assert!(
            message.contains("CSS verification failed"),
            "the error must carry the collected verification error, got {message:?}"
        );

        renderer.close().await;
    }

    /// scroll timeline の対照群: freeze なしでは二回撮りが一致しないこと。
    ///
    /// scroll timeline のアニメーションが running のまま残っている場合、
    /// 撮影タイミングで絵が変わり得る。
    ///
    /// 証明する: freeze の有無どちらの `render_story` 経路も撮影まで通ること。
    /// 証明しない: 差分の存在（末尾コメントの通り、主証拠は一致テスト側）。
    #[tokio::test(flavor = "multi_thread")]
    async fn unfrozen_scroll_timeline_captures_may_differ() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP unfrozen_scroll_timeline_captures_may_differ: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_scroll_timeline_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut frozen_options = RenderOptions::new(chromium.clone(), 320, 240);
        frozen_options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(frozen_options).await.expect("launch");
        let frozen = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("frozen capture")
            .png;
        renderer.close().await;

        let mut unfrozen_options = RenderOptions::new(chromium, 320, 240);
        unfrozen_options.story_timeout = Duration::from_secs(10);
        unfrozen_options.freeze_before_capture = false;
        let renderer = StoryRenderer::launch(unfrozen_options)
            .await
            .expect("launch unfrozen");
        let unfrozen = renderer
            .render_story(&server.base_url(), "scroll-timeline")
            .await
            .expect("unfrozen capture")
            .png;
        renderer.close().await;

        // scroll timeline は scroll 位置に連動するため freeze の有無で
        // 異なる座標に確定する可能性がある（確定的に差が出るとは限らないが、
        // 少なくとも freeze が通っていることの追加証拠になる）。
        // この対照テストは「freeze が何かをしている」ことの確認であり、
        // 主証拠は二回撮り一致テスト側にある。
        let _ = (frozen, unfrozen); // 使用済みの証拠
    }

    /// rAF を不発火にするバンドル。FREEZE_SCRIPT の `nextFrame` は
    /// `requestAnimationFrame` を 2 回待つ promise なので、これが永遠に
    /// 解決せず `page.evaluate(FREEZE_SCRIPT)` 自体が返らなくなる。
    /// READY 判定はチャンネル経由（rAF 不使用）なので通常どおり通る。
    ///
    /// コールバックは**保持する**（呼ばないだけ）。捨てると resolve 関数が
    /// どこからも参照されず、V8 が pending promise ごと GC で回収して
    /// 約 30 秒後に `Error -32000: Promise was collected` という CDP エラーで
    /// 返ってきてしまう（実測）。保持すれば promise は生き続け、evaluate は
    /// 本当に永遠に返らない——ポーズ中の rAF をキューに積む実ページと同型。
    ///
    /// `ready_delay_ms` は `storyRendered` を撃つまでの遅延。READY 待ちに
    /// story 予算の一部を意図的に消費させ、FREEZE evaluate へ渡る残余が
    /// 縮むこと（deadline の共有）を実測するために使う。
    fn write_raf_suppressed_bundle(root: &Path, ready_delay_ms: u64) {
        std::fs::write(
            root.join("iframe.html"),
            format!(
                r#"<!doctype html>
<html><head><style>
  html,body{{margin:0;padding:0;background:#fff}}
  #box {{ width:100%;height:100vh;background:#00ff00; }}
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  // rAF を握りつぶす。コールバックは保持するが永遠に呼ばない。
  window.__rafCallbacks = [];
  window.requestAnimationFrame = function (cb) {{
    window.__rafCallbacks.push(cb);
    return window.__rafCallbacks.length;
  }};
  var listeners = {{}};
  var channel = {{
    on: function (event, cb) {{ (listeners[event] = listeners[event] || []).push(cb); }},
    emit: function (event, payload) {{
      (listeners[event] || []).forEach(function (cb) {{ cb(payload); }});
    }}
  }};
  setTimeout(function () {{
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () {{ channel.emit('storyRendered', 'raf-suppressed'); }}, {ready_delay_ms});
  }}, 20);
</script>
</body></html>"#
            ),
        )
        .expect("write iframe.html");
    }

    /// **freeze の停止性**: rAF が発火しないページでもハングせず、
    /// `story_timeout` 内に失敗が返ること。
    ///
    /// 修正前は `page.evaluate(FREEZE_SCRIPT)` の promise が解決せず
    /// `render_story` が返らなかった（停止性が上位の CI ジョブタイムアウト
    /// 頼みだった——層ごとの失敗経路表「収束反復」行の穴）。テスト自身にも
    /// 外周の時間上限を置き、修正が外れた場合はハングでなくこの上限で
    /// 落ちるようにする。
    ///
    /// 証明する: `render_story`（freeze 込み）が rAF 不発火ページで
    /// `story_timeout` 内に失敗を返すこと。証明しない: 静止そのものの成否。
    #[tokio::test(flavor = "multi_thread")]
    async fn raf_suppressed_page_fails_within_the_story_timeout() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP raf_suppressed_page_fails_within_the_story_timeout: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_raf_suppressed_bundle(dir.path(), 20);
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(5);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        // 外周上限は story_timeout より十分大きく取る。ここで落ちたら
        // freeze に時間上限が無い（= render_story がハングする）ということ。
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            renderer.render_story(&server.base_url(), "raf-suppressed"),
        )
        .await
        .expect(
            "render_story must return within the outer bound — hanging here means \
             the freeze evaluate has no timeout of its own",
        );

        let err = result.expect_err("a page that never fires rAF must fail — not hang or succeed");
        let message = err.to_string();
        assert!(
            message.contains("freeze did not finish"),
            "the error must say the freeze timed out, got {message:?}"
        );

        renderer.close().await;
    }

    /// **FREEZE evaluate は READY 待ちと同じ deadline を分け合う**こと
    /// （独立予算による timeout 二重取りの回帰テスト）。
    ///
    /// READY まで約 5 秒かかり、かつ rAF を発火させないページ。story_timeout
    /// = 10 秒のとき、freeze へ渡るのは残余（約 5 秒 − SETTLE_DELAY）であり、
    /// 1 story の総所要は約 story_timeout + SETTLE_DELAY に収まる。修正前
    /// （freeze に story_timeout をフル予算で与え直す）は約 5 + 10 = 15 秒超
    /// かかっていた——13 秒の上限 assert がその再発を検知する。
    ///
    /// 証明する: freeze の時間上限が `started + story_timeout` の残余である
    /// こと、および時間切れが READY 側と同じ [`RenderError::Timeout`] 分類で
    /// 報告されること。証明しない: freeze の停止性そのもの（上の
    /// `raf_suppressed_page_fails_within_the_story_timeout` が担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn freeze_timeout_shares_the_story_deadline_with_the_ready_wait() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP freeze_timeout_shares_the_story_deadline_with_the_ready_wait: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_raf_suppressed_bundle(dir.path(), 5000);
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let started = std::time::Instant::now();
        let err = renderer
            .render_story(&server.base_url(), "raf-suppressed")
            .await
            .expect_err("the freeze cannot finish on this page, so the story must fail");
        let elapsed = started.elapsed();
        renderer.close().await;

        // 実測の要: 独立予算なら約 15 秒超（5s READY + 10s freeze）、残余なら
        // 約 10 秒強で返る。CI のゆらぎぶんの余裕を見て 13 秒を境にする。
        assert!(
            elapsed < Duration::from_secs(13),
            "the story must fail within roughly one story_timeout — \
             {elapsed:?} suggests the freeze was given a fresh full budget \
             instead of the remainder of the shared deadline"
        );
        assert!(
            matches!(err, RenderError::Timeout { .. }),
            "running out of the story budget must be classified as a timeout \
             (same as the READY wait), got {err:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("freeze did not finish"),
            "the timeout must still say which phase ran out, got {message:?}"
        );
    }

    /// rAF を握りつぶし（コールバック保持）、かつ一定周期で `location.reload()`
    /// するバンドル。freeze evaluate は rAF 待ちで pending のまま reload を
    /// 迎え、「Inspected target navigated or closed」（-32000。context 破棄系）
    /// の CDP エラーで返る（本モジュールのテストを旧実装に当てた実測）。
    /// reload は毎回同じページを読み直すので、リトライしても freeze は
    /// 完了できず、story の deadline まで同じ形が繰り返される。
    ///
    /// rAF 抑止はインライン `<head>` スクリプトで行う——リトライの evaluate が
    /// 新しい document に入る時点では必ず実行済みであり、「抑止前の一瞬に
    /// freeze が滑り込んで成功する」窓を実質的に残さない（リトライは CDP
    /// エラー後に [`POLL_INTERVAL`] 眠ってから入るため、この小ページの
    /// head 解析より遅い）。
    fn write_reloading_freeze_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head>
<script>
  // rAF を握りつぶす（コールバックは保持——捨てると Promise ごと GC され
  // 「Promise was collected」という別経路になる。そちらは
  // write_collected_freeze_bundle が担う）。
  window.__rafCallbacks = [];
  window.requestAnimationFrame = function (cb) {
    window.__rafCallbacks.push(cb);
    return window.__rafCallbacks.length;
  };
</script>
<style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'reloading'); }, 20);
  }, 20);
  // READY → settle(250ms) → freeze evaluate（rAF 待ちで pending）の後に
  // 実行コンテキストごと破壊する。reload 後も同じページなので周期的に繰り返す。
  setTimeout(function () { location.reload(); }, 800);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// **freeze 中の navigation / reload は story 単位の失敗である**こと
    /// （cmd_632 ①経路 1。実測のエラーは
    /// 「Inspected target navigated or closed」）。
    ///
    /// 修正前は freeze evaluate の CDP エラーを即 [`RenderError::Cdp`] に
    /// 倒していた——`render_build::is_story_scoped` は Cdp を環境分類とする
    /// ため、reload する story が 1 件あるだけで `render_all` の `?` 相当の
    /// 即中断となり、残り全 story の撮影が巻き添えで失われた。READY probe は
    /// 同種のエラーを deadline までリトライするのに freeze だけ即中断という
    /// 非対称でもあった。修正後は deadline までリトライし、期限で READY 側と
    /// 同じ [`RenderError::Timeout`]——`is_story_scoped` が story 分類とする
    /// variant——で返る（分類は render_build 側の
    /// `story_scoped_failures_are_isolated_and_infrastructure_failures_are_not`
    /// が固定している）。
    ///
    /// 証明する: reload し続けるページで `render_story` が [`RenderError::Timeout`]
    /// （story 分類）を返し、[`RenderError::Cdp`]（環境分類＝ビルド即中断）に
    /// ならないこと。証明しない: `render_all` のループが実際に continue する
    /// こと（分類の match 構造が保証。render_build 側テストの但し書きと同じ）。
    #[tokio::test(flavor = "multi_thread")]
    async fn reloading_page_during_freeze_fails_story_scoped() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP reloading_page_during_freeze_fails_story_scoped: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_reloading_freeze_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(5);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        // 外周上限: ここで落ちたらリトライが deadline を見ていない（無限リトライ）。
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            renderer.render_story(&server.base_url(), "reloading"),
        )
        .await
        .expect("render_story must return within the outer bound");
        renderer.close().await;

        let err = result.expect_err("a page that reloads during the freeze must fail");
        let RenderError::Timeout { phase, .. } = err else {
            panic!(
                "a story that destroys the freeze evaluate by navigating must be \
                 classified story-scoped (Timeout), not as an infrastructure Cdp \
                 error that aborts the whole build, got {err:?}"
            );
        };
        // 機構は reduced-motion 検証と共有していても、名乗る段は freeze のまま
        // であること（どちらの freeze phase で終わるかは最後の一回が pending の
        // まま期限を迎えたかで変わるので、経路だけを固定する）。
        assert!(
            phase.contains("freeze") && !phase.contains("reduced-motion"),
            "the failing stage must be reported as the freeze one, got {phase:?}"
        );
    }

    /// rAF コールバックを**捨てる**（保持しない）バンドル。freeze の pending
    /// promise は resolve 関数がどこからも参照されなくなり、V8 の GC が
    /// promise ごと回収して evaluate が「Error -32000: Promise was collected」
    /// で返る（cmd_630 の実測）。素の状態では回収まで約 30 秒かかるので、
    /// 大きな割り当てを捨て続けるループで GC を意図的に急がせる。
    fn write_collected_freeze_bundle(root: &Path) {
        std::fs::write(
            root.join("iframe.html"),
            r#"<!doctype html>
<html><head>
<script>
  // rAF コールバックを捨てる——resolve への参照が消え、pending promise が
  // GC 対象になる（保持する write_raf_suppressed_bundle との違いがこの一点）。
  window.requestAnimationFrame = function (cb) { return 0; };
</script>
<style>
  html,body{margin:0;padding:0;background:#fff}
  #box { width:100%;height:100vh;background:#00ff00; }
</style></head>
<body><div id="storybook-root"><div id="box"></div></div>
<script>
  var listeners = {};
  var channel = {
    on: function (event, cb) { (listeners[event] = listeners[event] || []).push(cb); },
    emit: function (event, payload) {
      (listeners[event] || []).forEach(function (cb) { cb(payload); });
    }
  };
  setTimeout(function () {
    window.__STORYBOOK_ADDONS_CHANNEL__ = channel;
    setTimeout(function () { channel.emit('storyRendered', 'collected'); }, 20);
  }, 20);
  // GC 圧: 参照を残さない大きな割り当てを繰り返し、major GC を数秒内に誘発
  // させる（pending promise の回収を約 30 秒から数秒へ縮める）。
  setInterval(function () {
    var garbage = [];
    for (var i = 0; i < 50; i++) garbage.push(new Array(100000).fill(Math.random()));
    window.__gcTicks = (window.__gcTicks || 0) + 1;
  }, 100);
</script>
</body></html>"#,
        )
        .expect("write iframe.html");
    }

    /// **pending promise の GC 回収も story 単位の失敗である**こと
    /// （cmd_632 ①経路 2: 「Promise was collected」——cmd_630 で
    /// `write_raf_suppressed_bundle` に実測として書き残した挙動が、
    /// そのまま「freeze evaluate の CDP エラー＝ビルド即中断」の穴を指した）。
    ///
    /// 修正前はこの CDP エラーが即 [`RenderError::Cdp`]（環境分類）となり
    /// ビルド全体を中断した。修正後はリトライして deadline で
    /// [`RenderError::Timeout`]（story 分類）に倒れる。
    ///
    /// 証明する: rAF コールバックを捨てるページで `render_story` が
    /// [`RenderError::Timeout`] を返し [`RenderError::Cdp`] にならないこと。
    /// 但し書き: 回収のタイミングは GC 依存で、GC 圧をかけても deadline 内に
    /// 回収が起きない可能性は残る——その場合は共有 deadline の時間切れで同じ
    /// Timeout に落ち、テストは通る（CDP エラー経路の決定的な固定は
    /// `reloading_page_during_freeze_fails_story_scoped` が担う。こちらは
    /// cmd_630 実測のエラー文言まで含めた第二経路の記録である）。
    #[tokio::test(flavor = "multi_thread")]
    async fn collected_freeze_promise_fails_story_scoped() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP collected_freeze_promise_fails_story_scoped: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_collected_freeze_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(8);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let result = tokio::time::timeout(
            Duration::from_secs(60),
            renderer.render_story(&server.base_url(), "collected"),
        )
        .await
        .expect("render_story must return within the outer bound");
        renderer.close().await;

        let err = result.expect_err("a page that discards rAF callbacks must fail");
        let RenderError::Timeout { phase, .. } = err else {
            panic!(
                "a story whose freeze promise is garbage-collected must be \
                 classified story-scoped (Timeout), not as an infrastructure Cdp \
                 error that aborts the whole build, got {err:?}"
            );
        };
        assert!(
            phase.contains("freeze") && !phase.contains("reduced-motion"),
            "the failing stage must be reported as the freeze one, got {phase:?}"
        );
    }

    /// **注入層自身の失敗の fail-closed**: `CSSStyleSheet` コンストラクタが
    /// 無い環境では、静止 CSS を注入する口が無く「注入できたか不明」のまま
    /// 進むことになるため、黙って成功へ倒さず失敗が返ること。
    ///
    /// constructed stylesheet への移行で生まれた新しい層（sheet 構築・adopt）
    /// にも、他の層と同じく「その層自身が失敗したときどう倒れるか」を数えて
    /// おく——構築不能は errors → `ok: false`（モジュール先頭の
    /// 「層ごとの失敗経路」表の静止 CSS 注入行）。
    ///
    /// 証明する: sheet を構築できない環境で `render_story` が撮らずに失敗する
    /// こと。証明しない: 通常環境で sheet が効くこと（CSP 下静止テストと
    /// frozen_* 系が担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn missing_constructed_stylesheet_api_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP missing_constructed_stylesheet_api_fails_instead_of_silently_succeeding: \
                 no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        // FREEZE_SCRIPT は main world で評価されるので、同じ realm の
        // コンストラクタを消せば「構築の口が無い環境」を再現できる。
        write_story_html(
            dir.path(),
            "  html,body{margin:0;padding:0;background:#fff}\n  #box { width:100%;height:100vh;background:#00ff00; }",
            r#"<div id="box"></div>"#,
            r#"      delete window.CSSStyleSheet;
      channel.emit('storyRendered', 'no-constructed-stylesheet');"#,
        );
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "no-constructed-stylesheet")
            .await
            .expect_err(
                "with no way to construct the freeze stylesheet, the story must \
                 fail — not silently capture an unfrozen page",
            );

        let message = err.to_string();
        assert!(
            message.contains("CSSStyleSheet constructor missing"),
            "the error must name the missing injection API, got {message:?}"
        );

        renderer.close().await;
    }

    /// open shadow root の中の**擬似要素**に無限アニメを持つバンドル。
    ///
    /// `::before` のアニメは `Element.getAnimations()`（オプション無し）では
    /// 返らず、root 側の `DocumentOrShadowRoot.getAnimations()` だけが数えられる。
    /// `break_api: true` は `Document` / `ShadowRoot` 両 prototype から
    /// `getAnimations` を消し、「root 側 API が存在しない環境」を再現する——
    /// 旧 collectRunning はこれを黙って `[]` へ倒し、走り続ける ::before の
    /// アニメを見逃して成功として撮っていた（fail-open）。
    fn write_shadow_pseudo_animation_bundle(root: &Path, break_api: bool) {
        // sabotage は FREEZE_SCRIPT の evaluate（storyRendered 後）より前に
        // 走ればよいので、チャンネル代入後のフックで消して同じ状況を作れる。
        let sabotage = if break_api {
            "      delete Document.prototype.getAnimations;\n      delete ShadowRoot.prototype.getAnimations;\n"
        } else {
            ""
        };
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  x-pseudo { display:block; }"#,
            "",
            &format!(
                r#"{sabotage}      var host = document.createElement('x-pseudo');
      var sr = host.attachShadow({{ mode: 'open' }});
      sr.innerHTML =
        '<style>' +
        '@keyframes sh-pulse {{ from {{ opacity:1; }} to {{ opacity:0.2; }} }}' +
        '.pulse::before {{ content:""; display:block; width:80px; height:80px;' +
        ' background:#ff0000; animation: sh-pulse 1.3s linear infinite; }}' +
        '</style><div class="pulse"></div>';
      document.getElementById('storybook-root').appendChild(host);
      channel.emit('storyRendered', 'shadow-pseudo');"#
            ),
        );
    }

    /// **positive control（shadow 内 running 見逃し）**: root 側の
    /// `getAnimations` API が無い環境では、擬似要素のアニメを数える口が無く
    /// 網羅を保証できないため、黙って成功へ倒さず失敗が返ること。
    ///
    /// 修正前の collectRunning は `typeof r.getAnimations === 'function'` で
    /// なければ黙って `[]` を返し、shadow 内で走り続ける `::before` の無限
    /// アニメを「残っていない」と誤認して flaky な絵を成功として撮っていた。
    ///
    /// 証明する: 収集 API の欠落が errors → `ok: false` へ倒れること。
    /// 証明しない: この fixture のアニメが通常環境で凍ること（下の対が担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn missing_root_getanimations_fails_instead_of_silently_succeeding() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP missing_root_getanimations_fails_instead_of_silently_succeeding: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_pseudo_animation_bundle(dir.path(), true);
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let err = renderer
            .render_story(&server.base_url(), "shadow-pseudo")
            .await
            .expect_err(
                "with the root-level getAnimations API missing, coverage cannot be \
                 verified — the freeze must fail, not silently succeed",
            );

        let message = err.to_string();
        assert!(
            message.contains("getAnimations API missing"),
            "the error must name the missing collection API, got {message:?}"
        );

        renderer.close().await;
    }

    /// **上の対（API が揃った通常環境）**: 同じ shadow 内擬似要素の無限アニメが
    /// root 側 `getAnimations` 経由で数えられ、凍って二回撮り一致すること。
    ///
    /// 証明する: fixture のアニメが「root 側 API でしか数えられない形」で
    /// 生きており、API があれば通常どおり静止できること——上の失敗が
    /// 「API 欠落の検知」であって fixture の壊れではないことの裏づけ。
    #[tokio::test(flavor = "multi_thread")]
    async fn shadow_pseudo_animation_freezes_when_the_api_is_present() {
        let Some(chromium) = discover_chromium() else {
            eprintln!("SKIP shadow_pseudo_animation_freezes_when_the_api_is_present: no chromium");
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_shadow_pseudo_animation_bundle(dir.path(), false);
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "shadow-pseudo")
            .await
            .expect("with the API present, the shadow pseudo-element animation must freeze")
            .png;
        let second = renderer
            .render_story(&server.base_url(), "shadow-pseudo")
            .await
            .expect("second shadow-pseudo capture")
            .png;
        assert_eq!(
            first, second,
            "shadow-pseudo: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// **無限** iteration の progress-based timeline を持つバンドル。
    ///
    /// `animation: ... infinite` + `animation-timeline: scroll()` では
    /// `endTime` が無限になり [`FREEZE_SCRIPT`] の `finiteEnd` は null を返す。
    /// 巻き戻し先の 0 も数値のままでは `TypeError`（progress-based timeline の
    /// `currentTime` は `CSSNumericValue` のみ受理）——修正前は errors 経由で
    /// `ok: false` となり、この**正当な** story を落としていた（過剰拒否）。
    fn write_infinite_scroll_timeline_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  #scroller { width:100%;height:200px;overflow-y:scroll; }
  #content { height:1000px; }
  @keyframes scroll-fade { from { opacity:1; } to { opacity:0; } }
  #target {
    width:100px;height:100px;background:#ff0000;
    animation: scroll-fade linear infinite;
    animation-timeline: scroll(nearest block);
  }"#,
            r#"
  <div id="scroller">
    <div id="target"></div>
    <div id="content"></div>
  </div>
"#,
            "      channel.emit('storyRendered', 'infinite-scroll-timeline');",
        );
    }

    /// infinite + progress-based timeline の正当な story が落とされないこと
    /// （型を保った 0 巻き戻しの回帰テスト）。
    ///
    /// 修正前は数値 0 の代入が `TypeError` → errors → `ok: false` で、
    /// この正当な story を撮れなかった。`CSSUnitValue(0, unit)` で型を
    /// 保って巻き戻すことで pause まで到達し、二回撮りが一致する。
    ///
    /// 証明する: `render_story`（freeze 込み）が infinite な progress-based
    /// timeline でも成功し決定的に撮れること。証明しない: 有限 progress-based
    /// の終端シーク（`frozen_scroll_timeline_captures_are_byte_identical` が担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_infinite_scroll_timeline_captures_are_byte_identical() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP frozen_infinite_scroll_timeline_captures_are_byte_identical: no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_infinite_scroll_timeline_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        let first = renderer
            .render_story(&server.base_url(), "infinite-scroll-timeline")
            .await
            .expect(
                "an infinite progress-based animation is a legitimate story — \
                 the typed zero rewind must let it freeze instead of TypeError-ing",
            )
            .png;
        let second = renderer
            .render_story(&server.base_url(), "infinite-scroll-timeline")
            .await
            .expect("second infinite-scroll-timeline capture")
            .png;
        assert_eq!(
            first, second,
            "infinite scroll timeline: two frozen captures must be byte-identical"
        );

        renderer.close().await;
    }

    /// reduced-motion を尊重して見た目が変わるバンドル。エミュレーション検証用。
    ///
    /// - `demo-rm--box` : 上半分は **CSS メディアクエリ**で色が変わる
    ///   （通常 赤 / reduce 青）。下半分は **JS が描画時に一度だけ
    ///   `matchMedia` を読んで**色を決める（通常 黄 / reduce 緑）。
    ///   JS 側はエミュレーションが**ナビゲーション前に**効いていることの
    ///   証明になる——撮影直前に設定したのでは、描画時に読んだ値は
    ///   通常のままで黄が出る
    /// - `demo-rm--mocked` : `window.matchMedia` を「常に matches: false」の
    ///   モックへ差し替えるページ（polyfill / テストダブルの事故を模す）。
    ///   エミュレーション有効時は検証の matchMedia 輪が不成立になり、
    ///   fail-closed で落ちるべき対象
    fn write_reduced_motion_bundle(root: &Path) {
        write_story_html(
            root,
            r#"  html,body{margin:0;padding:0;background:#fff}
  .css-box { width:100%; height:50vh; background:#ff0000; }
  @media (prefers-reduced-motion: reduce) { .css-box { background:#0000ff; } }"#,
            "",
            r#"      var root = document.getElementById('storybook-root');
      if (id === 'demo-rm--blocking') {
        // reduced-motion プローブが最初に触る API を、返ってこない構築子へ
        // 差し替える（story の描画そのものは触らない）。READY 判定は先に
        // 済むので、止まるのは検証の evaluate だけ——「verdict を返さない」
        // 段を決定的に作れる。
        window.CSSStyleSheet = function () {
          var end = Date.now() + 15000;
          while (Date.now() < end) {}
        };
      }
      if (id === 'demo-rm--mocked') {
        window.matchMedia = function () {
          return { matches: false, media: '',
                   addEventListener: function () {}, removeEventListener: function () {} };
        };
      }
      var cssBox = document.createElement('div');
      cssBox.className = 'css-box';
      root.appendChild(cssBox);
      var jsBox = document.createElement('div');
      jsBox.style.width = '100%';
      jsBox.style.height = '50vh';
      jsBox.style.background =
        window.matchMedia('(prefers-reduced-motion: reduce)').matches
          ? '#00ff00' : '#ffff00';
      root.appendChild(jsBox);
      channel.emit('storyRendered', id);"#,
        );
    }

    /// `ok === true` と確かめられたときだけ成功。それ以外——`ok: false`・
    /// 解析不能——はすべて失敗（fail-closed）。[`freeze_verdict`] と同じ
    /// 受理条件を reduced-motion 検証にも要求する。
    ///
    /// 証明する: `reduced_motion_verdict` の受理条件のみ。証明しない:
    /// 実ブラウザでプローブがこの形の JSON を返すこと（実ブラウザ系が担う）。
    #[test]
    fn reduced_motion_verdict_accepts_only_a_verified_ok_true() {
        let json = |raw: &str| serde_json::Value::String(raw.to_string());

        assert!(reduced_motion_verdict(Some(&json(r#"{"ok":true}"#)), "s").is_ok());
        // 将来プローブの戻り値を拡張しても ok:true なら通る。
        assert!(
            reduced_motion_verdict(Some(&json(r#"{"ok":true,"errors":[],"extra":1}"#)), "s")
                .is_ok()
        );

        // ok:false は「効いていると確かめられなかった」。errors が原因ごと載る。
        let message = reduced_motion_verdict(
            Some(&json(
                r#"{"ok":false,"errors":["matchMedia does not report reduce"]}"#,
            )),
            "s",
        )
        .expect_err("ok:false must fail")
        .to_string();
        assert!(
            message.contains("could not be verified")
                && message.contains("matchMedia does not report reduce"),
            "the failure must carry the probe's diagnostics, got {message:?}"
        );

        // 解析不能はすべて「効いているか不明」の失敗。成功へ倒さない。
        let unparseable: Vec<(&str, Option<serde_json::Value>)> = vec![
            ("evaluate returned no value", None),
            ("non-string value", Some(serde_json::json!(42))),
            ("non-JSON string", Some(json("not json"))),
            ("missing ok key", Some(json(r#"{"errors":[]}"#))),
            ("non-boolean ok", Some(json(r#"{"ok":"true"}"#))),
        ];
        for (label, value) in &unparseable {
            let message = reduced_motion_verdict(value.as_ref(), "s")
                .expect_err(&format!("{label}: must fail"))
                .to_string();
            assert!(
                message.contains("unparseable"),
                "{label}: the error must say the result could not be parsed, got {message:?}"
            );
        }
    }

    /// **positive control**: reduced-motion エミュレーションが絵を実際に
    /// 変えること、そして決定的であること。
    ///
    /// - OFF: CSS 輪は赤・JS 輪は黄（fixture が動いていることの対照——
    ///   ここで色が出なければ、ON の検証は何も証明しない）
    /// - ON: CSS 輪は青（メディアクエリが CSS カスケードに効いた）・
    ///   JS 輪は緑（**描画時に一度だけ読む** `matchMedia` にも見えた =
    ///   ナビゲーション前の適用が効いている）
    /// - ON の二回撮りはバイト一致（決定性）・ON と OFF の絵は異なる
    ///
    /// 証明する: エミュレーションの適用・検証・配線が経路全体として効くこと
    /// （「設定が有効なのに呼び出しが漏れる」経路をこのテストが貫通して固定）。
    /// 証明しない: 適用に失敗したとき落ちること（下の mocked テストが担う）。
    #[tokio::test(flavor = "multi_thread")]
    async fn reduced_motion_emulation_changes_the_picture_and_is_deterministic() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP reduced_motion_emulation_changes_the_picture_and_is_deterministic: \
                 no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_reduced_motion_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);

        // OFF（既定）: 通常の絵。fixture が本当に分岐を描いていることの対照。
        let renderer = StoryRenderer::launch(options.clone())
            .await
            .expect("launch (emulation off)");
        let off = renderer
            .render_story(&server.base_url(), "demo-rm--box")
            .await
            .expect("capture without emulation")
            .png;
        renderer.close().await;

        let image = decode_png(&off);
        let css = image.get_pixel(160, 60);
        let js = image.get_pixel(160, 180);
        assert_eq!(
            (css[0], css[1], css[2]),
            (255, 0, 0),
            "without emulation the CSS branch must be the no-preference color"
        );
        assert_eq!(
            (js[0], js[1], js[2]),
            (255, 255, 0),
            "without emulation the render-time matchMedia read must be no-preference"
        );

        // ON: reduce の絵。二回撮って決定性も確かめる。
        options.emulate_reduced_motion = true;
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch (emulation on)");
        let first = renderer
            .render_story(&server.base_url(), "demo-rm--box")
            .await
            .expect("first capture with emulation")
            .png;
        let second = renderer
            .render_story(&server.base_url(), "demo-rm--box")
            .await
            .expect("second capture with emulation")
            .png;
        renderer.close().await;

        assert_eq!(
            first, second,
            "two captures under reduced-motion emulation must be byte-identical"
        );
        assert_ne!(
            first, off,
            "the emulated capture must differ from the non-emulated one — \
             otherwise the emulation changed nothing and the switch is a no-op"
        );

        let image = decode_png(&first);
        let css = image.get_pixel(160, 60);
        let js = image.get_pixel(160, 180);
        assert_eq!(
            (css[0], css[1], css[2]),
            (0, 0, 255),
            "the reduce media query must reach the CSS cascade"
        );
        assert_eq!(
            (js[0], js[1], js[2]),
            (0, 255, 0),
            "a render-time matchMedia read must see reduce — the emulation must be \
             applied before navigation, not right before the capture"
        );
    }

    /// **fail-closed**: エミュレーションを要求したのに「効いている」と
    /// 確かめられないページでは、撮らずに失敗すること。
    ///
    /// fixture は `window.matchMedia` を「常に matches: false」のモックへ
    /// 差し替える（polyfill / テストダブルの事故を模す）。CSS 輪は効いて
    /// いるが matchMedia 輪が不成立——JS 実装には reduce が見えないまま
    /// 撮ることになるので、fail-closed で落とす。**検証を入れる前の実装
    /// （setEmulatedMedia を呼ぶだけ）はこのページを成功として撮っていた**
    /// ——このテストが落ちなくなったら、検証が判定から外れている。
    ///
    /// 対照: エミュレーションを**要求していない** project では、同じページが
    /// 成功する——モックされた matchMedia は「reduce を要求した」ときにだけ
    /// 失敗になる（検証は要求の裏取りであり、ページの行儀の監査ではない）。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_that_breaks_matchmedia_fails_instead_of_silently_capturing() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP a_page_that_breaks_matchmedia_fails_instead_of_silently_capturing: \
                 no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_reduced_motion_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(10);
        options.emulate_reduced_motion = true;
        let renderer = StoryRenderer::launch(options.clone())
            .await
            .expect("launch (emulation on)");
        let err = renderer
            .render_story(&server.base_url(), "demo-rm--mocked")
            .await
            .expect_err(
                "a page whose matchMedia cannot confirm the emulation must fail \
                 instead of being captured",
            );
        renderer.close().await;

        let message = err.to_string();
        assert!(
            message.contains("could not be verified") && message.contains("matchMedia"),
            "the error must say the emulation could not be verified and name the \
             failing probe, got {message:?}"
        );
        assert!(
            matches!(err, RenderError::Story { .. }),
            "the failure must be story-scoped so the remaining stories keep rendering"
        );

        // 対照: 要求していなければ同じページでも成功する。
        options.emulate_reduced_motion = false;
        let renderer = StoryRenderer::launch(options)
            .await
            .expect("launch (emulation off)");
        renderer
            .render_story(&server.base_url(), "demo-rm--mocked")
            .await
            .expect("without the request the mocked page is a legitimate story");
        renderer.close().await;
    }

    /// **配線の固定**: reduced-motion 検証が verdict を返さないとき、freeze の
    /// phase ではなく **reduced-motion 自身の phase** で倒れること。
    ///
    /// 機構（待ち・リトライ・期限）は [`evaluate_with_deadline_retry`] に
    /// 一本化されているが、分類は呼び出し側が渡す定数で決まる。ヘルパ自体の
    /// 両分岐はブラウザ無しの `evaluate_*` 系が固定しているので、ここが
    /// 固定するのは**この経路が [`REDUCED_MOTION_PHASES`] を渡していること**
    /// ——freeze 側と取り違えても型は通ってしまう一点である。
    ///
    /// fixture は検証プローブが最初に触る `CSSStyleSheet` を、返らない
    /// 構築子へ差し替える。差し替えは story 描画後に効くので READY 判定は
    /// 通り、止まるのは検証の evaluate だけになる。
    ///
    /// 証明する: 経路の phase と story 分類。証明しない: リトライ切れ側の
    /// 配線（実ページで CDP エラーを決定的に起こす手立てが無い——後述の
    /// 判断できなかった点）。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reduced_motion_probe_that_never_returns_fails_with_its_own_phase() {
        let Some(chromium) = discover_chromium() else {
            eprintln!(
                "SKIP a_reduced_motion_probe_that_never_returns_fails_with_its_own_phase: \
                 no chromium"
            );
            return;
        };
        let _guard = BROWSER_LOCK.lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        write_reduced_motion_bundle(dir.path());
        let server = StaticServer::start(dir.path()).await.expect("start server");

        let mut options = RenderOptions::new(chromium, 320, 240);
        options.story_timeout = Duration::from_secs(4);
        options.emulate_reduced_motion = true;
        let renderer = StoryRenderer::launch(options).await.expect("launch");

        // 外周上限: ここで落ちたら evaluate が deadline に載っていない。
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            renderer.render_story(&server.base_url(), "demo-rm--blocking"),
        )
        .await
        .expect("render_story must return within the outer bound");
        renderer.close().await;

        let err = result.expect_err("a verification that never returns must not be captured");
        match err {
            RenderError::Timeout { phase, .. } => {
                assert_eq!(
                    phase, "the reduced-motion verification never returned a verdict",
                    "the stage must be reported as the reduced-motion one"
                );
                assert!(
                    !phase.contains("freeze"),
                    "the freeze stage must not answer for the reduced-motion one"
                );
            }
            other => panic!(
                "a verification that never returns must be story-scoped Timeout, \
                 not an infrastructure failure that aborts the build, got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn static_server_serves_the_bundle_over_loopback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("iframe.html"), "<html>ok</html>").expect("write");

        let server = StaticServer::start(dir.path()).await.expect("start server");
        assert!(server.addr().ip().is_loopback());

        let body = reqwest::get(format!("{}/iframe.html", server.base_url()))
            .await
            .expect("request")
            .text()
            .await
            .expect("body");
        assert_eq!(body, "<html>ok</html>");
    }
}
