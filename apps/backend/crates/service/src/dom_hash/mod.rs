//! DOM と CSSOM の正規化と hash。
//! DB にも HTTP にもブラウザにも依存しない純粋な計算モジュール。
//!
//! 設計は `docs/dom-hash-relaxed-threshold.md` の段 2 にあたる。
//! 実物の Storybook で測った結果は `docs/dom-hash-normalization-measurement.md` にある。
//! この module は、呼ばれない限り何も起こさない。
//! 撮る仕掛け、設定、比較、画面、判定のどれからも呼ばれておらず、DB の列も API も持たない。
//!
//! ## 入力
//!
//! DOM の字は、ブラウザの直列化（HTML の断片の直列化の手順）が出す字を受け取る。
//! 属性値は二重引用符で括られ、`&` と `"` と `<` と `>` と no-break space は実体参照になっている。
//! この形でない入力（閉じていない引用符、実体参照になっていない `<` など）は、推し量らずに [`NormalizeError`] を返す。
//! 返さずに hash を作ってしまうと、読み違えた字を別の字と同じ hash に畳み込むおそれがあるからである。
//!
//! CSSOM の字は、全 stylesheet の全規則を文書の順に直列化して連ねた字を受け取る。
//! どう直列化して連ねるかは、撮る仕掛けの側（段 1）の責任である。
//!
//! ## 出力
//!
//! DOM と CSSOM を別々の hash にする。
//! 別々に持てば、不一致のとき、どちらが違ったかを利用者に言えるからである。
//!
//! hash は、正規化した字に、用途と正規化の版を前置して SHA-256 を取り、`sha256:` を頭に付けた hex で返す。
//! 用途を前置するのは、DOM の字と CSSOM の字がたまたま同じ字になっても、同じ hash にならないようにするためである。
//! 版を前置するのは、規則を変えたとき、古い hash と新しい hash が偶然一致しないようにするためである。
//!
//! ## 正規化の規則
//!
//! 規則は [`NormalizeRules`] の欄ごとに独立していて、一つずつ入れ切りできる。
//! 通常は [`NormalizeRules::all`] を使う。
//! 欄を分けてあるのは、規則ごとの試験と、規則を外したときに何が一致しなくなるかを測るためである。
//!
//! 迷うものは落とさない。
//! 落とせば不一致が減るが、本当の変化も同じ hash に畳み込み、見逃しの入口になる。
//! 落とさなければ、不一致が増えるだけで、従来どおりの厳しい比較になる。
//!
//! 正規化が捨ててよいのは、CSS から観測できぬ差だけである。
//! CSS から観測できるとは、selector の当たり（要素の名前、属性の名前と値、兄弟の順と数、子孫の有無）と、
//! 宣言が描く物（`content` と `attr()` の文字列、`url()` の先、custom property の値、`white-space` の下の空白と改行）に、差が出ることである。
//! 規則は、次の三つの形のどれかでなければならない。
//!
//! - 観測できない差を捨てる（属性の順、コメント、引用符の種類、CSS の空白の連なりの長さ）。
//! - 名前を付け替える。名前が識別子として現れる所のすべてで同じ表を使い、対応を残す。
//!   引用文字列と `url()` の中は識別子ではないので触れない。
//!   CSS が名前を字として読む場合（`attr()`、CSS が書いた id）は付け替えない。
//! - 条件を付けて捨てる。CSS が観測しうる時は手を引く。条件は多めに見積もる。
//!
//! 規則を足す時は、捨てる差を二つの入力に書き、それを観測する CSS を探す。
//! 書けるなら、その差は捨てられない。
//! `observation_tests` の両側の表に対を置くまで、試験が通らない。
//!
//! 参照で結ばれた id は、出現順の番号にする。
//! 参照の属性（`for`、`aria-labelledby`、`aria-controls` など）と `href` の `#` 以降が指す id と、同じ値の `id` が対象である。
//! build ごとに乱数が変わる id を、部品の名前に頼らずに畳むための、一般の形である。
//! 既知の生成規則の id（React や Vue の `useId`）を番号にする規則は、参照されていない id にも効く補助として残している。
//! CSSOM の字に現れる id は、名前が絵に効くので、どちらの規則でも残す。
//!
//! 参照で結ぶ規則が見逃すのは、次の変化である。
//! - 参照された id の名前だけが変わり、CSS にも描画にも出ない変化。
//! - 外の頁から `#id` で指される anchor の名前の変更（絵には出ない）。
//!
//! `url(#id)` の形の参照は扱わない。
//! その id は、参照されていないものとして、字のまま残る。
//!
//! 観測の見積もりが近似である所は、次のとおりである。
//! - 描画しない要素（`script`、`style`、`link`、`noscript`、`head`）の属性と中身を捨てる規則は、
//!   CSS がその要素の名前を裸で書いた時に手を引く。
//!   `*` や属性だけの selector で `display` を上書きして、これらを描く CSS は、見落とす。
//! - URL の URL らしさは、属性の名（`src`、`href` など）と、`url()` の引数で決める。
//! - 外の頁から `#id` で指される `:target` は、撮る URL に fragment が無い前提である。
//!
//! 時刻と乱数の text は消さない。
//! 消せば、本当の text の変化を同じ hash に畳み込む。
//! 時刻や乱数が絵に入る story は、story の側で固定するのが本筋である。

