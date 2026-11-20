//! CSSOM の字の正規化と、CSS の字から読み取る事実。
//!
//! CSS を構文木にはしない。
//! コメント、文字列、`url()` の中身、それ以外の四つに割って、それぞれに規則を当てる。
//! 規則の対象は、属性セレクターや `var()` に現れる名前の付け替えと、`url()` の中の origin と、空白の連なりだけである。
//! 引用符で括られた文字列は、`url()` の引数でない限り、中身を変えない。
//! 文字列は、`content` に描かれ、属性セレクターが比べ、custom property の値として渡るので、字のまま CSS に見えるからである。

use std::borrow::Cow;
use std::collections::HashSet;

use super::rules::{bound_var_name_len, mask_ephemeral_origins, scope_hex, scope_suffix_number};
use super::{NormalizeError, NormalizeRules, ScopeIds};

fn err(reason: &'static str, offset: usize) -> NormalizeError {
    NormalizeError::Css { reason, offset }
}

/// CSS の識別子に使える字か。
pub(crate) fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

/// CSS の空白か。
///
/// CSS の空白は、space、tab、LF、CR、FF の五つである。
/// `char::is_whitespace` は使わない。
/// それは NBSP や全角空白などの非 ASCII の字まで真にするが、それらは CSS の空白ではなく、識別子の一部になる。
pub(crate) fn is_css_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

/// `\` で始まる escape の byte 数。
///
/// `\` に 1 から 6 桁の 16 進が続くなら、桁と、その後ろの空白一つ（CRLF は一つ）までが escape である。
/// その空白は escape の終わりの印で、識別子の続きとは分けるために置かれる。
/// そうでなければ `\` と次の一字、末尾の `\` は一字である。
pub(crate) fn escape_len(s: &str) -> usize {
    let after = &s[1..];
    let hex = after
        .bytes()
        .take(6)
        .take_while(u8::is_ascii_hexdigit)
        .count();
    if hex > 0 {
        let tail = &after[hex..];
        let ws = if tail.starts_with("\r\n") {
            2
        } else {
            usize::from(tail.chars().next().is_some_and(is_css_whitespace))
        };
        return 1 + hex + ws;
    }
    1 + after.chars().next().map_or(0, char::len_utf8)
}

/// `\` で始まる escape を解く。
/// 返すのは、解いた字と、escape の byte 数。
fn decode_escape(s: &str) -> (char, usize) {
    let len = escape_len(s);
    let after = &s[1..];
    let hex = after
        .bytes()
        .take(6)
        .take_while(u8::is_ascii_hexdigit)
        .count();
    if hex > 0 {
        let value = u32::from_str_radix(&after[..hex], 16).unwrap_or(0);
        let decoded = char::from_u32(value)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{fffd}');
        return (decoded, len);
    }
    (after.chars().next().unwrap_or('\\'), len)
}

/// CSS の字に現れる名前の集まりを返す。
///
/// 識別子の連なりと、引用符の中の字（の全体と、その中の識別子の連なり）を集める。
/// escape は解いてから集める（`#a\:b` から `a:b`、`#\31 23` から `123` を得る）。
/// DOM の id が、CSS が選ぶ名前かどうかを見るために使う。
/// 多めに集めるのは安全側である（id を残すほうに倒れる）。
pub(crate) fn referenced_names(css: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut run = String::new();
    let flush = |run: &mut String, names: &mut HashSet<String>| {
        if !run.is_empty() {
            names.insert(std::mem::take(run));
        }
    };
    let mut i = 0;
    while i < css.len() {
        let rest = &css[i..];
        let c = rest.chars().next().expect("rest は空でない");
        if c == '\\' {
            let (decoded, len) = decode_escape(rest);
            run.push(decoded);
            i += len;
        } else if is_ident_char(c) {
            run.push(c);
            i += c.len_utf8();
        } else if c == '"' || c == '\'' {
            flush(&mut run, &mut names);
            i += 1;
            let mut inner = String::new();
            while i < css.len() {
                let tail = &css[i..];
                let d = tail.chars().next().expect("tail は空でない");
                if d == '\\' {
                    let (decoded, len) = decode_escape(tail);
                    inner.push(decoded);
                    i += len;
                } else {
                    i += d.len_utf8();
                    if d == c {
                        break;
                    }
                    inner.push(d);
                }
            }
            for word in inner.split(|ch: char| !is_ident_char(ch)) {
                if !word.is_empty() {
                    names.insert(word.to_string());
                }
            }
            if !inner.is_empty() {
                names.insert(inner);
            }
        } else {
            flush(&mut run, &mut names);
            i += c.len_utf8();
        }
    }
    flush(&mut run, &mut names);
    names
}

