//! 規則ごとの試験。
//!
//! 規則ごとに、「これは落とす」と「これは落とさぬ」の対を置く。
//! 落としすぎると本当の変化を見逃し、落とさなすぎると毎回不一致になる。
//! 片方だけでは、どちらかに倒れた実装を見分けられない。

use super::*;

fn all() -> NormalizeRules {
    NormalizeRules::all()
}

/// 規則を一つだけ入れた組。
fn only(set: impl FnOnce(&mut NormalizeRules)) -> NormalizeRules {
    let mut rules = NormalizeRules::none();
    set(&mut rules);
    rules
}

fn dom(html: &str, rules: &NormalizeRules) -> String {
    normalize_dom(html, rules, &mut ScopeIds::new()).expect("正規化できる入力")
}

fn css(text: &str, rules: &NormalizeRules) -> String {
    normalize_cssom(text, rules, &mut ScopeIds::new()).expect("正規化できる入力")
}

fn hashes(html: &str, text: &str) -> DomCssomHashes {
    hash_dom_and_cssom(html, text).expect("hash を作れる入力")
}

// 字句に割って書き戻す

#[test]
fn rendering_writes_attributes_in_one_canonical_form() {
    let rules = NormalizeRules::none();
    assert_eq!(
        dom("<input disabled value='a&amp;b' class=x><br/>", &rules),
        r#"<input disabled="" value="a&amp;b" class="x"><br>"#
    );
}

#[test]
fn entities_are_decoded_and_written_back_in_one_form() {
    let rules = NormalizeRules::none();
    assert_eq!(
        dom(
            "<p title=\"&quot;q&quot; &#38; &#x26;\">&lt;&gt;&amp;&nbsp;</p>",
            &rules
        ),
        "<p title=\"&quot;q&quot; &amp; &amp;\">&lt;&gt;&amp;&nbsp;</p>"
    );
}

#[test]
fn an_unknown_entity_is_kept_as_text_and_stays_distinct() {
    let rules = NormalizeRules::none();
    let a = dom("<p>&foo;</p>", &rules);
    let b = dom("<p>&bar;</p>", &rules);
    assert_ne!(a, b);
}

#[test]
fn line_endings_are_unified() {
    let rules = NormalizeRules::none();
    assert_eq!(dom("<p>a\r\nb\rc</p>", &rules), "<p>a\nb\nc</p>");
}

#[test]
fn text_whitespace_is_kept() {
    // pre や white-space の指定で描画が変わるので、text の空白は畳まない。
    let rules = all();
    assert_ne!(dom("<p>a b</p>", &rules), dom("<p>a  b</p>", &rules));
}

#[test]
fn tag_and_attribute_name_case_is_kept() {
    // ブラウザの直列化は svg の属性の大小を保つ。
    // 小文字へ畳むと別の属性を同じにする。
    let rules = all();
    assert_ne!(
        dom("<svg viewBox=\"0 0 1 1\"></svg>", &rules),
        dom("<svg viewbox=\"0 0 1 1\"></svg>", &rules)
    );
}

#[test]
fn multibyte_text_and_attributes_do_not_panic() {
    let rules = all();
    let out = dom(
        "<p title=\"日本語\" 属性=\"値\">日本語 &#x3042;</p>",
        &rules,
    );
    assert!(out.contains("日本語"));
    assert!(out.contains("あ"));
}

#[test]
fn unreadable_html_is_refused_instead_of_guessed() {
    let rules = all();
    for bad in [
        "<!-- never closed",
        "<p title=\"never closed>x</p>",
        "<p",
        "a < b",
        "<script>never closed",
        "<p title=",
        "</",
        "<!doctype",
    ] {
        let result = normalize_dom(bad, &rules, &mut ScopeIds::new());
        assert!(
            matches!(result, Err(NormalizeError::Html { .. })),
            "{bad:?} は断る: {result:?}"
        );
    }
}

#[test]
fn garbage_never_panics() {
    let rules = all();
    for input in [
        "",
        "<",
        "<<<",
        "&",
        "&#;",
        "&#x;",
        "&#xZZ;",
        "&#99999999999;",
        "<a =b>",
        "<a ==b>",
        "<a / / >",
        "<é>",
        "<p>é",
        "<p é=é>",
        "</é>",
        "<a b='>",
        "<!>",
    ] {
        let _ = normalize_dom(input, &rules, &mut ScopeIds::new());
    }
}

// 規則: コメントを落とす

#[test]
fn comments_are_dropped() {
    let rules = only(|r| r.drop_comments = true);
    assert_eq!(
        dom("<div><!--v-if--><p>a</p><!----></div>", &rules),
        "<div><p>a</p></div>"
    );
}

#[test]
fn text_that_looks_like_a_comment_is_kept() {
    let rules = only(|r| r.drop_comments = true);
    assert_eq!(
        dom("<p>&lt;!-- a --&gt;</p>", &rules),
        "<p>&lt;!-- a --&gt;</p>"
    );
}

#[test]
fn css_comments_are_dropped_but_not_the_declarations_around_them() {
    let rules = all();
    assert_eq!(
        css("a{color:red;/* note */background:blue}", &rules),
        "a{color:red; background:blue}"
    );
}

#[test]
fn a_comment_in_css_still_separates_tokens() {
    // コメントで区切られた二つの字を、一つの識別子にくっつけない。
    let rules = all();
    assert_ne!(css("a/**/b{}", &rules), css("ab{}", &rules));
}

// 規則: 描画しない要素を落とす

#[test]
fn script_link_style_and_head_are_dropped_with_their_contents() {
    let rules = only(|r| r.drop_non_rendering = true);
    let html = concat!(
        "<head><meta charset=\"utf-8\"><title>t</title></head>",
        "<body><script src=\"/assets/iframe-ab12cd.js\"></script>",
        "<link rel=\"stylesheet\" href=\"/assets/x-9f8e.css\">",
        "<style>.a{color:red}</style>",
        "<noscript><b>x</b></noscript>",
        "<p>keep</p></body>"
    );
    assert_eq!(dom(html, &rules), "<body><p>keep</p></body>");
}

