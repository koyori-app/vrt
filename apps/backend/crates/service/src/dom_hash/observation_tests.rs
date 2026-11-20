//! 「正規化が捨ててよいのは、CSS から観測できぬ差だけである」の試験。
//!
//! 規則ごとの試験（`tests.rs`）は、規則が効くことを見る。
//! ここでは逆に、規則が効きすぎないことを、二つの表で見る。
//!
//! - [`INVISIBLE`]：CSS から観測できない差。hash が一致しなければならない。
//! - [`VISIBLE`]：CSS から観測できる差。hash が割れなければならない。
//!
//! 表の各行は、その差を捨てうる規則の名（[`NormalizeRules`] の欄の名）を持つ。
//! 規則を足すと [`every_rule_is_exercised_on_both_sides`] が落ち、足した規則の行を両側に書くまで通らない。
//! 「観測できる差を捨てる規則」が増えた時に、その差を表に書いていなければ、気づく手が無い。
//! 行を書く時は、その差を CSS で観測する手（selector、`content`、`attr()`、`white-space` など）を、行の名に書く。

use super::*;

/// 一つの対。
/// 二つの入力の組（DOM と CSSOM）が、規則の下でどうなるべきかを、表のどちらに置くかで示す。
struct Pair {
    /// 差を捨てうる規則の欄の名。`roundtrip` は、規則を入れても入れなくても働く書き戻し。
    rule: &'static str,
    /// 何が違い、CSS からどう観測できる（またはできない）か。
    name: &'static str,
    a: (&'static str, &'static str),
    b: (&'static str, &'static str),
}

const fn pair(
    rule: &'static str,
    name: &'static str,
    a: (&'static str, &'static str),
    b: (&'static str, &'static str),
) -> Pair {
    Pair { rule, name, a, b }
}

/// 規則の欄の名。`NormalizeRules` の欄と一対一である。
const FIELDS: [&str; 9] = [
    "drop_comments",
    "drop_non_rendering",
    "scope_ids",
    "bound_css_vars",
    "generated_ids",
    "referenced_ids",
    "ephemeral_origins",
    "sort_attributes",
    "collapse_css_whitespace",
];

fn without(field: &str) -> NormalizeRules {
    let mut rules = NormalizeRules::all();
    match field {
        "drop_comments" => rules.drop_comments = false,
        "drop_non_rendering" => rules.drop_non_rendering = false,
        "scope_ids" => rules.scope_ids = false,
        "bound_css_vars" => rules.bound_css_vars = false,
        "generated_ids" => rules.generated_ids = false,
        "referenced_ids" => rules.referenced_ids = false,
        "ephemeral_origins" => rules.ephemeral_origins = false,
        "sort_attributes" => rules.sort_attributes = false,
        "collapse_css_whitespace" => rules.collapse_css_whitespace = false,
        other => panic!("知らない規則の欄: {other}"),
    }
    rules
}

fn pair_hashes(p: &Pair, rules: &NormalizeRules) -> (DomCssomHashes, DomCssomHashes) {
    let a = hash_dom_and_cssom_with(p.a.0, p.a.1, rules).expect("a を正規化できる");
    let b = hash_dom_and_cssom_with(p.b.0, p.b.1, rules).expect("b を正規化できる");
    (a, b)
}

fn same(a: &DomCssomHashes, b: &DomCssomHashes) -> bool {
    a.dom_hash == b.dom_hash && a.cssom_hash == b.cssom_hash
}