/// コメントと引用文字列の外にある、裸の識別子の集まりを返す（小文字にして）。
///
/// 裸とは、`.` `#` `:` `@` の直後でないことである。
/// class（`.link`）、id（`#head`）、擬似クラス（`a:link`）、at-rule は、要素の名前ではない。
/// 残る識別子は、type selector や、属性の名や、宣言の値である。
/// 要素の名前が CSS に現れるかを、多めに見積もるために使う。
/// 多めなのは安全側である（その要素を、落とさずに残すほうに倒れる）。
/// escape は解いてから集める（`scr\69 pt` から `script` を得る）。
pub(crate) fn bare_words(css: &str) -> HashSet<String> {
    let mut words = HashSet::new();
    let mut run = String::new();
    // 現在の識別子の連なりの直前の字。
    let mut before: Option<char> = None;
    let mut last: Option<char> = None;
    let flush = |run: &mut String, before: Option<char>, words: &mut HashSet<String>| {
        if !run.is_empty() {
            let word = std::mem::take(run);
            if !matches!(before, Some('.' | '#' | ':' | '@')) {
                words.insert(word.to_ascii_lowercase());
            }
        }
    };
    let mut i = 0;
    while i < css.len() {
        let rest = &css[i..];
        if let Some(after) = rest.strip_prefix("/*") {
            flush(&mut run, before, &mut words);
            i += 2 + after.find("*/").map_or(after.len(), |p| p + 2);
            last = None;
            continue;
        }
        let c = rest.chars().next().expect("rest は空でない");
        match c {
            '"' | '\'' => {
                flush(&mut run, before, &mut words);
                i += string_len(rest, c).unwrap_or(rest.len());
                last = None;
            }
            '\\' => {
                if run.is_empty() {
                    before = last;
                }
                let (decoded, len) = decode_escape(rest);
                run.push(decoded);
                i += len;
                last = Some('\\');
            }
            c if is_ident_char(c) => {
                if run.is_empty() {
                    before = last;
                }
                run.push(c);
                i += c.len_utf8();
                last = Some(c);
            }
            _ => {
                flush(&mut run, before, &mut words);
                i += c.len_utf8();
                last = Some(c);
            }
        }
    }
    flush(&mut run, before, &mut words);
    words
}

/// CSS が `attr()` で読む属性の名。
///
/// `attr()` は属性の値を `content` などに描く。
/// 読まれる属性の値は、字のまま CSS に見えるので、値を書き換える規則は、その属性に手を付けない。
#[derive(Debug, Default)]
pub(crate) struct AttrReads {
    /// 名前を読み取れない `attr()` があった。
    /// どの属性が読まれるか分からないので、すべて読まれるとみなす。
    any: bool,
    names: HashSet<String>,
}

impl AttrReads {
    /// この属性が `attr()` で読まれうるか。
    pub(crate) fn reads(&self, name: &str) -> bool {
        self.any || self.names.contains(&name.to_ascii_lowercase())
    }

    /// 名前を問わず、どれかが読まれうるか。
    pub(crate) fn reads_any_of(&self, names: &[&str]) -> bool {
        self.any || names.iter().any(|n| self.names.contains(*n))
    }
}

