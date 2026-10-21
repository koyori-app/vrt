//! DOM の正規化の規則。
//! 規則は一つずつ独立していて、[`NormalizeRules`] の欄で入れ切りできる。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::ScopeIds;
use super::html::{Attr, Token, is_void};

/// 正規化の規則の入れ切り。
///
/// 通常は [`NormalizeRules::all`] を使う。
/// 欄ごとに外せるのは、規則ごとの試験と、規則を外したときに何が一致しなくなるかの測定のためである。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeRules {
    /// コメントを落とす。
    /// DOM のコメントノードと、CSS のコメント。
    pub drop_comments: bool,
    /// 描画しない要素を落とす。
    /// `script`、`noscript`、`link`、`style`、`head` の中身。
    pub drop_non_rendering: bool,
    /// Vue の scoped style の印を、出現順の番号に置き換える。
    pub scope_ids: bool,
    /// `v-bind()` が作る custom property の名を、出現順の番号に置き換える。
    pub bound_css_vars: bool,
    /// 自動で生える id と、それを指す属性を、出現順の番号に置き換える。
    pub generated_ids: bool,
    /// 参照で結ばれた id を、出現順の番号に置き換える。
    /// CSSOM の字に現れる id は残す。
    pub referenced_ids: bool,
    /// loopback の origin と `blob:` の識別子を、決まった記号に置き換える。
    pub ephemeral_origins: bool,
    /// 属性を名前の順に並べ、class の語を並べ替える。
    pub sort_attributes: bool,
    /// CSS の空白の連なりを一つの空白にする。
    pub collapse_css_whitespace: bool,
}

impl NormalizeRules {
    /// すべての規則を入れる。
    pub fn all() -> Self {
        Self {
            drop_comments: true,
            drop_non_rendering: true,
            scope_ids: true,
            bound_css_vars: true,
            generated_ids: true,
            referenced_ids: true,
            ephemeral_origins: true,
            sort_attributes: true,
            collapse_css_whitespace: true,
        }
    }

    /// どの規則も入れない（字句に割って書き戻すだけ）。
    pub fn none() -> Self {
        Self {
            drop_comments: false,
            drop_non_rendering: false,
            scope_ids: false,
            bound_css_vars: false,
            generated_ids: false,
            referenced_ids: false,
            ephemeral_origins: false,
            sort_attributes: false,
            collapse_css_whitespace: false,
        }
    }
}

impl Default for NormalizeRules {
    fn default() -> Self {
        Self::all()
    }
}

pub(crate) fn apply_to_dom(
    tokens: Vec<Token>,
    rules: &NormalizeRules,
    scope_ids: &mut ScopeIds,
    css_names: &HashSet<String>,
) -> Vec<Token> {
    let mut tokens = tokens;
    if rules.drop_comments {
        tokens = drop_comments(tokens);
    }
    if rules.drop_non_rendering {
        tokens = drop_non_rendering(tokens);
    }
    if rules.scope_ids {
        number_scope_ids(&mut tokens, scope_ids);
    }
    if rules.bound_css_vars {
        number_bound_css_vars(&mut tokens, scope_ids);
    }
    if rules.referenced_ids {
        number_referenced_ids(&mut tokens, css_names);
    }
    if rules.generated_ids {
        number_generated_ids(&mut tokens, css_names);
    }
    if rules.ephemeral_origins {
        mask_origins_in_attrs(&mut tokens);
    }
    if rules.sort_attributes {
        sort_attributes(&mut tokens);
    }
    tokens
}

/// コメントノードを落とす。
pub(crate) fn drop_comments(tokens: Vec<Token>) -> Vec<Token> {
    tokens
        .into_iter()
        .filter(|t| !matches!(t, Token::Comment(_)))
        .collect()
}

/// 要素ごと落とす名前。
/// `head` は、中の `meta` や `title` ごと落とす。
fn is_dropped_element(name: &str) -> bool {
    ["script", "noscript", "link", "style", "head"]
        .iter()
        .any(|d| d.eq_ignore_ascii_case(name))
}

