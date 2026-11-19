//! 式レベルの型構文(非nullアサーション`!`・`as Type`キャスト)を
//! トークン列から除去するパス。
//!
//! 対応する形:
//! - **非nullアサーション**: `foo!.bar`・`foo!()`・`arr[0]!` — 値の直後に
//!   付く後置の`!`のみ除去する。前置の論理否定(`!foo`)や`!=`/`!==`
//!   (トークナイザ側で既に単一トークン化済み、`tokenizer.rs`参照)とは
//!   別のトークンなので混同しない。
//! - **`as Type`キャスト**: `expr as Type`の`as`以降(型部分)を除去する。
//!
//! **意図的にスコープ外**: `<Type>expr`という前置キャスト構文
//! (TypeScriptの古い記法)は、ジェネリクス`<T>`呼び出しやJSXタグと
//! 構文上区別がつきにくく、他のヒューリスティックパスと同様「安全側」
//! に倒すとほぼ何もできないため、今回は対応しない(`as`構文の方が
//! 現代のTypeScriptコードベースで圧倒的に主流のため、実用上の影響は
//! 小さいと判断)。
//!
//! `?.`(オプショナルチェイニング)・`??`(nullish合体)は、
//! トークナイザが`?`単体とは別の複数文字トークンとして扱うため
//! (`tokenizer.rs`のPUNCTUATORS一覧参照)、このパスが後置`!`を探す際に
//! 誤って`?`絡みのトークンを型構文と誤認することはない。

use crate::token::Token;
use crate::transpile::{is_significant, next_sig_index};

/// 直前の意味トークンが「値の終端」(後置`!`が非nullアサーションとして
/// 妥当な位置)かどうかを判定する。
fn is_value_end(tok: &Token) -> bool {
    match tok {
        Token::Identifier(s) => !matches!(
            s.as_str(),
            // これらの直後の`!`は非nullアサーションではなく前置の論理否定
            // (例: `return !foo`・`typeof !foo`は文法上あり得ないが、
            // `return`直後は明らかに値の終端ではない)。
            "return" | "typeof" | "delete" | "void" | "in" | "of" | "new" | "instanceof" | "yield" | "await"
        ),
        Token::Number(_) | Token::StringLiteral(_) | Token::Raw(_) => true,
        Token::Punctuator(p) => matches!(p.as_str(), ")" | "]"),
        _ => false,
    }
}


/// `start` から型1つ分(`A`・`A.B`・`A<B, C>`・`A[]`・`{ x: T }`・`[A, B]`・`(A | B)`・`"lit"`・
/// ユニオン/インターセクション/`keyof`/`typeof` つき)を読み飛ばし、終端(含まない)を返す。
/// 読めなければ `None`。関数型(`() => T`)は対象外。
fn skip_type(tokens: &[Token], start: usize) -> Option<usize> {
    use crate::transpile::{consume_angle_generic, consume_matching_bracket};
    let mut i = start;
    loop {
        // 前置: keyof / typeof / readonly / unique / `|` `&` の先頭
        while let Some(Token::Identifier(k)) = tokens.get(i) {
            if matches!(k.as_str(), "keyof" | "typeof" | "readonly" | "unique") {
                i = next_sig_index(tokens, i + 1)?;
            } else {
                break;
            }
        }
        // 原子
        let mut end = match tokens.get(i)? {
            Token::Identifier(_) | Token::StringLiteral(_) | Token::Number(_) => i + 1,
            Token::Punctuator(p) if p == "{" => consume_matching_bracket(tokens, i, "{", "}")? + 1,
            Token::Punctuator(p) if p == "[" => consume_matching_bracket(tokens, i, "[", "]")? + 1,
            Token::Punctuator(p) if p == "(" => consume_matching_bracket(tokens, i, "(", ")")? + 1,
            _ => return None,
        };
        // 後置: `.Name` `<...>` `[]` `[K]`(隣接するときだけ)
        loop {
            match tokens.get(end) {
                Some(Token::Punctuator(p)) if p == "<" => end = consume_angle_generic(tokens, end)? + 1,
                Some(Token::Punctuator(p)) if p == "[" => end = consume_matching_bracket(tokens, end, "[", "]")? + 1,
                Some(Token::Punctuator(p)) if p == "." => match tokens.get(end + 1) {
                    Some(Token::Identifier(_)) => end += 2,
                    _ => break,
                },
                _ => break,
            }
        }
        // 続き: `|` / `&`(空白をまたいでよい)
        match next_sig_index(tokens, end) {
            Some(n) if matches!(&tokens[n], Token::Punctuator(p) if p == "|" || p == "&") => {
                i = next_sig_index(tokens, n + 1)?;
            }
            _ => return Some(end),
        }
    }
}