#[test]
fn a_script_body_with_markup_inside_does_not_leak() {
    let rules = only(|r| r.drop_non_rendering = true);
    assert_eq!(
        dom("<script>var a = '<p>x</p>';</script><p>y</p>", &rules),
        "<p>y</p>"
    );
}

#[test]
fn a_title_inside_the_body_is_kept() {
    // svg の title は描画に関わりうる（Storybook の図にも在る）。
    // head の title だけを落とす。
    let rules = only(|r| r.drop_non_rendering = true);
    assert_eq!(
        dom("<body><svg><title>icon</title></svg></body>", &rules),
        "<body><svg><title>icon</title></svg></body>"
    );
}

#[test]
fn custom_elements_that_only_start_with_a_dropped_name_are_kept() {
    let rules = only(|r| r.drop_non_rendering = true);
    assert_eq!(
        dom(
            "<styled-box>a</styled-box><link-card>b</link-card><scripted>c</scripted>",
            &rules
        ),
        "<styled-box>a</styled-box><link-card>b</link-card><scripted>c</scripted>"
    );
}

#[test]
fn dropping_a_head_does_not_swallow_the_body_that_follows() {
    let rules = only(|r| r.drop_non_rendering = true);
    assert_eq!(
        dom(
            "<html lang=\"ja\"><head><style>a{}</style></head><body>x</body></html>",
            &rules
        ),
        "<html lang=\"ja\"><body>x</body></html>"
    );
}

#[test]
fn html_element_attributes_are_kept() {
    // theme の切り替えなどは html の属性に出て、絵を変える。
    let rules = all();
    assert_ne!(
        dom("<html class=\"dark\"><body>x</body></html>", &rules),
        dom("<html class=\"light\"><body>x</body></html>", &rules)
    );
}

// 規則: scoped style の印

#[test]
fn scope_ids_become_numbers_in_order_of_appearance() {
    let rules = only(|r| r.scope_ids = true);
    assert_eq!(
        dom(
            "<div data-v-a9f3c2d1=\"\"><p data-v-0b1c2d3e=\"\" data-v-a9f3c2d1=\"\">x</p></div>",
            &rules
        ),
        "<div data-v-#1=\"\"><p data-v-#2=\"\" data-v-#1=\"\">x</p></div>"
    );
}

#[test]
fn two_builds_with_different_scope_ids_normalize_to_the_same_text() {
    let rules = all();
    let build_a = "<div data-v-58aa11bb=\"\"><span data-v-0c0c0c0c=\"\">x</span></div>";
    let build_b = "<div data-v-a9ff22cc=\"\"><span data-v-ffeeddcc=\"\">x</span></div>";
    assert_eq!(dom(build_a, &rules), dom(build_b, &rules));
}

#[test]
fn numbering_does_not_depend_on_the_order_of_the_hex() {
    // 並べ替えを先にすると、16 進の大小で番号が決まり、build ごとに結果が変わる。
    // 同じ構造で、二つの印の 16 進の大小だけが逆になった二つの build を並べる。
    let rules = all();
    let build_a =
        "<div data-v-11111111=\"\" data-v-99999999=\"\"><p data-v-99999999=\"\">x</p></div>";
    let build_b =
        "<div data-v-99999999=\"\" data-v-11111111=\"\"><p data-v-11111111=\"\">x</p></div>";
    assert_eq!(dom(build_a, &rules), dom(build_b, &rules));
    assert_eq!(
        dom(build_a, &rules),
        "<div data-v-#1=\"\" data-v-#2=\"\"><p data-v-#2=\"\">x</p></div>"
    );
}

#[test]
fn which_element_carries_which_scope_is_not_lost() {
    // 印を属性ごと落としていたら、この二つは同じになってしまう。
    let rules = all();
    let same_scope = "<div data-v-aaaaaaaa=\"\"><p data-v-aaaaaaaa=\"\">x</p></div>";
    let other_scope = "<div data-v-aaaaaaaa=\"\"><p data-v-bbbbbbbb=\"\">x</p></div>";
    assert_ne!(dom(same_scope, &rules), dom(other_scope, &rules));
}

#[test]
fn attributes_that_only_look_like_a_scope_id_are_kept() {
    let rules = only(|r| r.scope_ids = true);
    for kept in [
        "data-value",
        "data-v-app",
        "data-v-abc",
        "data-v-a9f3c2d11",
        "data-v-xyz12345",
        "data-v-",
        "data-version",
    ] {
        let html = format!("<div {kept}=\"\">x</div>");
        assert_eq!(dom(&html, &rules), html, "{kept} は落とさない");
    }
}

#[test]
fn without_the_rule_scope_ids_make_builds_differ() {
    // 規則が効いていることの陽性対照。
    let none = NormalizeRules::none();
    assert_ne!(
        dom("<div data-v-58aa11bb=\"\">x</div>", &none),
        dom("<div data-v-a9ff22cc=\"\">x</div>", &none)
    );
}

#[test]
fn css_attribute_selectors_use_the_same_numbers_as_the_dom() {
    let rules = all();
    let mut ids = ScopeIds::new();
    let html = normalize_dom(
        "<div data-v-a9f3c2d1=\"\"><p data-v-0b1c2d3e=\"\">x</p></div>",
        &rules,
        &mut ids,
    )
    .unwrap();
    assert_eq!(html, "<div data-v-#1=\"\"><p data-v-#2=\"\">x</p></div>");
    let text = normalize_cssom(
        ".a[data-v-0b1c2d3e]{color:red}.b[data-v-a9f3c2d1]{color:blue}.c[data-v-12345678]{}",
        &rules,
        &mut ids,
    )
    .unwrap();
    assert_eq!(
        text,
        ".a[data-v-#2]{color:red}.b[data-v-#1]{color:blue}.c[data-v-#3]{}"
    );
}

