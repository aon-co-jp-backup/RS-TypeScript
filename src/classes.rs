//! クラス構文(可視性修飾子・コンストラクタのパラメータプロパティ・
//! クラスフィールドの型注釈)を扱うパス。
//!
//! 対応する内容:
//! - `public`/`private`/`protected`/`readonly`/`abstract`/`override`
//!   修飾子の除去(クラスメンバー宣言・コンストラクタパラメータの
//!   いずれの位置でも)。`static`はJSの正式な構文なので対象外
//!   (除去しない)。
//! - クラスフィールド宣言の型注釈・オプショナル`?`・確定代入表明`!`
//!   の除去(`private x: number;` → `x;`、`y?: string;` → `y;`、
//!   `z!: number;` → `z;`)。
//! - **コンストラクタのパラメータプロパティの展開**(最も間違えやすい
//!   ポイント——TypeScriptの`constructor(public y: string)`は単なる
//!   型注釈の除去では済まず、`this.y = y;`という**実行時に意味のある
//!   代入文**を生成しなければ正しいJSにならない)。
//!   `constructor(public y: string, private z: number = 0)`は
//!   `constructor(y, z = 0) { this.y = y; this.z = z; ...元の本体... }`
//!   のように、モディファイア付きパラメータの名前を記録しておき、
//!   コンストラクタ本体の開き波括弧の直後に代入文を注入する。
//!   (モディファイアなしの通常パラメータは対象外——プロパティ化
//!   されない、これは`tsc`の実際の仕様通り)。
//!
//! 型注釈自体の除去(`(a: number)`のような通常のパラメータ・戻り値型)
//! は、モディファイアを除去した後の後続パス(`transpile::
//! strip_type_annotations`)に委ねる(モディファイアさえ除去して
//! しまえば、パラメータの直前トークンが`(`/`,`になるため、既存の
//! ヒューリスティックがそのまま働く)。

use crate::token::Token;
use crate::transpile::{find_type_end, is_modifier_keyword, is_significant, preserve_trailing_whitespace};

struct ParenCtx {
    is_constructor: bool,
    props: Vec<String>,
}

fn last_significant(out: &[Token]) -> Option<&Token> {
    out.iter().rev().find(|t| is_significant(t))
}

fn is_stop_before_marker(t: &Token) -> bool {
    matches!(t, Token::Punctuator(p) if matches!(p.as_str(), ":" | ";" | "=" | "}"))
}