pub(crate) fn strip_expression_type_syntax(tokens: &[Token]) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    // `import { a as b }` / `export { a as b }` の波括弧の中では `as` はリネームでありキャストではない。
    let mut module_brace = false;
    while i < tokens.len() {
        let tok = &tokens[i];
        if let Token::Punctuator(p) = tok {
            if p == "{" {
                module_brace = matches!(out.iter().rev().find(|t| is_significant(t)),
                    Some(Token::Identifier(k)) if k == "import" || k == "export");
            } else if p == "}" {
                module_brace = false;
            }
        }

        // 後置`!`(非nullアサーション): 直前に出力済みの意味トークンが
        // 値の終端であれば、この`!`だけ読み飛ばす(出力しない)。
        if let Token::Punctuator(p) = tok {
            if p == "!" {
                if let Some(prev) = out.iter().rev().find(|t| is_significant(t)) {
                    if is_value_end(prev) {
                        i += 1;
                        continue;
                    }
                }
            }
        }

        // `as Type`: 意味トークンとして`as`が出現し、直前が値の終端の
        // 場合のみキャストとして扱う(`as`は予約語ではないため、変数名
        // として使われるケース—`const as = 1;`—との区別が必要)。
        if let Token::Identifier(s) = tok {
            if (s == "as" || s == "satisfies") && !module_brace {
                // `as`/`satisfies` の直前は値の終端(`}` はオブジェクト/関数リテラルの終端として許す。
                // `as` は文頭に来られないので、ブロックの `}` と混同する心配がない)
                let prev_is_value_end = out
                    .iter()
                    .rev()
                    .find(|t| is_significant(t))
                    .map(|t| is_value_end(t) || matches!(t, Token::Punctuator(p) if p == "}"))
                    .unwrap_or(false);
                if prev_is_value_end {
                    if let Some(next_sig) = next_sig_index(tokens, i + 1) {
                        if let Some(end) = skip_type(tokens, next_sig) {
                            // `x as T;` が `x ;` にならないよう、`as` の前の同じ行の空白も落とす
                            while matches!(out.last(), Some(Token::Whitespace(w)) if !w.contains('\n')) {
                                out.pop();
                            }
                            i = end;
                            continue;
                        }
                    }
                }
            }
        }

        out.push(tok.clone());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::CollectingSink;
    use crate::tokenizer::tokenize;
    use crate::transpile::token_text as tt;

    fn strip_and_render(src: &str) -> String {
        let mut sink = CollectingSink::default();
        tokenize(src, &mut sink);
        let stripped = strip_expression_type_syntax(&sink.tokens);
        stripped.iter().map(tt).collect()
    }

    #[test]
    fn strips_postfix_non_null_assertion_after_identifier() {
        assert_eq!(strip_and_render("foo!.bar();"), "foo.bar();");
    }

    #[test]
    fn strips_non_null_assertion_after_call_and_index() {
        assert_eq!(strip_and_render("getFoo()!.bar;"), "getFoo().bar;");
        assert_eq!(strip_and_render("arr[0]!.length;"), "arr[0].length;");
    }

    #[test]
    fn does_not_strip_prefix_logical_not() {
        assert_eq!(strip_and_render("if (!foo) { bar(); }"), "if (!foo) { bar(); }");
    }

    #[test]
    fn does_not_confuse_not_equal_operators() {
        assert_eq!(strip_and_render("if (a !== b) { c(); }"), "if (a !== b) { c(); }");
        assert_eq!(strip_and_render("if (a != b) { c(); }"), "if (a != b) { c(); }");
    }

    #[test]
    fn strips_as_type_cast() {
        assert_eq!(strip_and_render("const x = foo as Bar;"), "const x = foo;");
    }

    #[test]
    fn strips_as_const_assertion() {
        assert_eq!(strip_and_render("const x = [1, 2] as const;"), "const x = [1, 2];");
    }

    #[test]
    fn does_not_strip_as_used_as_a_plain_identifier() {
        assert_eq!(strip_and_render("const as = 1;"), "const as = 1;");
    }

    #[test]
    fn leaves_optional_chaining_and_nullish_coalescing_untouched() {
        assert_eq!(strip_and_render("const x = foo?.bar ?? baz;"), "const x = foo?.bar ?? baz;");
    }

    #[test]
    fn leaves_optional_chaining_call_untouched() {
        assert_eq!(strip_and_render("foo?.bar?.();"), "foo?.bar?.();");
    }

    #[test]
    fn strips_satisfies_and_complex_as_types() {
        use crate::transpile::transpile;
        assert_eq!(transpile("const o = { a: 1 } satisfies Rec;"), "const o = { a: 1 };");
        assert_eq!(transpile("const o = x satisfies Record<string, number>;"), "const o = x;");
        assert_eq!(transpile("const o = x as Array<number>;"), "const o = x;");
        assert_eq!(transpile("const o = x as { a: number };"), "const o = x;");
        assert_eq!(transpile("const o = x as A | B;"), "const o = x;");
        assert_eq!(transpile("const o = x as const;"), "const o = x;");
        assert_eq!(transpile("const o = f(x as string, y);"), "const o = f(x, y);");
    }

    #[test]
    fn as_in_import_and_export_braces_is_a_rename_not_a_cast() {
        use crate::transpile::transpile;
        assert_eq!(transpile("import { a as b } from 'x';"), "import { a as b } from 'x';");
        assert_eq!(transpile("export { a as b };"), "export { a as b };");
    }
}