#[test]
fn css_selectors_that_only_look_like_a_scope_id_are_kept() {
    let rules = only(|r| r.scope_ids = true);
    for kept in [
        "[data-value]",
        "[data-v-app]",
        "[data-v-abc]",
        "[data-v-a9f3c2d1x]",
        "[data-v-a9f3c2d11]",
        "[xdata-v-a9f3c2d1]",
    ] {
        let text = format!("{kept}{{color:red}}");
        assert_eq!(css(&text, &rules), text, "{kept} は落とさない");
    }
}

#[test]
fn a_scope_id_inside_a_css_string_is_not_touched() {
    let rules = only(|r| r.scope_ids = true);
    let text = "a::after{content:\"data-v-a9f3c2d1\"}";
    assert_eq!(css(text, &rules), text);
}

#[test]
fn a_scope_id_appended_to_a_keyframes_name_gets_the_same_number() {
    let rules = all();
    let mut ids = ScopeIds::new();
    let html = normalize_dom(
        "<div data-v-58152a41=\"\" style=\"animation-name: shimmer-58152a41\">x</div>",
        &rules,
        &mut ids,
    )
    .unwrap();
    assert_eq!(
        html,
        "<div data-v-#1=\"\" style=\"animation-name: shimmer-#1\">x</div>"
    );
    let text = normalize_cssom(
        ".a[data-v-58152a41]{animation:shimmer-58152a41 1s}@keyframes shimmer-58152a41{0%{opacity:0}}",
        &rules,
        &mut ids,
    )
    .unwrap();
    assert_eq!(
        text,
        ".a[data-v-#1]{animation:shimmer-#1 1s}@keyframes shimmer-#1{0%{opacity:0}}"
    );
}

#[test]
fn a_keyframes_name_that_comes_before_the_selector_is_still_numbered() {
    // 名前が属性セレクターより前に現れても、同じ番号になる。
    let rules = all();
    assert_eq!(
        css(
            "@keyframes shimmer-58152a41{0%{opacity:0}}.a[data-v-58152a41]{x:y}",
            &rules
        ),
        "@keyframes shimmer-#1{0%{opacity:0}}.a[data-v-#1]{x:y}"
    );
}

#[test]
fn two_builds_with_different_scope_ids_agree_on_keyframes_names_too() {
    let rules = all();
    let build_a = (
        "<div data-v-58152a41=\"\">x</div>",
        ".a[data-v-58152a41]{animation:s-58152a41 1s}@keyframes s-58152a41{0%{opacity:0}}",
    );
    let build_b = (
        "<div data-v-a9b69a0c=\"\">x</div>",
        ".a[data-v-a9b69a0c]{animation:s-a9b69a0c 1s}@keyframes s-a9b69a0c{0%{opacity:0}}",
    );
    assert_eq!(
        hash_dom_and_cssom_with(build_a.0, build_a.1, &rules).unwrap(),
        hash_dom_and_cssom_with(build_b.0, build_b.1, &rules).unwrap()
    );
}

#[test]
fn hex_values_that_are_not_a_known_scope_id_are_kept() {
    let rules = all();
    // 印として現れていない 16 進は、別の物かもしれないので置き換えない。
    for kept in [
        "@keyframes s-deadbeef{0%{opacity:0}}",
        ".a{animation:s-deadbeef1 1s}",
        ".a{content:\"s-58152a41\"}",
        ".a{color:#58152a41}",
    ] {
        assert_eq!(css(kept, &rules), kept, "{kept}");
    }
    // 印として現れていても、名前の一部でなければ置き換えない。
    let text = ".a[data-v-58152a41]{animation:s-58152a41x 1s;color:#58152a41;margin:1 -58152a41}";
    assert_eq!(
        css(text, &rules),
        ".a[data-v-#1]{animation:s-58152a41x 1s;color:#58152a41;margin:1 -58152a41}"
    );
}

// 規則: v-bind() の custom property の名

#[test]
fn bound_css_var_names_become_numbers_in_dom_style_and_in_css() {
    let rules = all();
    let mut ids = ScopeIds::new();
    let html = normalize_dom(
        "<div style=\"--cc7b5cd0: 40px; --v2768dd4d: 120px; --default: 1;\">x</div>",
        &rules,
        &mut ids,
    )
    .unwrap();
    assert_eq!(
        html,
        "<div style=\"--bound-#1: 40px; --bound-#2: 120px; --default: 1;\">x</div>"
    );
    let text = normalize_cssom(
        ".a{height:var(--v2768dd4d);width:var(--cc7b5cd0);margin:var(--0f0f0f0f)}",
        &rules,
        &mut ids,
    )
    .unwrap();
    assert_eq!(
        text,
        ".a{height:var(--bound-#2);width:var(--bound-#1);margin:var(--bound-#3)}"
    );
}

#[test]
fn two_builds_with_different_bound_css_var_names_normalize_to_the_same_text() {
    let rules = all();
    let build_a = (
        "<div style=\"--v5ad87c78: 40px; --cc7b5cd0: 8px\">x</div>",
        ".a{height:var(--v5ad87c78);gap:var(--cc7b5cd0)}",
    );
    let build_b = (
        "<div style=\"--cc7b5cd0: 40px; --v2768dd4d: 8px\">x</div>",
        ".a{height:var(--cc7b5cd0);gap:var(--v2768dd4d)}",
    );
    let a = hash_dom_and_cssom_with(build_a.0, build_a.1, &rules).unwrap();
    let b = hash_dom_and_cssom_with(build_b.0, build_b.1, &rules).unwrap();
    assert_eq!(a, b);
}