/// CSS の字にある `attr()` が読む属性の名を集める。
///
/// コメントや文字列の中の `attr(` も数える（多めに見積もるのは安全側である）。
pub(crate) fn attr_reads(css: &str) -> AttrReads {
    let mut reads = AttrReads::default();
    let lower = css.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("attr(") {
        let start = from + pos + 5;
        from = start;
        let tail = &css[start..];
        let tail = tail.trim_start_matches(is_css_whitespace);
        let name: String = tail
            .chars()
            .take_while(|&c| is_ident_char(c) || c == '|' || c == '\\')
            .collect();
        if name.is_empty() || name.contains('\\') {
            reads.any = true;
            continue;
        }
        let name = name.rsplit('|').next().unwrap_or_default();
        reads.names.insert(name.to_ascii_lowercase());
    }
    reads
}

/// CSS の字から読み取る事実。
///
/// DOM の規則のうち、CSS が読めば絵に出る差を捨てかねないものが、これを見て手を引く。
#[derive(Debug, Default)]
pub(crate) struct CssFacts {
    /// CSS の字に現れる名前（識別子と、引用符の中の字）。
    pub names: HashSet<String>,
    /// 裸の識別子（小文字）。
    pub bare_words: HashSet<String>,
    /// `attr()` が読む属性。
    pub attr_reads: AttrReads,
}

impl CssFacts {
    pub(crate) fn read(css: &str) -> Self {
        if css.is_empty() {
            return Self::default();
        }
        Self {
            names: referenced_names(css),
            bare_words: bare_words(css),
            attr_reads: attr_reads(css),
        }
    }
}

pub(crate) fn normalize(
    css: &str,
    rules: &NormalizeRules,
    scope_ids: &mut ScopeIds,
) -> Result<String, NormalizeError> {
    if rules.scope_ids {
        // 印を使う前に、この CSS の中の印を全部登録しておく。
        // `@keyframes` の名に付く印は、属性セレクターより前に現れることがあるため。
        pass(css, rules, scope_ids)?;
    }
    pass(css, rules, scope_ids)
}

