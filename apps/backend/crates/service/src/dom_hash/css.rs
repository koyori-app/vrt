//! CSSOM の字の正規化。
//!
//! CSS を構文木にはしない。
//! コメント、文字列、`url()` の中身、それ以外の三つに割って、それぞれに規則を当てる。
//! 規則の対象は、属性セレクターに現れる scoped style の印と、文字列や `url()` の中の origin だけである。
//! 規則を足しても、selector や宣言の意味は変えない。

use super::rules::{bound_var_name_len, mask_ephemeral_origins, scope_hex, scope_suffix_number};
use super::{NormalizeError, NormalizeRules, ScopeIds};

fn err(reason: &'static str, offset: usize) -> NormalizeError {
    NormalizeError::Css { reason, offset }
}

/// CSS の識別子に使える字か。
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
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
    while i < css.len() {
        let rest = &css[i..];
        if let Some(after) = rest.strip_prefix("/*") {
            let end = after
                .find("*/")
                .ok_or_else(|| err("閉じていないコメント", i))?;
            if !rules.drop_comments {
                out.push_str(&rest[..2 + end + 2]);
            } else if rules.collapse_css_whitespace {
                // コメントは、前後の字を繋げない区切りとして働くので、空白に置き換える。
                push_space(&mut out);
            }
            i += 2 + end + 2;
            prev_ident = false;
            continue;
        }
        let c = rest.chars().next().expect("rest は空でない");
        match c {
            '"' | '\'' => {
                let len = string_len(rest, c).ok_or_else(|| err("閉じていない文字列", i))?;
                push_masked(&mut out, &rest[..len], rules);
                i += len;
                prev_ident = false;
            }
            '\\' => {
                // エスケープは、次の字と一緒にそのまま残す。
                let next = rest[1..].chars().next();
                let len = 1 + next.map_or(0, char::len_utf8);
                out.push_str(&rest[..len]);
                i += len;
                prev_ident = true;
            }
            c if c.is_whitespace() && rules.collapse_css_whitespace => {
                let len = rest
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(rest.len());
                push_space(&mut out);
                i += len;
                prev_ident = false;
            }
            _ => {
                if let Some(after) = strip_prefix_ignore_case(rest, "url(")
                    && !prev_ident
                {
                    // 引用符の無い `url(...)` の中身は、文字列と同じに扱う。
                    let inner_start = rest.len() - after.len();
                    let trimmed = after.trim_start();
                    if !trimmed.starts_with('"') && !trimmed.starts_with('\'') {
                        let close = after
                            .find(')')
                            .ok_or_else(|| err("閉じていない url()", i))?;
                        out.push_str(&rest[..inner_start]);
                        push_masked(&mut out, &after[..close], rules);
                        out.push(')');
                        i += inner_start + close + 1;
                        prev_ident = false;
                        continue;
                    }
                }
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
        out.trim().to_string()
    } else {
        out
    })
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

fn push_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}