#[test]
fn which_property_a_rule_reads_is_not_lost() {
    // 名前を落としていたら、どちらの変数を読むかの違いが消える。
    let rules = all();
    let reads_first = "<i style=\"--aaaaaaaa: 1; --bbbbbbbb: 2\"></i>";
    let css_first = ".a{height:var(--aaaaaaaa)}";
    let css_second = ".a{height:var(--bbbbbbbb)}";
    let a = hash_dom_and_cssom_with(reads_first, css_first, &rules).unwrap();
    let b = hash_dom_and_cssom_with(reads_first, css_second, &rules).unwrap();
    assert_ne!(a.cssom_hash, b.cssom_hash);
}

#[test]
fn custom_properties_that_only_look_like_a_bound_name_are_kept() {
    let rules = only(|r| r.bound_css_vars = true);
    for kept in [
        "--spacing",
        "--default",
        "--color-amber-500",
        "--abcdefg",
        "--abcdefghi",
        "--abcdefgh",
        "--v5ad87c7",
        "--v5ad87c789",
        "--vv5ad87c78",
        "--cc7b5cd0-x",
        "--cc7b5cd0_x",
    ] {
        let text = format!(".a{{height:var({kept})}}");
        assert_eq!(css(&text, &rules), text, "{kept} は落とさない");
        let html = format!("<i style=\"{kept}: 1\"></i>");
        assert_eq!(dom(&html, &rules), html, "{kept} は落とさない");
    }
}

#[test]
fn a_bound_looking_name_in_other_places_is_not_touched() {
    let rules = only(|r| r.bound_css_vars = true);
    // style 以外の属性と、text と、CSS の文字列の中は、字のまま残す。
    let html = "<p title=\"--cc7b5cd0\">--cc7b5cd0</p>";
    assert_eq!(dom(html, &rules), html);
    let text = "a::after{content:\"--cc7b5cd0\"}";
    assert_eq!(css(text, &rules), text);
}

#[test]
fn a_name_inside_a_longer_name_is_not_touched() {
    let rules = only(|r| r.bound_css_vars = true);
    let text = ".a{height:var(--x--cc7b5cd0)}";
    assert_eq!(css(text, &rules), text);
}

#[test]
fn without_the_rule_bound_names_make_builds_differ() {
    let none = NormalizeRules::none();
    assert_ne!(
        css(".a{height:var(--cc7b5cd0)}", &none),
        css(".a{height:var(--v2768dd4d)}", &none)
    );
}

// 規則: 自動で生える id

#[test]
fn generated_ids_and_their_references_get_the_same_numbers() {
    let rules = only(|r| r.generated_ids = true);
    let html = concat!(
        "<label for=\":r1:\">a</label><input id=\":r1:\" aria-describedby=\":r2: :r3:\">",
        "<p id=\":r2:\">x</p><p id=\":r3:\">y</p><a href=\"#:r1:\">z</a>"
    );
    assert_eq!(
        dom(html, &rules),
        concat!(
            "<label for=\"#gen1\">a</label><input id=\"#gen1\" aria-describedby=\"#gen2 #gen3\">",
            "<p id=\"#gen2\">x</p><p id=\"#gen3\">y</p><a href=\"##gen1\">z</a>"
        )
    );
}

#[test]
fn two_renders_with_different_generated_ids_normalize_to_the_same_text() {
    let rules = all();
    let render_a = "<button id=\":r1:\" aria-controls=\":r2:\">a</button><div id=\":r2:\">b</div>";
    let render_b = "<button id=\":r9:\" aria-controls=\":ra:\">a</button><div id=\":ra:\">b</div>";
    assert_eq!(dom(render_a, &rules), dom(render_b, &rules));
}

#[test]
fn every_known_generation_scheme_is_recognized() {
    let rules = only(|r| r.generated_ids = true);
    for (id, expected) in [
        (":r1:", "#gen1"),
        (":r1f:", "#gen1"),
        ("«r2»", "#gen1"),
        ("_r_3_", "#gen1"),
        ("radix-:r4:", "radix-#gen1"),
        ("radix-«r4»-trigger", "radix-#gen1-trigger"),
        ("v-0", "#gen1"),
        ("reka-tabs-trigger-v-12", "reka-tabs-trigger-#gen1"),
        ("reka-tabs-trigger-v-1-2", "reka-tabs-trigger-#gen1"),
    ] {
        let html = format!("<i id=\"{id}\"></i>");
        assert_eq!(
            dom(&html, &rules),
            format!("<i id=\"{expected}\"></i>"),
            "{id}"
        );
    }
}

#[test]
fn ids_that_only_look_generated_are_kept() {
    let rules = only(|r| r.generated_ids = true);
    for kept in [
        "main",
        "r1",
        ":r:",
        ":R1:",
        "section-1",
        "v-",
        "v-x",
        "v-0x",
        "dev-0",
        "my-v-0x",
        "reka-v-",
    ] {
        let html = format!("<i id=\"{kept}\"></i>");
        assert_eq!(dom(&html, &rules), html, "{kept} は落とさない");
    }
}

#[test]
fn pointing_at_a_different_element_is_still_a_difference() {
    // 参照先の取り違えは、本当の変化である。
    // 番号に畳んで同じにしてはならない。
    let rules = all();
    let points_at_first = "<i id=\":r1:\"></i><i id=\":r2:\"></i><b aria-labelledby=\":r1:\"></b>";
    let points_at_second = "<i id=\":r1:\"></i><i id=\":r2:\"></i><b aria-labelledby=\":r2:\"></b>";
    assert_ne!(dom(points_at_first, &rules), dom(points_at_second, &rules));
}

#[test]
fn hand_written_ids_that_the_css_selects_are_kept() {
    // 名前が CSS に現れる id は、名前が絵に効く。
    // 参照で結ばれていても、番号にして同じにしてはならない。
    let rules = all();
    let a = hash_dom_and_cssom_with(
        "<i id=\"a\"></i><b aria-labelledby=\"a\"></b>",
        "#a{color:red}",
        &rules,
    )
    .unwrap();
    let b = hash_dom_and_cssom_with(
        "<i id=\"b\"></i><b aria-labelledby=\"b\"></b>",
        "#b{color:red}",
        &rules,
    )
    .unwrap();
    assert_ne!(a.dom_hash, b.dom_hash);
}