fn pass(
    css: &str,
    rules: &NormalizeRules,
    scope_ids: &mut ScopeIds,
) -> Result<String, NormalizeError> {
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    // 直前の入力の字が識別子の字だったか（`data-v-` の前後の境界の判定に使う）。
    let mut prev_ident = false;
    // 直前が引用符の付く `url(` で、次の文字列がその引数か。
    let mut url_open = false;
    // 直前の escape が、空白で終わっていたか。
    let mut escape_ends_in_space = false;
    while i < css.len() {
        let rest = &css[i..];
        // 直前の escape が空白で終わっていても、その空白は escape の一部で、識別子との境である。
        // 次に来る空白やコメントは、それとは別に残す（畳むと、別の識別子になる）。
        let after_escape_space = std::mem::take(&mut escape_ends_in_space);
        if let Some(after) = rest.strip_prefix("/*") {
            let end = after
                .find("*/")
                .ok_or_else(|| err("閉じていないコメント", i))?;
            if !rules.drop_comments {
                out.push_str(&rest[..2 + end + 2]);
            } else if rules.collapse_css_whitespace {
                // コメントは、前後の字を繋げない区切りとして働くので、空白に置き換える。
                push_space(&mut out, after_escape_space);
            }
            i += 2 + end + 2;
            prev_ident = false;
            continue;
        }
        let c = rest.chars().next().expect("rest は空でない");
        match c {
            '"' | '\'' => {
                let len = string_len(rest, c).ok_or_else(|| err("閉じていない文字列", i))?;
                // 文字列は字のまま CSS に見える。
                // 置き換えてよいのは、`url()` の引数に限る。
                if url_open {
                    push_masked(&mut out, &rest[..len], rules);
                } else {
                    out.push_str(&rest[..len]);
                }
                i += len;
                prev_ident = false;
                url_open = false;
            }
            '\\' => {
                // escape は、後ろの 16 進と、その終わりの空白と一緒に、そのまま残す。
                // 終わりの空白は識別子の続きとの境で、畳むと別の識別子になる。
                let len = escape_len(rest);
                out.push_str(&rest[..len]);
                escape_ends_in_space = rest[..len].ends_with(is_css_whitespace);
                i += len;
                prev_ident = true;
                url_open = false;
            }
            c if is_css_whitespace(c) && rules.collapse_css_whitespace => {
                let len = rest
                    .find(|c: char| !is_css_whitespace(c))
                    .unwrap_or(rest.len());
                push_space(&mut out, after_escape_space);
                i += len;
                prev_ident = false;
            }
            _ => {
                if is_css_whitespace(c) {
                    // 畳まない設定での空白。`url(` と引数の文字列の間なら、引数の印を保つ。
                    out.push(c);
                    i += 1;
                    prev_ident = false;
                    continue;
                }
                if let Some(after) = strip_prefix_ignore_case(rest, "url(")
                    && !prev_ident
                {
                    let inner_start = rest.len() - after.len();
                    let trimmed = after.trim_start_matches(is_css_whitespace);
                    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
                        // 引用符の付く `url(` は、次の文字列が引数である。
                        out.push_str(&rest[..inner_start]);
                        i += inner_start;
                        prev_ident = false;
                        url_open = true;
                        continue;
                    }
                    // 引用符の無い `url(...)` の中身は、引数として扱う。
                    let close = after
                        .find(')')
                        .ok_or_else(|| err("閉じていない url()", i))?;
                    out.push_str(&rest[..inner_start]);
                    push_masked(&mut out, &after[..close], rules);
                    out.push(')');
                    i += inner_start + close + 1;
                    prev_ident = false;
                    url_open = false;
                    continue;
                }
                url_open = false;
                if rules.scope_ids
                    && let Some(n) = scope_suffix_number(rest, prev_ident, scope_ids)
                {
                    out.push_str(&format!("-#{n}"));
                    i += 9;
                    prev_ident = true;
                    continue;
                }
                if rules.bound_css_vars
                    && !prev_ident
                    && let Some(len) = bound_var_name_len(rest)
                {
                    let n = scope_ids.number_for_css_var(&rest[2..2 + len]);
                    out.push_str(&format!("--bound-#{n}"));
                    i += 2 + len;
                    prev_ident = true;
                    continue;
                }
                if rules.scope_ids
                    && !prev_ident
                    && let Some(len) = scope_selector_len(rest)
                {
                    let n = scope_ids.number_for(&rest[7..len]);
                    out.push_str(&format!("data-v-#{n}"));
                    i += len;
                    prev_ident = true;
                    continue;
                }
                out.push(c);
                i += c.len_utf8();
                prev_ident = is_ident_char(c);
            }
        }
    }
    Ok(if rules.collapse_css_whitespace {
        out.trim_matches(is_css_whitespace).to_string()
    } else {
        out
    })
}

