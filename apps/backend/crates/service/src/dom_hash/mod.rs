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
pub const NORM_VERSION: u16 = 1;

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
    pub(crate) fn lookup(&self, id: &str) -> Option<usize> {
        self.numbers.get(&id.to_ascii_lowercase()).copied()
    }

    /// `v-bind()` が作る custom property の名に番号を振る。
    /// 既に振ってあれば、同じ番号を返す。
    pub(crate) fn number_for_css_var(&mut self, name: &str) -> usize {
        let next = self.css_vars.len() + 1;
        *self
            .css_vars
            .entry(name.to_ascii_lowercase())
            .or_insert(next)
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
pub fn normalize_dom(
    dom: &str,
    rules: &NormalizeRules,
    scope_ids: &mut ScopeIds,
) -> Result<String, NormalizeError> {
    check_size(dom)?;
    let tokens = html::tokenize(dom)?;
    let tokens = rules::apply_to_dom(tokens, rules, scope_ids);
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
    let dom_text = normalize_dom(dom, rules, &mut scope_ids)?;
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
mod tests;