// 規則: 参照で結ばれた id

fn dom_css(html: &str, css_text: &str, rules: &NormalizeRules) -> String {
    normalize_dom_with_css(html, css_text, rules, &mut ScopeIds::new()).expect("正規化できる入力")
}

fn only_referenced() -> NormalizeRules {
    only(|r| r.referenced_ids = true)
}

#[test]
fn referenced_ids_and_their_references_get_the_same_numbers() {
    let rules = only_referenced();
    let html = concat!(
        "<label for=\"email\">a</label><input id=\"email\" aria-describedby=\"hint\">",
        "<p id=\"hint\">h</p><div aria-controls=\"panel\" aria-activedescendant=\"opt\"></div>",
        "<ul id=\"panel\"><li id=\"opt\"></li></ul><a href=\"#hint\">j</a>"
    );
    assert_eq!(
        dom_css(html, "", &rules),
        concat!(
            "<label for=\"#ref1\">a</label><input id=\"#ref1\" aria-describedby=\"#ref2\">",
            "<p id=\"#ref2\">h</p><div aria-controls=\"#ref3\" aria-activedescendant=\"#ref4\"></div>",
            "<ul id=\"#ref3\"><li id=\"#ref4\"></li></ul><a href=\"##ref2\">j</a>"
        )
    );
}

#[test]
fn every_reference_attribute_is_followed() {
    let rules = only_referenced();
    for attr in [
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
    ] {
        let html = format!("<i id=\"x1\"></i><b {attr}=\"x1\"></b>");
        assert_eq!(
            dom_css(&html, "", &rules),
            format!("<i id=\"#ref1\"></i><b {attr}=\"#ref1\"></b>"),
            "{attr}"
        );
    }
}

#[test]
fn two_renders_with_different_random_ids_normalize_to_the_same_text() {
    // 部品の名前に頼らない。
    // 参照で結ばれていれば、どの形の id でも畳める。
    let rules = all();
    let render_a = concat!(
        "<div aria-activedescendant=\"cm-ac-tqao-0\" aria-controls=\"cm-ac-tqao\"></div>",
        "<ul id=\"cm-ac-tqao\"><li id=\"cm-ac-tqao-0\">x</li></ul>"
    );
    let render_b = concat!(
        "<div aria-activedescendant=\"cm-ac-11z4e-0\" aria-controls=\"cm-ac-11z4e\"></div>",
        "<ul id=\"cm-ac-11z4e\"><li id=\"cm-ac-11z4e-0\">x</li></ul>"
    );
    assert_eq!(dom_css(render_a, "", &rules), dom_css(render_b, "", &rules));
    // 参照されていない id は、この規則では畳まない。
    assert_ne!(
        dom_css("<ul id=\"cm-ac-tqao\"></ul>", "", &rules),
        dom_css("<ul id=\"cm-ac-11z4e\"></ul>", "", &rules)
    );
}

#[test]
fn ids_that_the_css_text_names_are_kept() {
    let rules = only_referenced();
    let html = "<i id=\"main\"></i><b aria-labelledby=\"main\"></b>";
    for css_text in [
        "#main{color:red}",
        "[id=\"main\"]{color:red}",
        "[id='main']{color:red}",
        "a #main > b{color:red}",
        "/* 説明 */ #main{}",
    ] {
        assert_eq!(dom_css(html, css_text, &rules), html, "{css_text}");
    }
    // escape を含む id も、CSS の書き方に合わせて守る。
    let colon = "<i id=\"a:b\"></i><b aria-labelledby=\"a:b\"></b>";
    assert_eq!(dom_css(colon, "#a\\:b{color:red}", &rules), colon);
    // CSS に現れない id は番号になる。
    assert_eq!(
        dom_css(html, ".main-x{color:red}#other{}", &rules),
        "<i id=\"#ref1\"></i><b aria-labelledby=\"#ref1\"></b>"
    );
}

#[test]
fn an_id_that_only_shares_a_prefix_with_a_css_name_is_still_numbered() {
    let rules = only_referenced();
    assert_eq!(
        dom_css(
            "<i id=\"main\"></i><b aria-labelledby=\"main\"></b>",
            "#main2{color:red}.xmain{}",
            &rules
        ),
        "<i id=\"#ref1\"></i><b aria-labelledby=\"#ref1\"></b>"
    );
}

#[test]
fn ids_that_nothing_refers_to_are_kept() {
    // 参照されていない id は、名前が何に使われるか分からないので、残す。
    // 参照で結ばれた id が文書に在っても、結ばれていない id は番号にしない。
    let rules = only_referenced();
    let html = concat!(
        "<i id=\"lonely\"></i><b id=\"other\" aria-hidden=\"true\"></b>",
        "<i id=\"x1\"></i><label for=\"x1\"></label>"
    );
    assert_eq!(
        dom_css(html, "", &rules),
        concat!(
            "<i id=\"lonely\"></i><b id=\"other\" aria-hidden=\"true\"></b>",
            "<i id=\"#ref1\"></i><label for=\"#ref1\"></label>"
        )
    );
}

#[test]
fn an_id_that_only_a_fragment_link_points_at_is_numbered() {
    let rules = only_referenced();
    assert_eq!(
        dom_css(
            "<h2 id=\"sec-9f\"></h2><a href=\"#sec-9f\">x</a>",
            "",
            &rules
        ),
        "<h2 id=\"#ref1\"></h2><a href=\"##ref1\">x</a>"
    );
    assert_eq!(
        dom_css(
            "<h2 id=\"sec-9f\"></h2><svg><use xlink:href=\"#sec-9f\"></use></svg>",
            "",
            &rules
        ),
        "<h2 id=\"#ref1\"></h2><svg><use xlink:href=\"##ref1\"></use></svg>"
    );
}