/// 描画しない要素を、中身ごと落とす。
///
/// `style` と `link` は、中身が CSSOM の側に入るので、DOM からは落とす。
/// body の中の `title`（svg の説明）は、描画に関わりうるので落とさない。
pub(crate) fn drop_non_rendering(tokens: Vec<Token>) -> Vec<Token> {
    let mut out = Vec::with_capacity(tokens.len());
    // 落としている最中の要素の名と、同じ名の入れ子の深さ。
    let mut skipping: Option<(String, usize)> = None;
    for token in tokens {
        if let Some((name, depth)) = &mut skipping {
            match &token {
                Token::Start { name: n, .. } if n.eq_ignore_ascii_case(name) && !is_void(n) => {
                    *depth += 1;
                }
                Token::End(n) if n.eq_ignore_ascii_case(name) => {
                    *depth -= 1;
                    if *depth == 0 {
                        skipping = None;
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Token::Start { name, .. } = &token
            && is_dropped_element(name)
        {
            if !is_void(name) {
                skipping = Some((name.clone(), 1));
            }
            continue;
        }
        // 落とした空要素の、対にならない終了タグは書かれないので、そのまま残してよい。
        out.push(token);
    }
    out
}

/// `data-v-` に 8 桁の 16 進が続く名前なら、その 16 進を返す。
pub(crate) fn scope_hex(name: &str) -> Option<&str> {
    name.get(..7)
        .filter(|p| p.eq_ignore_ascii_case("data-v-"))?;
    let hex = name.get(7..)?;
    (hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// 先頭が `-` に 8 桁の 16 進を続けた形で、その 16 進が既に番号を振った scoped style の印なら、その番号を返す。
///
/// Vue は、scoped style の中の `@keyframes` の名の後ろに、印の 16 進を付ける（`name-58152a41`）。
/// 印と同じ値なので、印と同じ番号に置き換えれば、build ごとの違いを畳める。
/// 既知の印だけを置き換える。
/// 知らない 16 進は、別の物かもしれないので残す。
pub(crate) fn scope_suffix_number(rest: &str, prev_ident: bool, ids: &ScopeIds) -> Option<usize> {
    if !prev_ident {
        return None;
    }
    let hex = rest.strip_prefix('-')?.get(..8)?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let next = rest[9..].chars().next();
    if next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()) {
        return None;
    }
    ids.lookup(hex)
}

/// 文字列の中の、既知の scoped style の印を後ろに付けた名前を、番号に置き換える。
pub(crate) fn rename_scope_suffixes<'a>(text: &'a str, ids: &ScopeIds) -> Cow<'a, str> {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut changed = false;
    let mut prev_ident = false;
    while i < text.len() {
        let rest = &text[i..];
        if let Some(n) = scope_suffix_number(rest, prev_ident, ids) {
            out.push_str(&format!("-#{n}"));
            i += 9;
            changed = true;
            prev_ident = true;
            continue;
        }
        let c = rest.chars().next().expect("rest は空でない");
        out.push(c);
        i += c.len_utf8();
        prev_ident = c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii();
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

/// Vue の scoped style の印を、出現順の番号へ置き換える。
///
/// 属性ごと落とすと、どの要素にどの scope の規則が効くかの対応が消える。
/// 番号にすれば、CSSOM の属性セレクターを同じ表で置き換えて、対応を保てる。
/// 番号は属性の並べ替えより前に振る。
/// 並べ替えを先にすると、build ごとに変わる 16 進の順で番号が決まってしまう。
pub(crate) fn number_scope_ids(tokens: &mut [Token], scope_ids: &mut ScopeIds) {
    for token in tokens.iter_mut() {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs {
            if let Some(hex) = scope_hex(&attr.name) {
                let n = scope_ids.number_for(hex);
                attr.name = format!("data-v-#{n}");
            }
        }
    }
    // 印を全部振ってから、`style` 属性の中の、印を後ろに付けた名前を置き換える。
    for token in tokens {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs {
            if attr.name.eq_ignore_ascii_case("style")
                && let Cow::Owned(renamed) = rename_scope_suffixes(&attr.value, scope_ids)
            {
                attr.value = renamed;
            }
        }
    }
}

/// `--` に続く名前が、`v-bind()` の作る名前（8 桁の 16 進、または `v` を前置した 8 桁の 16 進）なら、
/// 名前の部分（`--` を除く）の長さを返す。
///
/// 名前の後ろが識別子の続きなら、別の名前なので `None` を返す。
/// 手で付けた名前（`--spacing`、`--color-amber-500` など）は、この形に当たらない。
pub(crate) fn bound_var_name_len(s: &str) -> Option<usize> {
    let name = s.strip_prefix("--")?;
    let hex_start = usize::from(name.starts_with(['v', 'V']));
    let hex = name.get(hex_start..hex_start + 8)?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let len = hex_start + 8;
    let next = name[len..].chars().next();
    if next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()) {
        return None;
    }
    Some(len)
}

/// 文字列の中の、`v-bind()` の作る custom property の名を、出現順の番号に置き換える。
///
/// `style` 属性の値と、CSS の字の両方に使う。
/// 名前の前が識別子の字なら、別の名前の一部なので置き換えない。
pub(crate) fn rename_bound_vars<'a>(text: &'a str, ids: &mut ScopeIds) -> Cow<'a, str> {
    if !text.contains("--") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut changed = false;
    let mut prev_ident = false;
    while i < text.len() {
        let rest = &text[i..];
        if !prev_ident && let Some(len) = bound_var_name_len(rest) {
            let n = ids.number_for_css_var(&rest[2..2 + len]);
            out.push_str(&format!("--bound-#{n}"));
            i += 2 + len;
            changed = true;
            prev_ident = true;
            continue;
        }
        let c = rest.chars().next().expect("rest は空でない");
        out.push(c);
        i += c.len_utf8();
        prev_ident = c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii();
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

/// `style` 属性の中の、`v-bind()` の作る custom property の名を、番号に置き換える。
///
/// 名前は build ごとに変わる。
/// 番号にすれば、CSSOM の `var()` を同じ表で置き換えて、対応を保てる。
pub(crate) fn number_bound_css_vars(tokens: &mut [Token], ids: &mut ScopeIds) {
    for token in tokens {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs {
            if attr.name.eq_ignore_ascii_case("style")
                && let Cow::Owned(renamed) = rename_bound_vars(&attr.value, ids)
            {
                attr.value = renamed;
            }
        }
    }
}

/// id を参照する属性のうち、値が id の並びのもの。
const ID_LIST_ATTRS: &[&str] = &[
    "for",
    "aria-labelledby",
    "aria-describedby",
    "aria-controls",
    "aria-owns",
    "aria-activedescendant",
    "aria-details",
    "aria-errormessage",
    "aria-flowto",
    "headers",
    "list",
    "form",
    "popovertarget",
    "commandfor",
];

/// 自動で生える id の部分を、`(開始, 終了)` の byte の範囲で返す。
///
/// 既知の生成規則だけを見る。
/// 手で書いた id（CSS の `#main` が指すもの）と、知らない規則の id は、生成された物とみなさない。
/// 落として失うのは見逃しで、残して失うのは緩める機会だけだからである。
///
/// - React の `useId`: `:r1:`、`«r1»`、`_r_1_`
/// - Vue の `useId`: `v-0`（前に `-` を挟んでもよい）
pub(crate) fn generated_span(id: &str) -> Option<(usize, usize)> {
    let is_token_char = |c: char| c.is_ascii_digit() || c.is_ascii_lowercase();
    for (i, c) in id.char_indices() {
        let rest = &id[i..];
        let patterns: [(&str, &str); 3] = [(":r", ":"), ("«r", "»"), ("_r_", "_")];
        for (open, close) in patterns {
            if let Some(after) = rest.strip_prefix(open) {
                let body: usize = after
                    .chars()
                    .take_while(|&c| is_token_char(c))
                    .map(char::len_utf8)
                    .sum();
                if body > 0 && after[body..].starts_with(close) {
                    return Some((i, i + open.len() + body + close.len()));
                }
            }
        }
        if (c == 'v') && (i == 0 || id[..i].ends_with('-')) && vue_use_id_tail(rest) {
            return Some((i, id.len()));
        }
    }
    None
}

/// `v-0` や `v-0-1` のように、`v-` と数字の連なりだけで終わるか。
fn vue_use_id_tail(s: &str) -> bool {
    let Some(mut rest) = s.strip_prefix("v-") else {
        return false;
    };
    loop {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        rest = &rest[digits..];
        match rest.strip_prefix('-') {
            Some(next) => rest = next,
            None => return rest.is_empty(),
        }
    }
}

/// 自動で生える id と、それを指す属性を、出現順の番号へ置き換える。
///
/// 参照で結ばれた id は、[`number_referenced_ids`] が先に番号にする。
/// この規則は、形が生成物らしい id のうち、参照されていない物にも効く補助である。
/// CSSOM の字に現れる id は、名前が絵に効くので残す。
///
/// 同じ id には同じ番号を振る。
/// 参照が定義より先に現れることもあるので、定義と参照を区別せず、現れた順に振る。
pub(crate) fn number_generated_ids(tokens: &mut [Token], css_names: &HashSet<String>) {
    let mut numbers: HashMap<String, usize> = HashMap::new();
    let mut mask = |id: &str| -> String {
        // CSSOM の字に現れる id は、名前が絵に効くので残す。
        if css_names.contains(id) {
            return id.to_string();
        }
        let Some((start, end)) = generated_span(id) else {
            return id.to_string();
        };
        let next = numbers.len() + 1;
        let n = *numbers.entry(id.to_string()).or_insert(next);
        format!("{}#gen{n}{}", &id[..start], &id[end..])
    };
    for token in tokens {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs {
            let name = attr.name.to_ascii_lowercase();
            if name == "id" {
                attr.value = mask(&attr.value);
            } else if ID_LIST_ATTRS.contains(&name.as_str()) {
                attr.value = attr
                    .value
                    .split_ascii_whitespace()
                    .map(&mut mask)
                    .collect::<Vec<_>>()
                    .join(" ");
            } else if (name == "href" || name == "xlink:href")
                && let Some(target) = attr.value.strip_prefix('#')
            {
                attr.value = format!("#{}", mask(target));
            }
        }
    }
}

/// 参照で結ばれた id を、出現順の番号へ置き換える。
///
/// 参照の属性（`for`、`aria-labelledby`、`aria-describedby`、`aria-controls`、`aria-activedescendant` など）の値と、
/// `href` の `#` 以降が指す id を「参照された id」とする。
/// 参照された id と、同じ値の `id` 属性を、同じ番号にする。
/// 番号は、定義と参照を区別せず、現れた順に振る。
///
/// build ごとに乱数が変わる id を、部品の名前に頼らず畳むための、一般の形である。
/// id の名前が絵に効く道は、CSS が id で選ぶ場合だけである。
/// JS が名前で読んだ結果は、描画後の DOM に出る。
/// そこで、CSSOM の字に現れる id は、名前に意味があるので残す。
///
/// 参照されていない id は、この規則では置き換えない。
/// 既知の生成規則の id は、[`number_generated_ids`] が別に番号にする。
///
/// 見逃すのは、次の変化である。
/// - 参照された id の名前だけが変わり、CSS にも描画にも出ない変化。
/// - 外の頁から `#id` で指される anchor の名前の変更（絵には出ない）。
/// - CSS が id を 16 進の escape で書いている場合の、名前の変更（字の一致で見ているため、守れない）。
///
/// `url(#id)` の形の参照は、まだ扱わない。
/// その id は参照されていないものとして、残る。
pub(crate) fn number_referenced_ids(tokens: &mut [Token], css_names: &HashSet<String>) {
    let mut referenced: HashSet<String> = HashSet::new();
    for token in tokens.iter() {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs {
            let name = attr.name.to_ascii_lowercase();
            if ID_LIST_ATTRS.contains(&name.as_str()) {
                referenced.extend(attr.value.split_ascii_whitespace().map(str::to_string));
            } else if (name == "href" || name == "xlink:href")
                && let Some(target) = attr.value.strip_prefix('#')
                && !target.is_empty()
            {
                referenced.insert(target.to_string());
            }
        }
    }
    if referenced.is_empty() {
        return;
    }
    let mut numbers: HashMap<String, usize> = HashMap::new();
    let mut mask = |id: &str| -> String {
        if !referenced.contains(id) || css_names.contains(id) {
            return id.to_string();
        }
        let next = numbers.len() + 1;
        let n = *numbers.entry(id.to_string()).or_insert(next);
        format!("#ref{n}")
    };
    for token in tokens {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs {
            let name = attr.name.to_ascii_lowercase();
            if name == "id" {
                attr.value = mask(&attr.value);
            } else if ID_LIST_ATTRS.contains(&name.as_str()) {
                attr.value = attr
                    .value
                    .split_ascii_whitespace()
                    .map(&mut mask)
                    .collect::<Vec<_>>()
                    .join(" ");
            } else if (name == "href" || name == "xlink:href")
                && let Some(target) = attr.value.strip_prefix('#')
                && !target.is_empty()
            {
                attr.value = format!("#{}", mask(target));
            }
        }
    }
}

/// 毎回変わる origin と識別子を、決まった記号へ置き換える。
///
/// - `http://127.0.0.1:4101/` のような loopback の origin は `<origin>` にする。
///   runner は空いている port で bundle を配信するので、port が毎回変わる。
/// - `blob:` で始まる URL は `blob:<id>` にする。
///   識別子が毎回変わる。
///
/// loopback でない origin（外部の CDN など）は、そのまま残す。
pub(crate) fn mask_ephemeral_origins(s: &str) -> Cow<'_, str> {
    if !s.contains("://") && !s.contains("blob:") {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut changed = false;
    while i < s.len() {
        let rest = &s[i..];
        let at_boundary = i == 0
            || !s[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric());
        if at_boundary {
            if let Some(after) = rest.strip_prefix("blob:") {
                let len = after
                    .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ','))
                    .unwrap_or(after.len());
                out.push_str("blob:<id>");
                i += 5 + len;
                changed = true;
                continue;
            }
            if let Some(len) = loopback_origin_len(rest) {
                out.push_str("<origin>");
                i += len;
                changed = true;
                continue;
            }
        }
        let c = rest.chars().next().expect("rest は空でない");
        out.push(c);
        i += c.len_utf8();
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(s)
    }
}

/// 先頭が loopback の origin（`scheme://host:port`）なら、その byte 数を返す。
fn loopback_origin_len(s: &str) -> Option<usize> {
    let after_scheme = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))?;
    let scheme_len = s.len() - after_scheme.len();
    let host = ["127.0.0.1", "localhost", "[::1]", "0.0.0.0"]
        .into_iter()
        .find(|h| {
            after_scheme
                .get(..h.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(h))
        })?;
    let mut len = scheme_len + host.len();
    let tail = &s[len..];
    if let Some(port) = tail.strip_prefix(':') {
        let digits = port.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            len += 1 + digits;
        }
    }
    // host の続きが名前の字なら、別の host（`localhost.example.com` など）である。
    let next = s[len..].chars().next();
    if next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return None;
    }
    Some(len)
}

/// 値が URL か、URL を含みうる属性。
const URL_ATTRS: &[&str] = &[
    "src",
    "href",
    "srcset",
    "poster",
    "action",
    "xlink:href",
    "data",
    "cite",
    "style",
];

fn mask_origins_in_attrs(tokens: &mut [Token]) {
    for token in tokens {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs {
            if URL_ATTRS.contains(&attr.name.to_ascii_lowercase().as_str())
                && let Cow::Owned(masked) = mask_ephemeral_origins(&attr.value)
            {
                attr.value = masked;
            }
        }
    }
}

/// 属性を名前の順に並べ、class の語を並べ替える。
///
/// どちらも、描画に影響せず、順序は不定である。
pub(crate) fn sort_attributes(tokens: &mut [Token]) {
    for token in tokens {
        let Token::Start { attrs, .. } = token else {
            continue;
        };
        for attr in attrs.iter_mut() {
            if attr.name.eq_ignore_ascii_case("class") {
                let mut words: Vec<&str> = attr.value.split_ascii_whitespace().collect();
                words.sort_unstable();
                attr.value = words.join(" ");
            }
        }
        attrs.sort_by(|a: &Attr, b: &Attr| a.name.cmp(&b.name));
    }
}