/// CSS から観測できない差。
/// 捨ててよい。hash は一致する。
const INVISIBLE: &[Pair] = &[
    pair(
        "roundtrip",
        "属性の引用符の種類と、値の無い属性の書き方は、DOM の値を変えない",
        ("<input disabled title='a&#39;b'>", "input[disabled]{x:y}"),
        (
            "<input title=\"a'b\" disabled=\"\">",
            "input[disabled]{x:y}",
        ),
    ),
    pair(
        "drop_comments",
        "コメントノードは、兄弟として数えられず、:empty も崩さない",
        (
            "<ul><!--a--><li>x</li></ul><p><!--b--></p>",
            "li:first-child,p:empty{color:red}",
        ),
        (
            "<ul><li>x</li></ul><p></p>",
            "li:first-child,p:empty{color:red}",
        ),
    ),
    pair(
        "drop_comments",
        "CSS のコメントは、宣言の意味を変えない",
        ("<p>x</p>", "a{ /* a */ color:red;margin:0}"),
        ("<p>x</p>", "a{ color:red;margin:0}"),
    ),
    pair(
        "drop_non_rendering",
        "描画しない要素の属性（build ごとに変わる hash）と中身は、同じ位置にある限り選ばれない",
        (
            "<head><link rel=\"stylesheet\" href=\"/a-1.css\"></head><body><div><script src=\"/a-1.js\">1</script><p>x</p></div></body>",
            "p:first-child{color:red}",
        ),
        (
            "<head><link rel=\"stylesheet\" href=\"/a-2.css\"><meta charset=\"utf-8\"></head><body><div><script src=\"/a-2.js\">2</script><p>x</p></div></body>",
            "p:first-child{color:red}",
        ),
    ),
    pair(
        "drop_non_rendering",
        "class や擬似クラスに link、head という語があっても、要素の名前を書いたことにはならない",
        (
            "<head><link href=\"/a.css\"></head><body><a>x</a></body>",
            ".link,.head,a:link{color:red}",
        ),
        (
            "<head><link href=\"/b.css\"></head><body><a>x</a></body>",
            ".link,.head,a:link{color:red}",
        ),
    ),
    pair(
        "scope_ids",
        "scoped style の印の 16 進は、DOM と CSSOM で同じ表に置き換わる",
        (
            "<div data-v-58aa11bb=\"\"><span data-v-0c0c0c0c=\"\">x</span></div>",
            ".a[data-v-58aa11bb]{animation:s-58aa11bb 1s}@keyframes s-58aa11bb{0%{opacity:0}}",
        ),
        (
            "<div data-v-a9ff22cc=\"\"><span data-v-ffeeddcc=\"\">x</span></div>",
            ".a[data-v-a9ff22cc]{animation:s-a9ff22cc 1s}@keyframes s-a9ff22cc{0%{opacity:0}}",
        ),
    ),
    pair(
        "bound_css_vars",
        "v-bind() の変数名は、style 属性と var() で同じ表に置き換わる",
        (
            "<div style=\"--v5ad87c78: 1px\">x</div>",
            "div{width:var(--v5ad87c78)}",
        ),
        (
            "<div style=\"--cc7b5cd0: 1px\">x</div>",
            "div{width:var(--cc7b5cd0)}",
        ),
    ),
    pair(
        "generated_ids",
        "自動で生える id の形の名前は、誰も参照しておらず、CSS も書かない限り描かれない",
        ("<input id=\":r1:\">", "input{color:red}"),
        ("<input id=\":r7:\">", "input{color:red}"),
    ),
    pair(
        "referenced_ids",
        "参照で結ばれた id は、CSS が名前を書かない限り、名前を問わない",
        (
            "<label for=\"cm-ac-91f3\">a</label><input id=\"cm-ac-91f3\" aria-controls=\"cm-ac-91f3\">",
            "label{color:red}",
        ),
        (
            "<label for=\"cm-ac-2b07\">a</label><input id=\"cm-ac-2b07\" aria-controls=\"cm-ac-2b07\">",
            "label{color:red}",
        ),
    ),
    pair(
        "ephemeral_origins",
        "URL を取る属性の loopback の port は、描かれない",
        (
            "<img src=\"http://127.0.0.1:4101/a.png\">",
            "img{width:1px}",
        ),
        (
            "<img src=\"http://127.0.0.1:50999/a.png\">",
            "img{width:1px}",
        ),
    ),
    pair(
        "ephemeral_origins",
        "CSS の url() の引数（引用符の有無を問わず）の loopback の port は、描かれない",
        (
            "<p>x</p>",
            "a{background:url(http://127.0.0.1:4101/a.png)}b{background:url(\"http://127.0.0.1:4101/b.png\")}",
        ),
        (
            "<p>x</p>",
            "a{background:url(http://127.0.0.1:50999/a.png)}b{background:url(\"http://127.0.0.1:50999/b.png\")}",
        ),
    ),
    pair(
        "ephemeral_origins",
        "style 属性の url() の引数の loopback の port は、描かれない",
        (
            "<div style=\"background:url('http://127.0.0.1:4101/b.png')\">x</div>",
            "",
        ),
        (
            "<div style=\"background:url('http://127.0.0.1:50999/b.png')\">x</div>",
            "",
        ),
    ),
    pair(
        "sort_attributes",
        "属性の並びは、CSS から見えない",
        (
            "<p id=\"x\" class=\"a b\" title=\"t\" data-k=\"v\">x</p>",
            "#x.a.b[title=t][data-k=v]{color:red}",
        ),
        (
            "<p data-k=\"v\" title=\"t\" class=\"a b\" id=\"x\">x</p>",
            "#x.a.b[title=t][data-k=v]{color:red}",
        ),
    ),
    pair(
        "collapse_css_whitespace",
        "CSS の空白（space、tab、LF、CR、FF）の連なりは、一つの空白と同じに読まれる",
        ("<p>x</p>", "a > b  {\n color :\t red ;\n}"),
        ("<p>x</p>", "a >\r\nb {\x0c color : red ; }"),
    ),
];