#[test]
fn a_reference_without_a_target_is_numbered_too() {
    // 参照先が無くても、名前が絵に効く道は無い。
    let rules = only_referenced();
    assert_eq!(
        dom_css("<b aria-controls=\"gone\"></b>", "", &rules),
        "<b aria-controls=\"#ref1\"></b>"
    );
}

#[test]
fn an_attribute_with_several_ids_numbers_each_and_keeps_the_order() {
    let rules = only_referenced();
    let html = concat!(
        "<i id=\"a1\"></i><i id=\"b1\"></i><i id=\"c1\"></i>",
        "<p aria-describedby=\"c1   a1 b1\"></p>"
    );
    assert_eq!(
        dom_css(html, "", &rules),
        concat!(
            "<i id=\"#ref1\"></i><i id=\"#ref2\"></i><i id=\"#ref3\"></i>",
            "<p aria-describedby=\"#ref3 #ref1 #ref2\"></p>"
        )
    );
    // 一部の id だけを CSS が選んでいるときは、その id だけが残る。
    assert_eq!(
        dom_css(html, "#b1{color:red}", &rules),
        concat!(
            "<i id=\"#ref1\"></i><i id=\"b1\"></i><i id=\"#ref2\"></i>",
            "<p aria-describedby=\"#ref2 #ref1 b1\"></p>"
        )
    );
}

#[test]
fn ids_that_appear_twice_share_one_number() {
    let rules = only_referenced();
    assert_eq!(
        dom_css(
            "<i id=\"d\"></i><i id=\"d\"></i><b for=\"d\"></b><i id=\"e\"></i><b for=\"e\"></b>",
            "",
            &rules
        ),
        "<i id=\"#ref1\"></i><i id=\"#ref1\"></i><b for=\"#ref1\"></b><i id=\"#ref2\"></i><b for=\"#ref2\"></b>"
    );
}

#[test]
fn pointing_at_another_referenced_id_is_still_a_difference_among_numbered_ids() {
    // 番号にしても、どの要素がどの要素を指すかは残る。
    let rules = all();
    let first = "<i id=\"p\"></i><i id=\"q\"></i><b aria-labelledby=\"p\"></b><u aria-labelledby=\"q\"></u>";
    let swapped = "<i id=\"p\"></i><i id=\"q\"></i><b aria-labelledby=\"q\"></b><u aria-labelledby=\"p\"></u>";
    assert_ne!(dom_css(first, "", &rules), dom_css(swapped, "", &rules));
}

#[test]
fn an_empty_or_missing_fragment_is_not_a_reference() {
    let rules = only_referenced();
    let html = "<a href=\"#\">a</a><a href=\"/x#y\">b</a><a href=\"\">c</a>";
    assert_eq!(dom_css(html, "", &rules), html);
}

#[test]
fn a_reference_through_an_unrelated_attribute_is_not_followed() {
    // 属性の値がたまたま id と同じでも、参照の属性でなければ字のまま残す。
    let rules = only_referenced();
    let html = "<i id=\"email\"></i><input name=\"email\" autocomplete=\"email\" data-testid=\"email\" for-x=\"email\">";
    assert_eq!(dom_css(html, "", &rules), html);
}

#[test]
fn without_the_rule_random_ids_make_builds_differ() {
    let none = NormalizeRules::none();
    assert_ne!(
        dom_css("<i id=\"r9\"></i><b for=\"r9\"></b>", "", &none),
        dom_css("<i id=\"r7\"></i><b for=\"r7\"></b>", "", &none)
    );
}

#[test]
fn the_referenced_rule_comes_before_the_shape_rule_and_both_respect_the_css() {
    let rules = all();
    // 参照で結ばれた id は、形が生成物らしくても、参照の規則の番号になる。
    assert_eq!(
        dom_css("<i id=\":r1:\"></i><b for=\":r1:\"></b>", "", &rules),
        "<i id=\"#ref1\"></i><b for=\"#ref1\"></b>"
    );
    // 参照されていない生成物らしい id は、形の規則の番号になる。
    assert_eq!(
        dom_css("<i id=\":r2:\"></i>", "", &rules),
        "<i id=\"#gen1\"></i>"
    );
    // CSS が名前を持つ id は、形が生成物らしくても、どちらの規則でも残す。
    assert_eq!(
        dom_css("<i id=\"v-0\"></i><b for=\"v-0\"></b>", "#v-0{x:y}", &rules),
        "<i id=\"v-0\"></i><b for=\"v-0\"></b>"
    );
    assert_eq!(
        dom_css("<i id=\"v-1\"></i>", ".a{content:\"v-1\"}", &rules),
        "<i id=\"v-1\"></i>"
    );
}

#[test]
fn css_names_collects_identifiers_strings_and_escapes() {
    let names = css::referenced_names("#a-b .c_d[x=\"e f\"]{g:'h-i'} #j\\:k{} /* l */");
    for expected in ["a-b", "c_d", "x", "e f", "e", "f", "g", "h-i", "j:k", "l"] {
        assert!(names.contains(expected), "{expected} を集める: {names:?}");
    }
    assert!(!names.contains("a"));
}

// 規則: 毎回変わる origin

#[test]
fn loopback_origins_and_blob_ids_are_masked() {
    let rules = only(|r| r.ephemeral_origins = true);
    assert_eq!(
        dom(
            concat!(
                "<img src=\"http://127.0.0.1:4101/a.png\">",
                "<a href=\"https://localhost:9/x\">l</a>",
                "<img src=\"blob:http://127.0.0.1:4101/6f1c-22\">",
                "<div style=\"background:url(http://127.0.0.1:4101/b.png)\"></div>"
            ),
            &rules
        ),
        concat!(
            "<img src=\"<origin>/a.png\">",
            "<a href=\"<origin>/x\">l</a>",
            "<img src=\"blob:<id>\">",
            "<div style=\"background:url(<origin>/b.png)\"></div>"
        )
    );
}