/// 宣言の並び（`style` 属性の値など）を、コメント、引用文字列、`url()` の引数、それ以外に割って歩く。
///
/// - `plain` は、それ以外の字の先頭で呼ばれる。
///   置き換えるなら、置き換えた字と、読み進めた byte 数を返す。
///   識別子の位置だけに効く規則を、ここに渡す。
/// - `url_arg` は、`url()` の引数（引用符つきの文字列、または引用符の無い中身）で呼ばれる。
///   置き換えるなら、置き換えた字を返す。
///
/// コメントと、`url()` の引数でない文字列は、どちらの規則にも渡さず、字のまま残す。
/// 文字列の中の `--aaaaaaaa` は識別子ではなく、`content` に描かれる字だからである。
pub(crate) fn rewrite_declarations<'a>(
    text: &'a str,
    mut plain: impl FnMut(&str, bool) -> Option<(String, usize)>,
    mut url_arg: impl FnMut(&str) -> Option<String>,
) -> Cow<'a, str> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut i = 0;
    let mut prev_ident = false;
    let mut url_open = false;
    while i < text.len() {
        let rest = &text[i..];
        if let Some(after) = rest.strip_prefix("/*") {
            let len = after.find("*/").map_or(rest.len(), |p| 2 + p + 2);
            out.push_str(&rest[..len]);
            i += len;
            prev_ident = false;
            continue;
        }
        let c = rest.chars().next().expect("rest は空でない");
        match c {
            '"' | '\'' => {
                // 閉じていない文字列は、残りを字のまま残す。
                let len = string_len(rest, c).unwrap_or(rest.len());
                let literal = &rest[..len];
                match url_open.then(|| url_arg(literal)).flatten() {
                    Some(replaced) => {
                        out.push_str(&replaced);
                        changed = true;
                    }
                    None => out.push_str(literal),
                }
                i += len;
                prev_ident = false;
                url_open = false;
            }
            '\\' => {
                let len = escape_len(rest);
                out.push_str(&rest[..len]);
                i += len;
                prev_ident = true;
                url_open = false;
            }
            c if is_css_whitespace(c) => {
                out.push(c);
                i += 1;
                prev_ident = false;
            }
            _ => {
                if let Some(after) = strip_prefix_ignore_case(rest, "url(")
                    && !prev_ident
                {
                    let inner_start = rest.len() - after.len();
                    out.push_str(&rest[..inner_start]);
                    i += inner_start;
                    prev_ident = false;
                    let trimmed = after.trim_start_matches(is_css_whitespace);
                    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
                        url_open = true;
                        continue;
                    }
                    // 引用符の無い `url(...)`。閉じていなければ、残りを字のまま残す。
                    let close = after.find(')').unwrap_or(after.len());
                    let body = &after[..close];
                    match url_arg(body) {
                        Some(replaced) => {
                            out.push_str(&replaced);
                            changed = true;
                        }
                        None => out.push_str(body),
                    }
                    i += body.len();
                    url_open = false;
                    continue;
                }
                url_open = false;
                if let Some((replaced, consumed)) = plain(rest, prev_ident) {
                    out.push_str(&replaced);
                    i += consumed;
                    prev_ident = true;
                    changed = true;
                    continue;
                }
                out.push(c);
                i += c.len_utf8();
                prev_ident = is_ident_char(c);
            }
        }
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

/// 引用符で始まる文字列の byte 数（引用符を含む）。
/// 閉じていなければ `None`。
fn string_len(s: &str, quote: char) -> Option<usize> {
    let mut chars = s.char_indices().skip(1);
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c == quote {
            return Some(i + c.len_utf8());
        }
    }
    None
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// 先頭が `data-v-` に 8 桁の 16 進を続けた印で、その後ろが識別子の続きでなければ、その byte 数を返す。
fn scope_selector_len(s: &str) -> Option<usize> {
    let name = s.get(..15)?;
    scope_hex(name)?;
    let next = s[15..].chars().next();
    if next.is_some_and(is_ident_char) {
        return None;
    }
    Some(15)
}

fn push_masked(out: &mut String, text: &str, rules: &NormalizeRules) {
    if rules.ephemeral_origins {
        out.push_str(&mask_ephemeral_origins(text));
    } else {
        out.push_str(text);
    }
}

/// 空白を一つ書く。直前が空白なら、重ねない。
///
/// ただし、直前の空白が escape の終わりなら、それは空白ではなく識別子の一部なので、重ねて書く。
fn push_space(out: &mut String, after_escape_space: bool) {
    if after_escape_space || (!out.is_empty() && !out.ends_with(' ')) {
        out.push(' ');
    }
}