mod css;
mod html;
mod rules;

use std::collections::HashMap;

use sha2::{Digest, Sha256};

pub use rules::NormalizeRules;

/// 正規化の規則の版。
///
/// 規則を変えたら上げる。
/// hash の入力にこの値を含めるので、版が違う hash は、字が同じでも一致しない。
pub const NORM_VERSION: u16 = 2;

/// 受け取る字の大きさの上限（DOM と CSSOM のそれぞれ）。
///
/// 超える入力は、読まずに断る。
pub const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;

/// hash の方式の名。
/// 値の頭に付け、将来の方式の変更で、違う方式同士を一致扱いしないようにする。
pub const HASH_SCHEME: &str = "sha256";

/// 正規化できなかった理由。
///
/// 返った場合は、hash を作らない。
/// 呼び手は、緩めずに従来どおりの比較へ倒すこと。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NormalizeError {
    #[error("DOM の字を読めない: {reason}（{offset} byte 目）")]
    Html { reason: &'static str, offset: usize },
    #[error("CSSOM の字を読めない: {reason}（{offset} byte 目）")]
    Css { reason: &'static str, offset: usize },
    #[error("入力が大きすぎる: {bytes} byte（上限 {limit} byte）")]
    TooLarge { bytes: usize, limit: usize },
}

/// DOM と CSSOM の hash の組。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomCssomHashes {
    /// 正規化した DOM の hash。
    pub dom_hash: String,
    /// 正規化した CSSOM の hash。
    pub cssom_hash: String,
    /// 正規化の規則の版。
    pub norm_version: u16,
}

/// 正規化の途中で、DOM と CSSOM が共有する連番の表。
///
/// Vue の scoped style の印は、DOM の属性と CSSOM の属性セレクターの両方に現れる。
/// `v-bind()` が作る custom property の名も、DOM の `style` 属性と CSSOM の両方に現れる。
/// 同じ物には同じ番号を振らないと、どの要素にどの規則が効くかの対応が壊れる。
/// 番号は、DOM、CSSOM の順に、最初に現れた順に振る。
#[derive(Debug, Default)]
pub struct ScopeIds {
    numbers: HashMap<String, usize>,
    css_vars: HashMap<String, usize>,
}

impl ScopeIds {
    pub fn new() -> Self {
        Self::default()
    }

    /// 印に番号を振る。
    /// 既に振ってあれば、同じ番号を返す。
    /// 16 進の大文字と小文字は同じ印として扱う。
    pub(crate) fn number_for(&mut self, id: &str) -> usize {
        let next = self.numbers.len() + 1;
        *self.numbers.entry(id.to_ascii_lowercase()).or_insert(next)
    }

    /// 既に番号を振った印なら、その番号を返す（新しくは振らない）。
    ///
    /// `@keyframes` の名の後ろに付く印は、識別子の一部で、大文字と小文字を区別する。
    /// 属性の名は小文字で記録してあるので、大文字の 16 進は同じ印として扱わない。
    pub(crate) fn lookup(&self, id: &str) -> Option<usize> {
        self.numbers.get(id).copied()
    }