#[test]
fn two_builds_served_on_different_ports_normalize_to_the_same_text() {
    let rules = all();
    assert_eq!(
        dom("<img src=\"http://127.0.0.1:4101/a.png\">", &rules),
        dom("<img src=\"http://127.0.0.1:50999/a.png\">", &rules)
    );
}

#[test]
fn other_hosts_and_other_places_are_not_masked() {
    let rules = only(|r| r.ephemeral_origins = true);
    for kept in [
        "<img src=\"https://cdn.example.com/a.png\">",
        "<img src=\"http://localhost.example.com:8080/a.png\">",
        "<img src=\"http://127.0.0.10/a.png\">",
        "<img src=\"/assets/a-3f9c.png\">",
        // URL を取らない属性と text は、字のまま残す。
        "<p title=\"http://127.0.0.1:4101/\">x</p>",
        "<p>http://127.0.0.1:4101/</p>",
    ] {
        assert_eq!(dom(kept, &rules), kept, "{kept}");
    }
}

#[test]
fn a_different_path_on_the_same_loopback_origin_is_still_a_difference() {
    let rules = all();
    assert_ne!(
        dom("<img src=\"http://127.0.0.1:4101/a.png\">", &rules),
        dom("<img src=\"http://127.0.0.1:4101/b.png\">", &rules)
    );
}

#[test]
fn origins_inside_css_urls_and_strings_are_masked() {
    let rules = only(|r| r.ephemeral_origins = true);
    assert_eq!(
        css(
            "@font-face{src:url(http://127.0.0.1:4101/f.woff2)}a{b:url('http://127.0.0.1:4101/i.png')}",
            &rules
        ),
        "@font-face{src:url(<origin>/f.woff2)}a{b:url('<origin>/i.png')}"
    );
}

#[test]
fn css_text_outside_urls_and_strings_is_not_masked() {
    let rules = only(|r| r.ephemeral_origins = true);
    let text = "a{content:attr(data-x);margin:http}";
    assert_eq!(css(text, &rules), text);
}

// 規則: 属性と class の並び

#[test]
fn attribute_and_class_order_do_not_matter() {
    let rules = all();
    assert_eq!(
        dom("<p id=\"x\" class=\"b a  c\" title=\"t\">x</p>", &rules),
        dom("<p title=\"t\" class=\"c b a\" id=\"x\">x</p>", &rules)
    );
}

#[test]
fn different_attribute_values_and_different_class_sets_still_differ() {
    let rules = all();
    assert_ne!(
        dom("<p class=\"a b\">x</p>", &rules),
        dom("<p class=\"a c\">x</p>", &rules)
    );
    assert_ne!(
        dom("<p title=\"a\">x</p>", &rules),
        dom("<p title=\"b\">x</p>", &rules)
    );
    assert_ne!(
        dom("<p class=\"a\">x</p>", &rules),
        dom("<p class=\"a a\">x</p>", &rules)
    );
}

#[test]
fn element_order_is_a_difference() {
    let rules = all();
    assert_ne!(
        dom("<i>a</i><b>b</b>", &rules),
        dom("<b>b</b><i>a</i>", &rules)
    );
}

// 規則: CSS の空白

#[test]
fn css_whitespace_runs_become_one_space() {
    let rules = only(|r| r.collapse_css_whitespace = true);
    assert_eq!(
        css("  a  {\n  color :\t red ;\n}  ", &rules),
        "a { color : red ; }"
    );
}

#[test]
fn whitespace_inside_css_strings_and_urls_is_kept() {
    let rules = all();
    assert_eq!(
        css("a::after{content:\"a   b\"}b{c:url(\"x  y.png\")}", &rules),
        "a::after{content:\"a   b\"}b{c:url(\"x  y.png\")}"
    );
    assert_ne!(
        css("a::after{content:\"a b\"}", &rules),
        css("a::after{content:\"a  b\"}", &rules)
    );
}

#[test]
fn unreadable_css_is_refused_instead_of_guessed() {
    let rules = all();
    for bad in [
        "a{/* never closed",
        "a{content:\"never closed}",
        "a{b:url(x",
    ] {
        let result = normalize_cssom(bad, &rules, &mut ScopeIds::new());
        assert!(
            matches!(result, Err(NormalizeError::Css { .. })),
            "{bad:?} は断る: {result:?}"
        );
    }
}

#[test]
fn css_escapes_and_multibyte_text_do_not_panic() {
    let rules = all();
    let out = css("a\\:b{content:\"日本語\\\"\"}\\", &rules);
    assert!(out.contains("日本語"));
}

// 落とさぬと決めた物

#[test]
fn times_and_random_looking_text_are_not_dropped() {
    // 消せば、本当の text の変化を同じ hash に畳み込む。
    // story 側で固定するのが本筋である。
    let a = hashes("<time>12:34:56</time>", "");
    let b = hashes("<time>12:34:57</time>", "");
    assert_ne!(a.dom_hash, b.dom_hash);
    let a = hashes("<p>id: 3f9c2a</p>", "");
    let b = hashes("<p>id: 8d7e11</p>", "");
    assert_ne!(a.dom_hash, b.dom_hash);
}

#[test]
fn random_looking_attribute_values_are_not_dropped() {
    let a = hashes("<p data-testid=\"x7f3\" title=\"3f9c\">a</p>", "");
    let b = hashes("<p data-testid=\"y2b9\" title=\"3f9c\">a</p>", "");
    assert_ne!(a.dom_hash, b.dom_hash);
}