/// CSS から観測できる差。
/// 捨ててはならない。hash は割れる。
const VISIBLE: &[Pair] = &[
    pair(
        "roundtrip",
        "CR と LF は、white-space: pre の下で空白と改行になる",
        ("<pre>a\rb</pre>", "pre{white-space:pre}"),
        ("<pre>a\nb</pre>", "pre{white-space:pre}"),
    ),
    pair(
        "roundtrip",
        "NBSP と space は、属性セレクターが字として比べる",
        ("<p title=\"a&nbsp;b\">x</p>", "[title=\"a b\"]{color:red}"),
        ("<p title=\"a b\">x</p>", "[title=\"a b\"]{color:red}"),
    ),
    pair(
        "drop_comments",
        "コメントに見える字は text であり、描かれる",
        ("<p>&lt;!-- a --&gt;</p>", ""),
        ("<p>&lt;!-- b --&gt;</p>", ""),
    ),
    pair(
        "drop_comments",
        "文字列の中の /* */ は content に描かれる",
        ("<p>x</p>", "p::before{content:\"/* a */\"}"),
        ("<p>x</p>", "p::before{content:\"/* b */\"}"),
    ),
    pair(
        "drop_comments",
        "コメントは前後の字を繋げない（1px/**/2px と 1px2px は別の値）",
        ("<p>x</p>", "p{margin:1px/**/2px}"),
        ("<p>x</p>", "p{margin:1px2px}"),
    ),
    pair(
        "drop_non_rendering",
        ":first-child は描画しない兄弟も数える",
        (
            "<div><script></script><p>x</p></div>",
            "p:first-child{color:red}",
        ),
        ("<div><p>x</p></div>", "p:first-child{color:red}"),
    ),
    pair(
        "drop_non_rendering",
        ":nth-child は描画しない兄弟も数える",
        (
            "<p>a</p><style></style><p>b</p>",
            "p:nth-child(2){color:red}",
        ),
        ("<p>a</p><p>b</p>", "p:nth-child(2){color:red}"),
    ),
    pair(
        "drop_non_rendering",
        ":empty は描画しない子も数える",
        ("<div><link></div>", "div:empty{color:red}"),
        ("<div></div>", "div:empty{color:red}"),
    ),
    pair(
        "drop_non_rendering",
        ":has() は描画しない子孫にも当たる",
        (
            "<div><noscript></noscript></div>",
            "div:has(noscript){color:red}",
        ),
        ("<div></div>", "div:has(noscript){color:red}"),
    ),
    pair(
        "drop_non_rendering",
        "CSS が script を表示にすれば、中身は描かれる",
        ("<div><script>a</script></div>", "script{display:block}"),
        ("<div><script>b</script></div>", "script{display:block}"),
    ),
    pair(
        "drop_non_rendering",
        "CSS が link を名指して属性で選べば、属性は観測される",
        (
            "<div><link rel=\"a\"></div>",
            "link[rel=\"a\"]{display:block}",
        ),
        (
            "<div><link rel=\"b\"></div>",
            "link[rel=\"a\"]{display:block}",
        ),
    ),
    pair(
        "scope_ids",
        "どの要素にどの scope の規則が効くかの対応",
        (
            "<div data-v-aaaaaaaa=\"\"><p data-v-aaaaaaaa=\"\">x</p></div>",
            ".a[data-v-aaaaaaaa]{x:y}",
        ),
        (
            "<div data-v-aaaaaaaa=\"\"><p data-v-bbbbbbbb=\"\">x</p></div>",
            ".a[data-v-aaaaaaaa]{x:y}",
        ),
    ),
    pair(
        "scope_ids",
        "style 属性の引用文字列の中の、印に似た字は content に描かれる",
        (
            "<div data-v-58152a41=\"\" style=\"--l:'x-58152a41'\">x</div>",
            "div::before{content:var(--l)}",
        ),
        (
            "<div data-v-58152a41=\"\" style=\"--l:'x-#1'\">x</div>",
            "div::before{content:var(--l)}",
        ),
    ),
    pair(
        "scope_ids",
        "@keyframes の名は大文字と小文字を区別する",
        (
            "<div data-v-58152a41=\"\">x</div>",
            "@keyframes s-58152a41{0%{opacity:0}}.a{animation:s-58152a41 1s}",
        ),
        (
            "<div data-v-58152a41=\"\">x</div>",
            "@keyframes s-58152a41{0%{opacity:0}}.a{animation:s-58152A41 1s}",
        ),
    ),
    pair(
        "bound_css_vars",
        "style 属性の引用文字列の中の、変数名に似た字は content に描かれる",
        (
            "<div style=\"--label: '--aaaaaaaa'\">x</div>",
            "div::before{content:var(--label)}",
        ),
        (
            "<div style=\"--label: '--bbbbbbbb'\">x</div>",
            "div::before{content:var(--label)}",
        ),
    ),
    pair(
        "bound_css_vars",
        "style 属性の url() の中の、変数名に似た字は URL として読まれる",
        ("<div style=\"background:url(--aaaaaaaa.png)\">x</div>", ""),
        ("<div style=\"background:url(--bbbbbbbb.png)\">x</div>", ""),
    ),
    pair(
        "bound_css_vars",
        "custom property の名は大文字と小文字を区別する（二つの変数と、一つの変数）",
        (
            "<p>x</p>",
            "a{color:var(--V5AD87C78);margin:var(--v5ad87c78)}",
        ),
        (
            "<p>x</p>",
            "a{color:var(--v5ad87c78);margin:var(--v5ad87c78)}",
        ),
    ),
    pair(
        "generated_ids",
        "参照が別の要素を指す",
        (
            "<label for=\":r1:\">a</label><input id=\":r1:\"><input id=\":r2:\">",
            "",
        ),
        (
            "<label for=\":r2:\">a</label><input id=\":r1:\"><input id=\":r2:\">",
            "",
        ),
    ),
    pair(
        "generated_ids",
        "CSS が自動 id の形の字を選ぶ",
        ("<input id=\":r1:\">", "[id=\":r1:\"]{color:red}"),
        ("<input id=\":r7:\">", "[id=\":r1:\"]{color:red}"),
    ),
    pair(
        "referenced_ids",
        "CSS が id を選ぶ",
        (
            "<label for=\"a1\">x</label><input id=\"a1\">",
            "#a1{color:red}",
        ),
        (
            "<label for=\"b1\">x</label><input id=\"b1\">",
            "#a1{color:red}",
        ),
    ),
    pair(
        "referenced_ids",
        "CSS が id を 16 進の escape で選ぶ",
        (
            "<div id=\"123\" aria-labelledby=\"123\">x</div>",
            "#\\31 23{color:red}",
        ),
        (
            "<div id=\"124\" aria-labelledby=\"124\">x</div>",
            "#\\31 23{color:red}",
        ),
    ),
    pair(
        "referenced_ids",
        "id の並びの空白の数は、属性セレクターが字として比べる",
        (
            "<i id=\"x\"></i><i id=\"y\"></i><p aria-labelledby=\"x  y\">z</p>",
            "[aria-labelledby=\"x y\"]{color:red}",
        ),
        (
            "<i id=\"x\"></i><i id=\"y\"></i><p aria-labelledby=\"x y\">z</p>",
            "[aria-labelledby=\"x y\"]{color:red}",
        ),
    ),
    pair(
        "referenced_ids",
        "attr(id) は id の名前を描く",
        (
            "<label for=\"a\">x</label><input id=\"a\">",
            "input::after{content:attr(id)}",
        ),
        (
            "<label for=\"b\">x</label><input id=\"b\">",
            "input::after{content:attr(id)}",
        ),
    ),
    pair(
        "ephemeral_origins",
        "content の文字列の中の URL は描かれる",
        ("<p>x</p>", "p::before{content:\"http://127.0.0.1:4101/\"}"),
        ("<p>x</p>", "p::before{content:\"http://127.0.0.1:50999/\"}"),
    ),
    pair(
        "ephemeral_origins",
        "style 属性の引用文字列の中の URL は、content に描かれる",
        (
            "<div style=\"--u:'http://127.0.0.1:4101/'\">x</div>",
            "div::before{content:var(--u)}",
        ),
        (
            "<div style=\"--u:'http://127.0.0.1:50999/'\">x</div>",
            "div::before{content:var(--u)}",
        ),
    ),
    pair(
        "ephemeral_origins",
        "attr(href) は href の字を描く",
        (
            "<a href=\"http://127.0.0.1:4101/x\">x</a>",
            "a::after{content:attr(href)}",
        ),
        (
            "<a href=\"http://127.0.0.1:50999/x\">x</a>",
            "a::after{content:attr(href)}",
        ),
    ),
    pair(
        "ephemeral_origins",
        "同じ origin の別の path",
        ("<img src=\"http://127.0.0.1:4101/a.png\">", ""),
        ("<img src=\"http://127.0.0.1:4101/b.png\">", ""),
    ),
    pair(
        "sort_attributes",
        "class の語の順は、[class=\"a b\"] が字として比べる",
        (
            "<div class=\"a b\">x</div><div class=\"b a\">y</div>",
            "[class=\"a b\"]{color:red}[class=\"b a\"]{color:blue}",
        ),
        (
            "<div class=\"b a\">x</div><div class=\"a b\">y</div>",
            "[class=\"a b\"]{color:red}[class=\"b a\"]{color:blue}",
        ),
    ),
    pair(
        "sort_attributes",
        "class の空白の数は、属性セレクターが字として比べる",
        ("<div class=\"a  b\">x</div>", "[class=\"a b\"]{color:red}"),
        ("<div class=\"a b\">x</div>", "[class=\"a b\"]{color:red}"),
    ),
    pair(
        "collapse_css_whitespace",
        "NBSP は CSS の空白ではなく、識別子の一部になる",
        ("<div class=\"a&nbsp;b\">x</div>", ".a\u{a0}b{color:red}"),
        ("<div class=\"a&nbsp;b\">x</div>", ".a b{color:red}"),
    ),
    pair(
        "collapse_css_whitespace",
        "NBSP 以外の非 ASCII の空白（全角空白、EM SPACE、NEL）も識別子の一部になる",
        ("<p>x</p>", ".a\u{3000}b{x:y}.c\u{2003}d{x:y}.e\u{85}f{x:y}"),
        ("<p>x</p>", ".a b{x:y}.c d{x:y}.e f{x:y}"),
    ),
    pair(
        "collapse_css_whitespace",
        "VT は CSS の空白ではない",
        ("<p>x</p>", ".a\x0bb{x:y}"),
        ("<p>x</p>", ".a b{x:y}"),
    ),
    pair(
        "collapse_css_whitespace",
        "末尾の NBSP は trim されない",
        ("<p>x</p>", "a{}\u{a0}"),
        ("<p>x</p>", "a{}"),
    ),
    pair(
        "collapse_css_whitespace",
        "16 進の escape の終わりの空白は、識別子の続きとの境である",
        ("<p>x</p>", "#\\31  p{color:red}"),
        ("<p>x</p>", "#\\31 p{color:red}"),
    ),
    pair(
        "collapse_css_whitespace",
        "文字列の中の空白は content に描かれる",
        ("<p>x</p>", "p::before{content:\"a  b\"}"),
        ("<p>x</p>", "p::before{content:\"a b\"}"),
    ),
];