pub(crate) fn strip_class_syntax(tokens: &[Token]) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut paren_ctx_stack: Vec<ParenCtx> = Vec::new();
    let mut class_member_depths: Vec<i32> = Vec::new();
    let mut brace_depth: i32 = 0;
    let mut awaiting_class_brace = false;
    let mut pending_ctor_injection: Option<Vec<String>> = None;
    let mut pending_property_marker = false;
    // `extends` を持つクラスか(コンストラクタの代入は `super(...)` の後に置く必要がある)
    let mut class_extends_stack: Vec<bool> = Vec::new();
    let mut pending_class_extends = false;
    // `extends` クラスのコンストラクタ: (代入する名前, 本体の波括弧の深さ)
    let mut super_wait: Option<(Vec<String>, i32)> = None;
    // `super(` の括弧を閉じる直前のスタック長 / 閉じた後に `;` を待つ代入
    let mut super_call_len: Option<usize> = None;
    let mut inject_after_semicolon: Option<Vec<String>> = None;
    let mut i = 0usize;

    while i < tokens.len() {
        let tok = &tokens[i];
        if matches!(tok, Token::Eof) {
            out.push(tok.clone());
            break;
        }
        if !is_significant(tok) {
            out.push(tok.clone());
            i += 1;
            continue;
        }

        let is_member_level = class_member_depths.last() == Some(&brace_depth) && paren_ctx_stack.is_empty();

        if let Token::Identifier(name) = tok {
            if name == "class" {
                awaiting_class_brace = true;
                pending_class_extends = false;
                out.push(tok.clone());
                i += 1;
                continue;
            }

            if awaiting_class_brace && name == "extends" {
                pending_class_extends = true;
            }

            // `implements A, B<C>` は型だけの節なので、`{` の直前まで丸ごと除く
            if awaiting_class_brace && name == "implements" {
                if let Some(brace) = (i..tokens.len()).find(|&k| matches!(&tokens[k], Token::Punctuator(p) if p == "{")) {
                    while matches!(out.last(), Some(t) if !is_significant(t)) {
                        out.pop();
                    }
                    let mut m = brace;
                    while m > i && !is_significant(&tokens[m - 1]) {
                        m -= 1;
                    }
                    i = m;
                    continue;
                }
            }

            // `abstract` メンバー・`declare` フィールドは実行時に存在しないので、`;` まで(または次の行/`}`の手前まで)丸ごと除く
            if is_member_level && (name == "abstract" || name == "declare") {
                let prev_sig = last_significant(&out);
                let at_member_start = prev_sig.is_none()
                    || matches!(prev_sig, Some(Token::Punctuator(p)) if matches!(p.as_str(), ";" | "{" | "}"));
                let next_sig = tokens[i + 1..].iter().find(|t| is_significant(t));
                let is_modifier_use = !matches!(next_sig, Some(Token::Punctuator(p)) if matches!(p.as_str(), "(" | "=" | ";" | ":" | "?" | "!"));
                if at_member_start && is_modifier_use {
                    let mut depth = 0i32;
                    let mut j = i;
                    let mut seen_sig = false;
                    loop {
                        match tokens.get(j) {
                            None | Some(Token::Eof) => break,
                            Some(Token::Punctuator(p)) => {
                                seen_sig = true;
                                match p.as_str() {
                                    "(" | "[" | "{" => depth += 1,
                                    ")" | "]" => depth -= 1,
                                    "}" => {
                                        if depth == 0 {
                                            break; // クラス本体の終わり(消費しない)
                                        }
                                        depth -= 1;
                                    }
                                    ";" if depth == 0 => {
                                        j += 1;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            Some(Token::Whitespace(w)) if w.contains('\n') && depth == 0 && seen_sig => break,
                            Some(t) if is_significant(t) => seen_sig = true,
                            _ => {}
                        }
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }

            if is_modifier_keyword(name) {
                let prev_sig = last_significant(&out);
                let in_param_start = !paren_ctx_stack.is_empty()
                    && matches!(prev_sig, Some(Token::Punctuator(p)) if p == "(" || p == ",");
                let in_member_start = is_member_level
                    && (prev_sig.is_none()
                        || matches!(prev_sig, Some(Token::Punctuator(p)) if matches!(p.as_str(), ";" | "{" | "}")));

                // `abstract() {}` / `readonly = 1` のように、修飾子と同じ綴りのメンバー名は残す
                let next_sig_after = tokens[i + 1..].iter().find(|t| is_significant(t));
                let used_as_name = matches!(next_sig_after, Some(Token::Punctuator(p)) if matches!(p.as_str(), "(" | "=" | ";" | ":" | "?" | "!" | "," | ")"));
                if used_as_name {
                    out.push(tok.clone());
                    i += 1;
                    continue;
                }
                if in_param_start {
                    if paren_ctx_stack.last().map(|c| c.is_constructor).unwrap_or(false) {
                        pending_property_marker = true;
                    }
                    // モディファイア直後の空白1個も合わせて読み飛ばす
                    // (`public x`の`public`だけ除いて空白を残すと、
                    // 直前のカンマ後の空白と合わさって`,  y`のように
                    // 空白が二重になってしまうため)。
                    i += 1;
                    if matches!(tokens.get(i), Some(Token::Whitespace(_))) {
                        i += 1;
                    }
                    continue;
                } else if in_member_start {
                    i += 1;
                    if matches!(tokens.get(i), Some(Token::Whitespace(_))) {
                        i += 1;
                    }
                    continue;
                }
            }

            if name == "super" && matches!(&super_wait, Some((_, d)) if *d == brace_depth) {
                let next_sig = tokens[i + 1..].iter().find(|t| is_significant(t));
                if matches!(next_sig, Some(Token::Punctuator(p)) if p == "(") {
                    // この `(` が push されたあとのスタック長 = 現在長 + 1
                    super_call_len = Some(paren_ctx_stack.len() + 1);
                }
            }

            if pending_property_marker {
                // モディファイア(1つ以上)の直後に来た識別子 = パラメータ
                // プロパティのパラメータ名。
                if let Some(ctx) = paren_ctx_stack.last_mut() {
                    ctx.props.push(name.clone());
                }
                pending_property_marker = false;
            }
        }

        if let Token::Punctuator(p) = tok {
            match p.as_str() {
                "(" => {
                    let prev_sig = last_significant(&out);
                    let is_ctor = matches!(prev_sig, Some(Token::Identifier(n)) if n == "constructor");
                    paren_ctx_stack.push(ParenCtx { is_constructor: is_ctor, props: Vec::new() });
                }
                ")" => {
                    let popped_len_before = paren_ctx_stack.len();
                    if super_call_len == Some(popped_len_before) {
                        super_call_len = None;
                        if let Some((props, _)) = super_wait.take() {
                            inject_after_semicolon = Some(props);
                        }
                    }
                    if let Some(ctx) = paren_ctx_stack.pop() {
                        if ctx.is_constructor && !ctx.props.is_empty() {
                            pending_ctor_injection = Some(ctx.props);
                        }
                    }
                }
                "{" => {
                    let new_depth = brace_depth + 1;
                    if awaiting_class_brace {
                        class_member_depths.push(new_depth);
                        class_extends_stack.push(pending_class_extends);
                        pending_class_extends = false;
                        awaiting_class_brace = false;
                    }
                    brace_depth = new_depth;
                    out.push(tok.clone());
                    if let Some(props) = pending_ctor_injection.take() {
                        if class_extends_stack.last() == Some(&true) {
                            // 派生クラスでは `this` は `super()` の後でしか使えない
                            super_wait = Some((props, new_depth));
                        } else {
                            let injected: String = props.iter().map(|p| format!(" this.{p} = {p};")).collect();
                            out.push(Token::Raw(injected));
                        }
                    }
                    i += 1;
                    continue;
                }
                "}" => {
                    if class_member_depths.last() == Some(&brace_depth) {
                        class_member_depths.pop();
                        class_extends_stack.pop();
                    }
                    if matches!(&super_wait, Some((_, d)) if *d == brace_depth) {
                        super_wait = None; // `super()` が見つからないまま本体が終わった
                    }
                    brace_depth -= 1;
                }
                ";" if inject_after_semicolon.is_some() => {
                    out.push(tok.clone());
                    if let Some(props) = inject_after_semicolon.take() {
                        let injected: String = props.iter().map(|p| format!(" this.{p} = {p};")).collect();
                        out.push(Token::Raw(injected));
                    }
                    i += 1;
                    continue;
                }
                "?" | "!" if is_member_level && matches!(last_significant(&out), Some(Token::Identifier(_))) => {
                    let next_sig = tokens[i + 1..].iter().find(|t| is_significant(t));
                    if matches!(next_sig, Some(t) if is_stop_before_marker(t)) {
                        i += 1;
                        continue;
                    }
                }
                ":" if is_member_level && matches!(last_significant(&out), Some(Token::Identifier(_))) => {
                    let end = find_type_end(tokens, i + 1, &["=", ";", "}"]);
                    i = preserve_trailing_whitespace(tokens, i, end);
                    continue;
                }
                _ => {}
            }
        }

        out.push(tok.clone());
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use crate::transpile::transpile;

    #[test]
    fn strips_visibility_modifiers_from_a_constructor_parameter_without_property_shorthand() {
        // モディファイアなしの通常パラメータ(型注釈のみ)。
        assert_eq!(
            transpile("class Point {\n  constructor(x: number, y: number) {\n    this.sum = x + y;\n  }\n}"),
            "class Point {\n  constructor(x, y) {\n    this.sum = x + y;\n  }\n}"
        );
    }

    #[test]
    fn expands_constructor_parameter_properties_into_this_assignments() {
        // これがこのクレートで最も間違えやすいケース: `public`/`private`は
        // 単なる型レベルの飾りではなく、TypeScriptコンパイラが実際に
        // `this.y = y;`という代入文を生成する実行時挙動を持つ。
        let ts = "class Point {\n  constructor(public x: number, private y: string) {\n    console.log(x);\n  }\n}";
        let js = transpile(ts);
        assert_eq!(
            js,
            "class Point {\n  constructor(x, y) { this.x = x; this.y = y;\n    console.log(x);\n  }\n}"
        );
    }

    #[test]
    fn expands_a_readonly_only_parameter_property() {
        assert_eq!(
            transpile("class C {\n  constructor(readonly id: string) {}\n}"),
            "class C {\n  constructor(id) { this.id = id;}\n}"
        );
    }

    #[test]
    fn mixes_parameter_properties_with_plain_parameters() {
        let ts = "class C {\n  constructor(public a: number, b: number) {\n    this.b = b;\n  }\n}";
        assert_eq!(
            transpile(ts),
            "class C {\n  constructor(a, b) { this.a = a;\n    this.b = b;\n  }\n}"
        );
    }

    #[test]
    fn strips_class_field_modifiers_and_type_annotations() {
        let ts = "class Foo {\n  private x: number;\n  public y: string = \"a\";\n  readonly z: boolean = true;\n}";
        assert_eq!(transpile(ts), "class Foo {\n  x;\n  y = \"a\";\n  z = true;\n}");
    }

    #[test]
    fn strips_optional_and_definite_assignment_markers_on_class_fields() {
        assert_eq!(
            transpile("class Foo {\n  x?: number;\n  y!: string;\n}"),
            "class Foo {\n  x;\n  y;\n}"
        );
    }

    #[test]
    fn removes_implements_clause() {
        assert_eq!(transpile("class A implements I, J<K> { x = 1; }"), "class A { x = 1; }");
        assert_eq!(transpile("class A extends B<T> implements I { }"), "class A extends B { }");
    }

    #[test]
    fn drops_abstract_members_and_declare_fields() {
        let ts = "abstract class Q {\n  abstract m(): void;\n  protected abstract get v(): number;\n  real() { return 1; }\n  declare x: number;\n}";
        let js = transpile(ts);
        assert!(!js.contains("abstract") && !js.contains("declare"), "{js}");
        assert!(js.contains("real() { return 1; }"), "{js}");
        assert!(!js.contains("m()") && !js.contains("get v"), "{js}");
    }

    #[test]
    fn a_method_named_abstract_or_declare_is_kept() {
        let ts = "class A { abstract() { return 1; } declare() { return 2; } }";
        assert_eq!(transpile(ts), ts);
    }

    #[test]
    fn parameter_properties_in_a_derived_class_are_assigned_after_super() {
        let js = transpile("class A extends B { constructor(public y: string) { super(y); log(); } }");
        assert_eq!(js, "class A extends B { constructor(y) { super(y); this.y = y; log(); } }");
    }

    #[test]
    fn nested_calls_inside_super_do_not_confuse_the_injection_point() {
        let js = transpile("class A extends B { constructor(private a: number) { super(f(a), g(1)); } }");
        assert_eq!(js, "class A extends B { constructor(a) { super(f(a), g(1)); this.a = a; } }");
    }

    #[test]
    fn leaves_static_modifier_untouched_since_it_is_real_javascript() {
        assert_eq!(transpile("class Foo {\n  static count = 0;\n}"), "class Foo {\n  static count = 0;\n}");
    }

    #[test]
    fn does_not_touch_method_bodies_that_merely_contain_colons() {
        // メソッド本体内のラベル付き文・三項演算子・オブジェクトリテラルの
        // `:`は、クラスメンバーレベルの型注釈と誤認しない
        // (`is_member_level`が`brace_depth`をメソッド本体の深さと
        // 区別しているため)。
        let ts = "class Foo {\n  bar() {\n    const o = { a: 1 };\n    return true ? 1 : 2;\n  }\n}";
        assert_eq!(transpile(ts), ts);
    }
}