    /// `v-bind()` が作る custom property の名に番号を振る。
    /// 既に振ってあれば、同じ番号を返す。
    ///
    /// custom property の名は、大文字と小文字を区別する。
    /// `--V5AD87C78` と `--v5ad87c78` は別の変数なので、同じ番号にしない。
    pub(crate) fn number_for_css_var(&mut self, name: &str) -> usize {
        let next = self.css_vars.len() + 1;
        *self.css_vars.entry(name.to_string()).or_insert(next)
    }

    /// これまでに振った番号の数。
    pub fn len(&self) -> usize {
        self.numbers.len() + self.css_vars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.numbers.is_empty() && self.css_vars.is_empty()
    }
}

/// DOM の字を正規化する。
///
/// `scope_ids` は、続けて CSSOM の字を正規化するときに同じものを渡す。
///
/// CSSOM の字を知らないので、参照で結ばれた id は、すべて番号にする。
/// CSSOM の字に現れる id を残すには、[`normalize_dom_with_css`] を使う。
pub fn normalize_dom(
    dom: &str,
    rules: &NormalizeRules,
    scope_ids: &mut ScopeIds,
) -> Result<String, NormalizeError> {
    normalize_dom_with_css(dom, "", rules, scope_ids)
}

/// DOM の字を、CSSOM の字を見ながら正規化する。
///
/// CSSOM の字に現れる id は、名前が絵に効くので、番号にせず残す。
/// `cssom` には、正規化する前の字を渡す（字に現れるかを見るだけである）。
pub fn normalize_dom_with_css(
    dom: &str,
    cssom: &str,
    rules: &NormalizeRules,
    scope_ids: &mut ScopeIds,
) -> Result<String, NormalizeError> {
    check_size(dom)?;
    check_size(cssom)?;
    let tokens = html::tokenize(dom)?;
    let facts = css::CssFacts::read(cssom);
    let tokens = rules::apply_to_dom(tokens, rules, scope_ids, &facts);
    Ok(html::render(&tokens))
}

/// CSSOM の字を正規化する。
///
/// DOM の字を先に正規化したときの `scope_ids` を渡す。
pub fn normalize_cssom(
    cssom: &str,
    rules: &NormalizeRules,
    scope_ids: &mut ScopeIds,
) -> Result<String, NormalizeError> {
    check_size(cssom)?;
    css::normalize(cssom, rules, scope_ids)
}

/// DOM の字と CSSOM の字から、二つの hash を作る。
pub fn hash_dom_and_cssom(dom: &str, cssom: &str) -> Result<DomCssomHashes, NormalizeError> {
    hash_dom_and_cssom_with(dom, cssom, &NormalizeRules::all())
}

/// 規則を選んで、二つの hash を作る。
///
/// 通常は [`hash_dom_and_cssom`] を使う。
/// 規則を外した結果は、本番の hash と一致しない（一致させる用途に使わないこと）。
pub fn hash_dom_and_cssom_with(
    dom: &str,
    cssom: &str,
    rules: &NormalizeRules,
) -> Result<DomCssomHashes, NormalizeError> {
    let mut scope_ids = ScopeIds::new();
    let dom_text = normalize_dom_with_css(dom, cssom, rules, &mut scope_ids)?;
    let cssom_text = normalize_cssom(cssom, rules, &mut scope_ids)?;
    Ok(DomCssomHashes {
        dom_hash: hash_text("dom", &dom_text),
        cssom_hash: hash_text("cssom", &cssom_text),
        norm_version: NORM_VERSION,
    })
}

/// 正規化した字から hash を作る。
fn hash_text(kind: &str, normalized: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("vrt-dom-hash/{NORM_VERSION}/{kind}\n").as_bytes());
    hasher.update(normalized.as_bytes());
    format!("{HASH_SCHEME}:{}", hex::encode(hasher.finalize()))
}

fn check_size(input: &str) -> Result<(), NormalizeError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(NormalizeError::TooLarge {
            bytes: input.len(),
            limit: MAX_INPUT_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod observation_tests;
#[cfg(test)]
mod tests;