#[test]
fn input_values_written_as_attributes_are_kept() {
    let a = hashes("<input value=\"a\">", "");
    let b = hashes("<input value=\"b\">", "");
    assert_ne!(a.dom_hash, b.dom_hash);
}

// hash

#[test]
fn a_hash_has_the_scheme_and_a_full_digest() {
    let h = hashes("<p>a</p>", "a{color:red}");
    for hash in [&h.dom_hash, &h.cssom_hash] {
        let digest = hash.strip_prefix("sha256:").expect("方式を頭に持つ");
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    assert_eq!(h.norm_version, NORM_VERSION);
}

#[test]
fn the_same_input_gives_the_same_hash() {
    assert_eq!(
        hashes("<p>a</p>", "a{color:red}"),
        hashes("<p>a</p>", "a{color:red}")
    );
}

#[test]
fn dom_and_cssom_hashes_are_kept_apart() {
    // 同じ字でも、DOM と CSSOM で同じ hash にならない。
    let h = hashes("a", "a");
    assert_ne!(h.dom_hash, h.cssom_hash);
}

#[test]
fn a_dom_change_moves_only_the_dom_hash_and_a_css_change_only_the_cssom_hash() {
    let base = hashes("<p>a</p>", "a{color:red}");
    let dom_changed = hashes("<p>b</p>", "a{color:red}");
    let css_changed = hashes("<p>a</p>", "a{color:blue}");
    assert_ne!(base.dom_hash, dom_changed.dom_hash);
    assert_eq!(base.cssom_hash, dom_changed.cssom_hash);
    assert_eq!(base.dom_hash, css_changed.dom_hash);
    assert_ne!(base.cssom_hash, css_changed.cssom_hash);
}

#[test]
fn real_changes_in_css_are_not_hidden_by_normalization() {
    let rules = all();
    let base = css(".a{color:red;margin:0}", &rules);
    for changed in [
        ".a{color:blue;margin:0}",
        ".a{color:red;margin:1px}",
        ".b{color:red;margin:0}",
        ".a{margin:0;color:red}",
        ".a{color:red;margin:0}.c{}",
        "@media (min-width:1px){.a{color:red;margin:0}}",
    ] {
        assert_ne!(base, css(changed, &rules), "{changed}");
    }
}

#[test]
fn an_oversized_input_is_refused_without_being_read() {
    let big = "a".repeat(MAX_INPUT_BYTES + 1);
    assert!(matches!(
        hash_dom_and_cssom(&big, ""),
        Err(NormalizeError::TooLarge { .. })
    ));
    assert!(matches!(
        hash_dom_and_cssom("", &big),
        Err(NormalizeError::TooLarge { .. })
    ));
}

// 通し

/// 同じ source の二度の build を模した組。
/// 毎回変わる物だけが違う。
fn two_builds() -> ((String, String), (String, String)) {
    let dom_of = |scope1: &str, scope2: &str, id: &str, port: u16, chunk: &str| {
        format!(
            concat!(
                "<!DOCTYPE html><html lang=\"ja\" class=\"sb-show-main\"><head>",
                "<script type=\"module\" src=\"/assets/iframe-{chunk}.js\"></script>",
                "<link rel=\"stylesheet\" href=\"/assets/index-{chunk}.css\"></head>",
                "<body><div id=\"storybook-root\"><!--v-if-->",
                "<div data-v-{scope1}=\"\" class=\"card b a\">",
                "<button data-v-{scope2}=\"\" id=\"{id}\" aria-describedby=\"{id}-d\">go</button>",
                "<img src=\"http://127.0.0.1:{port}/img/logo-7c1.png\"></div></div></body></html>"
            ),
            scope1 = scope1,
            scope2 = scope2,
            id = id,
            port = port,
            chunk = chunk
        )
    };
    let css_of = |scope1: &str, scope2: &str, port: u16| {
        format!(
            ".card[data-v-{scope1}]{{color:red}}\nbutton[data-v-{scope2}]{{margin:0}}\n@font-face{{src:url(http://127.0.0.1:{port}/f-1a.woff2)}}"
        )
    };
    (
        (
            dom_of("58aa11bb", "0c0c0c0c", ":r1:", 4101, "a1b2c3"),
            css_of("58aa11bb", "0c0c0c0c", 4101),
        ),
        (
            dom_of("a9ff22cc", "ffeeddcc", ":r7:", 50999, "d4e5f6"),
            css_of("a9ff22cc", "ffeeddcc", 50999),
        ),
    )
}

#[test]
fn two_builds_of_the_same_source_give_the_same_hashes() {
    let ((dom_a, css_a), (dom_b, css_b)) = two_builds();
    assert_eq!(hashes(&dom_a, &css_a), hashes(&dom_b, &css_b));
}

#[test]
fn without_normalization_the_same_two_builds_differ() {
    // 規則が実際に働いていることの陽性対照。
    let ((dom_a, css_a), (dom_b, css_b)) = two_builds();
    let none = NormalizeRules::none();
    let a = hash_dom_and_cssom_with(&dom_a, &css_a, &none).unwrap();
    let b = hash_dom_and_cssom_with(&dom_b, &css_b, &none).unwrap();
    assert_ne!(a.dom_hash, b.dom_hash);
    assert_ne!(a.cssom_hash, b.cssom_hash);
}

#[test]
fn a_real_change_survives_all_the_rules() {
    let ((dom_a, css_a), (dom_b, css_b)) = two_builds();
    let base = hashes(&dom_a, &css_a);
    // 文言が変わった、色が変わった、構造が変わった。
    let text = hashes(&dom_b.replace(">go<", ">stop<"), &css_b);
    let color = hashes(&dom_b, &css_b.replace("red", "green"));
    let structure = hashes(&dom_b.replace("<button", "<a"), &css_b);
    assert_ne!(base.dom_hash, text.dom_hash);
    assert_ne!(base.cssom_hash, color.cssom_hash);
    assert_ne!(base.dom_hash, structure.dom_hash);
}