#[test]
fn invisible_differences_are_normalized_away() {
    for p in INVISIBLE {
        let (a, b) = pair_hashes(p, &NormalizeRules::all());
        assert!(same(&a, &b), "一致するはず: [{}] {}", p.rule, p.name);
    }
}

#[test]
fn visible_differences_survive_normalization() {
    for p in VISIBLE {
        let (a, b) = pair_hashes(p, &NormalizeRules::all());
        assert!(!same(&a, &b), "割れるはず: [{}] {}", p.rule, p.name);
    }
}

#[test]
fn the_pairs_really_differ_before_any_rule_is_applied() {
    // 割れるはずの対が、規則の前から別の入力であることの確かめ。
    // 入力が同じなら、「割れる」という試験は何も縛らない。
    for p in VISIBLE {
        let (a, b) = pair_hashes(p, &NormalizeRules::none());
        assert!(
            !same(&a, &b),
            "規則なしでも別の入力のはず: [{}] {}",
            p.rule,
            p.name
        );
    }
}

#[test]
fn each_invisible_pair_depends_on_its_rule() {
    // 一致するはずの対が、その規則を外すと割れること。
    // 規則が実際に効いている対だけを、表に置く。
    for p in INVISIBLE.iter().filter(|p| p.rule != "roundtrip") {
        let (a, b) = pair_hashes(p, &without(p.rule));
        assert!(
            !same(&a, &b),
            "規則 {} を外せば割れるはず: {}",
            p.rule,
            p.name
        );
    }
}

#[test]
fn every_rule_is_exercised_on_both_sides() {
    // 欄を足すと、この分解が通らなくなる。
    // 足した規則を FIELDS と `without` に書き、両側の表に行を足すまで、進めない。
    let NormalizeRules {
        drop_comments: _,
        drop_non_rendering: _,
        scope_ids: _,
        bound_css_vars: _,
        generated_ids: _,
        referenced_ids: _,
        ephemeral_origins: _,
        sort_attributes: _,
        collapse_css_whitespace: _,
    } = NormalizeRules::all();
    assert_eq!(FIELDS.len(), 9);
    for field in FIELDS {
        assert!(
            INVISIBLE.iter().any(|p| p.rule == field),
            "{field}: 一致するはずの対が無い"
        );
        assert!(
            VISIBLE.iter().any(|p| p.rule == field),
            "{field}: 割れるはずの対が無い"
        );
    }
    for p in INVISIBLE.iter().chain(VISIBLE) {
        assert!(
            p.rule == "roundtrip" || FIELDS.contains(&p.rule),
            "知らない規則の名: {}",
            p.rule
        );
    }
}
