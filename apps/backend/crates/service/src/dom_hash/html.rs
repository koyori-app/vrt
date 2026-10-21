//! ブラウザが直列化した HTML の字を、字句に割って、決まった形に書き戻す。
//!
//! 受け取る字は、ブラウザの直列化が出す形に限る。
//! 読めない字は推し量らずに断る（断れば、呼び手は hash を作らない）。

use super::NormalizeError;

/// 属性の名と値。
/// 値は実体参照を解いた後の字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attr {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    /// `<!doctype html>` の中身（`doctype html`）。
    Doctype(String),
    Start {
        name: String,
        attrs: Vec<Attr>,
    },
    End(String),
    /// 実体参照を解いた後の text。
    Text(String),
    /// `script` や `style` のように、中身を実体参照として読まない要素の中身。
    RawText(String),
    Comment(String),
}

/// 終了タグを持たない要素（空要素）。
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input",
    "keygen", "link", "meta", "param", "source", "track", "wbr",
];

/// 中身を字として読む要素。
/// 直列化は中身の字を実体参照にしない。
const RAW_TEXT_ELEMENTS: &[&str] = &[
    "script",
    "style",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "plaintext",
    "noscript",
];

pub(crate) fn is_void(name: &str) -> bool {
    VOID_ELEMENTS.iter().any(|v| v.eq_ignore_ascii_case(name))
}

fn is_raw_text(name: &str) -> bool {
    RAW_TEXT_ELEMENTS
        .iter()
        .any(|v| v.eq_ignore_ascii_case(name))
}

fn err(reason: &'static str, offset: usize) -> NormalizeError {
    NormalizeError::Html { reason, offset }
}

pub(crate) fn tokenize(input: &str) -> Result<Vec<Token>, NormalizeError> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            let end = input[i..].find('<').map_or(bytes.len(), |p| i + p);
            tokens.push(Token::Text(decode_entities(&input[i..end])));
            i = end;
            continue;
        }
        let rest = &input[i..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after
                .find("-->")
                .ok_or_else(|| err("閉じていないコメント", i))?;
            tokens.push(Token::Comment(after[..end].to_string()));
            i += 4 + end + 3;
            continue;
        }
        if let Some(after) = rest.strip_prefix("<!") {
            let end = after.find('>').ok_or_else(|| err("閉じていない宣言", i))?;
            tokens.push(Token::Doctype(after[..end].trim().to_string()));
            i += 2 + end + 1;
            continue;
        }
        if let Some(after) = rest.strip_prefix("</") {
            let end = after
                .find('>')
                .ok_or_else(|| err("閉じていない終了タグ", i))?;
            let name = after[..end].trim();
            if name.is_empty() || !name.bytes().all(is_name_byte) {
                return Err(err("終了タグの名が読めない", i));
            }
            tokens.push(Token::End(name.to_string()));
            i += 2 + end + 1;
            continue;
        }
        // 開始タグ。
        let (token, self_closing, consumed) = parse_start_tag(rest, i)?;
        i += consumed;
        let Token::Start { name, .. } = &token else {
            unreachable!("parse_start_tag は Start だけを返す");
        };
        let name = name.clone();
        tokens.push(token);
        if self_closing {
            if !is_void(&name) {
                tokens.push(Token::End(name));
            }
            continue;
        }
        if is_raw_text(&name) {
            let close = format!("</{}", name.to_ascii_lowercase());
            let end = find_ignore_ascii_case(&bytes[i..], close.as_bytes())
                .ok_or_else(|| err("閉じていない要素の中身", i))?;
            if end > 0 {
                tokens.push(Token::RawText(input[i..i + end].to_string()));
            }
            i += end;
        }
    }
    Ok(tokens)
}

/// 大文字小文字を区別せずに探す。
/// `needle` は小文字で渡す。
fn find_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')
}

/// 開始タグを一つ読む。
/// 返すのは、字句、自己終了か、読んだ byte 数。
fn parse_start_tag(rest: &str, base: usize) -> Result<(Token, bool, usize), NormalizeError> {
    let bytes = rest.as_bytes();
    // bytes[0] は '<'。
    if bytes.len() < 2 || !bytes[1].is_ascii_alphabetic() {
        return Err(err("実体参照になっていない `<`", base));
    }
    let mut p = 1;
    while p < bytes.len() && is_name_byte(bytes[p]) {
        p += 1;
    }
    let name = rest[1..p].to_string();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while p < bytes.len() && bytes[p].is_ascii_whitespace() {
            p += 1;
        }
        let Some(&b) = bytes.get(p) else {
            return Err(err("閉じていない開始タグ", base));
        };
        match b {
            b'>' => {
                p += 1;
                break;
            }
            b'/' => {
                if bytes.get(p + 1) == Some(&b'>') {
                    self_closing = true;
                    p += 2;
                    break;
                }
                p += 1;
            }
            _ => {
                let start = p;
                while p < bytes.len()
                    && !bytes[p].is_ascii_whitespace()
                    && !matches!(bytes[p], b'=' | b'>' | b'/')
                {
                    p += 1;
                }
                if p == start {
                    return Err(err("属性の名が読めない", base + p));
                }
                let attr_name = rest[start..p].to_string();
                while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                    p += 1;
                }
                let mut value = String::new();
                if bytes.get(p) == Some(&b'=') {
                    p += 1;
                    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                        p += 1;
                    }
                    match bytes.get(p) {
                        Some(&q) if q == b'"' || q == b'\'' => {
                            let close = rest[p + 1..]
                                .find(q as char)
                                .ok_or_else(|| err("閉じていない引用符", base + p))?;
                            value = decode_entities(&rest[p + 1..p + 1 + close]);
                            p += close + 2;
                        }
                        Some(_) => {
                            let vstart = p;
                            while p < bytes.len()
                                && !bytes[p].is_ascii_whitespace()
                                && bytes[p] != b'>'
                            {
                                p += 1;
                            }
                            value = decode_entities(&rest[vstart..p]);
                        }
                        None => return Err(err("属性の値が無い", base + p)),
                    }
                }
                attrs.push(Attr {
                    name: attr_name,
                    value,
                });
            }
        }
    }
    Ok((Token::Start { name, attrs }, self_closing, p))
}

/// 直列化が出す実体参照だけを解く。
/// 知らない名前の実体参照は、そのまま字として残す。
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let Some(semi) = rest.find(';').filter(|&n| n <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let body = &rest[1..semi];
        let decoded = match body {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => body
                .strip_prefix("#x")
                .or_else(|| body.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| body.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}

fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}

/// 字句を、決まった形の字に書き戻す。
///
/// 属性は常に二重引用符で括り、値の無い属性も `name=""` と書く。
/// 空要素は終了タグを書かない。
/// 改行は LF に揃える。
pub(crate) fn render(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Doctype(d) => {
                out.push_str("<!");
                out.push_str(d);
                out.push('>');
            }
            Token::Start { name, attrs } => {
                out.push('<');
                out.push_str(name);
                for a in attrs {
                    out.push(' ');
                    out.push_str(&a.name);
                    out.push_str("=\"");
                    escape_attr(&a.value, &mut out);
                    out.push('"');
                }
                out.push('>');
            }
            Token::End(name) => {
                if !is_void(name) {
                    out.push_str("</");
                    out.push_str(name);
                    out.push('>');
                }
            }
            Token::Text(t) => escape_text(t, &mut out),
            Token::RawText(t) => out.push_str(t),
            Token::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
        }
    }
    if out.contains('\r') {
        out = out.replace("\r\n", "\n").replace('\r', "\n");
    }
    out
}
