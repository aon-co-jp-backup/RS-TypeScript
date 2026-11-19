//! 文レベルのTS専用構文を扱うパス(他のパスより先に走る)。
//!
//! 対応:
//! - `import type { A } from 'x';` / `export type { A };` → 文ごと除去。
//! - `import { type A, B } from 'x'` → 型だけの指定子を除去(全部が型なら文ごと除去)。
//! - `declare const/let/var/function/class/namespace/module/global/enum/...` → 文ごと除去
//!   (実行時に何も生成しないため)。直前の`export`も一緒に除去する。
//! - `abstract class` の `abstract` を除去。
//! - `namespace N { ... }` → IIFE(`var N; (function (N) { ... })(N || (N = {}));`)へ展開。
//!   `export const/let/var/function/class/enum` は元の宣言を残したうえで `N.x = x;` を足す。
//!   型だけの名前空間(値の宣言なし)は何も出さない(tscと同じ)。
//!
//! 対象外(安全側で素通し): 入れ子の `export namespace`、`namespace A.B`、`this`パラメータ、
//! デコレーター(JS側のStage 3構文として有効なため素通し)。

use crate::token::Token;
use crate::transpile::{consume_matching_bracket, is_significant, next_sig_index};

fn is_ident(t: &Token, s: &str) -> bool {
    matches!(t, Token::Identifier(x) if x == s)
}
fn is_punct(t: &Token, s: &str) -> bool {
    matches!(t, Token::Punctuator(x) if x == s)
}
fn has_newline(t: &Token) -> bool {
    matches!(t, Token::Whitespace(w) if w.contains('\n'))
}

/// 出力済みトークン列の末尾が「文の先頭」(直前の意味トークンが無い・`;` `{` `}`、または間に改行がある)か。
fn at_statement_start(out: &[Token]) -> bool {
    for t in out.iter().rev() {
        if is_significant(t) {
            return matches!(t, Token::Punctuator(p) if matches!(p.as_str(), ";" | "{" | "}"));
        }
        if has_newline(t) {
            return true;
        }
    }
    true
}

/// 出力済み列の末尾の `export`(+空白)を取り除く。
fn drop_trailing_export(out: &mut Vec<Token>) {
    let mut idx = out.len();
    while idx > 0 && !is_significant(&out[idx - 1]) {
        idx -= 1;
    }
    if idx > 0 && is_ident(&out[idx - 1], "export") {
        out.truncate(idx - 1);
    }
}

/// `start` から、`;`(深さ0)の次、または深さ0での改行の手前までの終端(含まない)を返す。
fn semi_or_line_end(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0i32;
    let mut seen_sig = false;
    let mut i = start;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Eof => return i,
            Token::Punctuator(p) => {
                seen_sig = true;
                match p.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => {
                        if depth == 0 {
                            return i; // 外側の閉じ括弧(文が閉じ括弧で終わる)
                        }
                        depth -= 1;
                    }
                    ";" if depth == 0 => return i + 1,
                    _ => {}
                }
            }
            t if has_newline(t) && depth == 0 && seen_sig => return i,
            t if is_significant(t) => seen_sig = true,
            _ => {}
        }
        i += 1;
    }
    i
}

/// `{`を含む宣言(class/namespace/...)の終端: 最初の深さ0の`{`に対応する`}`の次。
fn block_end(tokens: &[Token], start: usize) -> Option<usize> {
    let mut i = start;
    while i < tokens.len() {
        if is_punct(&tokens[i], "{") {
            return consume_matching_bracket(tokens, i, "{", "}").map(|c| c + 1);
        }
        if matches!(tokens[i], Token::Eof) {
            return None;
        }
        i += 1;
    }
    None
}

/// 末尾の`;`を1つ含めて、`end` の手前の空白は残す。
fn skip_statement_semicolon(tokens: &[Token], end: usize) -> usize {
    if matches!(tokens.get(end), Some(t) if is_punct(t, ";")) {
        end + 1
    } else {
        end
    }
}

/// `import`/`export`直後の`{ ... }`内から型だけの指定子を除く。戻り値は (新しい中身, 除いた個数, 残った個数)。
fn filter_type_specifiers(inner: &[Token]) -> (Vec<Token>, usize, usize) {
    let mut segments: Vec<Vec<Token>> = vec![Vec::new()];
    for t in inner {
        if is_punct(t, ",") {
            segments.push(Vec::new());
        } else {
            segments.last_mut().unwrap().push(t.clone());
        }
    }
    let mut kept: Vec<Vec<Token>> = Vec::new();
    let (mut removed, mut remaining) = (0usize, 0usize);
    for seg in segments {
        let sig: Vec<&Token> = seg.iter().filter(|t| is_significant(t)).collect();
        if sig.is_empty() {
            kept.push(seg); // 末尾カンマ後の空白など
            continue;
        }
        let is_type_only = sig.len() >= 2 && is_ident(sig[0], "type") && matches!(sig[1], Token::Identifier(n) if n != "as");
        if is_type_only {
            removed += 1;
        } else {
            remaining += 1;
            kept.push(seg);
        }
    }
    let mut out = Vec::new();
    for (n, seg) in kept.into_iter().enumerate() {
        if n > 0 {
            out.push(Token::Punctuator(",".into()));
        }
        out.extend(seg);
    }
    (out, removed, remaining)
}

fn names_declared(tokens: &[Token]) -> Vec<String> {
    // `const a = 1, b: T = 2` の a, b(深さ0、`,`直後または宣言直後の識別子)
    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut expect_name = true;
    for t in tokens {
        match t {
            Token::Punctuator(p) => match p.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                "," if depth == 0 => expect_name = true,
                _ => {}
            },
            Token::Identifier(n) if depth == 0 && expect_name => {
                names.push(n.clone());
                expect_name = false;
            }
            _ => {}
        }
    }
    names
}

fn namespace_body_has_values(body: &[Token]) -> bool {
    body.iter().any(|t| matches!(t, Token::Identifier(k) if matches!(k.as_str(), "const" | "let" | "var" | "function" | "class" | "enum" | "namespace")))
}

/// 名前空間の中身を変換する(`export`宣言→元の宣言+`N.x = x;`)。
fn transform_namespace_body(name: &str, body: &[Token]) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(body.len());
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < body.len() {
        let t = &body[i];
        if is_punct(t, "{") {
            depth += 1;
        } else if is_punct(t, "}") {
            depth -= 1;
        }
        if depth == 0 && is_ident(t, "export") && at_statement_start(&out) {
            if let Some(k) = next_sig_index(body, i + 1) {
                let kw = match &body[k] {
                    Token::Identifier(s) => s.as_str(),
                    _ => "",
                };
                let mut decl_kw = k;
                let mut kind = kw;
                if kw == "async" {
                    if let Some(k2) = next_sig_index(body, k + 1) {
                        decl_kw = k2;
                        kind = "function";
                    }
                }
                match kind {
                    "const" | "let" | "var" => {
                        let end = semi_or_line_end(body, k);
                        let names = names_declared(&body[next_sig_index(body, k + 1).unwrap_or(k + 1)..end.min(body.len())]);
                        // `export` とその後ろの空白を落とし、宣言本体をそのまま出す
                        out.extend(body[k..end].iter().cloned());
                        let ended_with_semicolon = matches!(body[k..end].iter().rev().find(|t| is_significant(t)), Some(t) if is_punct(t, ";"));
                        if !ended_with_semicolon {
                            out.push(Token::Raw(";".into()));
                        }
                        let assigns: String = names.iter().map(|n| format!(" {name}.{n} = {n};")).collect();
                        out.push(Token::Raw(assigns));
                        i = end;
                        continue;
                    }
                    "function" | "class" | "enum" => {
                        // 名前: kind の次の識別子(`function*` は未対応)
                        let name_idx = next_sig_index(body, decl_kw + 1);
                        let decl_name = name_idx.and_then(|n| match &body[n] {
                            Token::Identifier(s) => Some(s.clone()),
                            _ => None,
                        });
                        if let (Some(dn), Some(end)) = (decl_name, block_end(body, decl_kw)) {
                            out.extend(body[k..end].iter().cloned());
                            out.push(Token::Raw(format!(" {name}.{dn} = {dn};")));
                            i = end;
                            continue;
                        }
                    }
                    _ => {}
                }
            }
        }
        out.push(t.clone());
        i += 1;
    }
    out
}

pub(crate) fn strip_statement_level(tokens: &[Token]) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut i = 0usize;
    while i < tokens.len() {
        let tok = &tokens[i];
        if matches!(tok, Token::Eof) {
            out.push(tok.clone());
            break;
        }
        if let Token::Identifier(name) = tok {
            let stmt_start = at_statement_start(&out) || out.last().is_some() && {
                // `export declare ...` のように`export`の直後
                let mut idx = out.len();
                while idx > 0 && !is_significant(&out[idx - 1]) {
                    idx -= 1;
                }
                idx > 0 && is_ident(&out[idx - 1], "export")
            };
            match name.as_str() {
                "import" | "export" if at_statement_start(&out) => {
                    if let Some(k) = next_sig_index(tokens, i + 1) {
                        // import type ... / export type { ... }
                        if is_ident(&tokens[k], "type") {
                            if let Some(k2) = next_sig_index(tokens, k + 1) {
                                let type_only = if name == "import" {
                                    is_punct(&tokens[k2], "{")
                                        || is_punct(&tokens[k2], "*")
                                        || matches!(&tokens[k2], Token::Identifier(n) if n != "from")
                                } else {
                                    is_punct(&tokens[k2], "{") || is_punct(&tokens[k2], "*")
                                };
                                if type_only {
                                    let end = semi_or_line_end(tokens, k2);
                                    i = skip_statement_semicolon(tokens, end);
                                    continue;
                                }
                            }
                        }
                        // import { type A, B } / export { type A }
                        if is_punct(&tokens[k], "{") {
                            if let Some(close) = consume_matching_bracket(tokens, k, "{", "}") {
                                let (inner, removed, remaining) = filter_type_specifiers(&tokens[k + 1..close]);
                                if removed > 0 {
                                    if remaining == 0 && name == "import" {
                                        let end = semi_or_line_end(tokens, close + 1);
                                        i = skip_statement_semicolon(tokens, end);
                                        continue;
                                    }
                                    out.extend(tokens[i..=k].iter().cloned());
                                    out.extend(inner);
                                    out.push(tokens[close].clone());
                                    i = close + 1;
                                    continue;
                                }
                            }
                        }
                    }
                }
                "declare" if stmt_start => {
                    if let Some(k) = next_sig_index(tokens, i + 1) {
                        let block_kw = matches!(&tokens[k], Token::Identifier(s) if matches!(s.as_str(),
                            "class" | "namespace" | "module" | "global" | "enum" | "interface" | "abstract"))
                            || (is_ident(&tokens[k], "const")
                                && matches!(next_sig_index(tokens, k + 1).map(|n| &tokens[n]), Some(Token::Identifier(e)) if e == "enum"));
                        let is_stmt = matches!(&tokens[k], Token::Identifier(s) if matches!(s.as_str(),
                            "const" | "let" | "var" | "function" | "type" | "async")) || block_kw;
                        if is_stmt {
                            let end = if block_kw {
                                block_end(tokens, k)
                            } else {
                                Some(skip_statement_semicolon(tokens, semi_or_line_end(tokens, k)))
                            };
                            if let Some(end) = end {
                                drop_trailing_export(&mut out);
                                i = end;
                                continue;
                            }
                        }
                    }
                }
                "abstract" => {
                    if let Some(k) = next_sig_index(tokens, i + 1) {
                        if is_ident(&tokens[k], "class") {
                            i = k; // `abstract` と間の空白を落とす
                            continue;
                        }
                    }
                }
                "namespace" | "module" if stmt_start => {
                    // namespace Name {  (入れ子のドット名・文字列名のmoduleは対象外)
                    if let Some(n) = next_sig_index(tokens, i + 1) {
                        if let Token::Identifier(ns_name) = &tokens[n] {
                            if let Some(b) = next_sig_index(tokens, n + 1) {
                                if is_punct(&tokens[b], "{") {
                                    if let Some(close) = consume_matching_bracket(tokens, b, "{", "}") {
                                        let inner = &tokens[b + 1..close];
                                        // 中身を先に再帰変換(入れ子のnamespace・declare等)
                                        let inner = strip_statement_level(inner);
                                        if namespace_body_has_values(&inner) {
                                            let body = transform_namespace_body(ns_name, &inner);
                                            out.push(Token::Raw(format!("var {ns_name};\n(function ({ns_name}) {{")));
                                            out.extend(body);
                                            out.push(Token::Raw(format!("}})({ns_name} || ({ns_name} = {{}}));")));
                                        } else {
                                            drop_trailing_export(&mut out);
                                        }
                                        i = close + 1;
                                        continue;
                                    }
                                }
                            }
                        }
                    }
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
    fn removes_type_only_imports_and_exports() {
        assert_eq!(transpile("import type { A } from './a';\nconst x = 1;"), "\nconst x = 1;");
        assert_eq!(transpile("import type * as T from './a';\nlet y = 2;"), "\nlet y = 2;");
        assert_eq!(transpile("export type { A, B };\nlet y = 2;"), "\nlet y = 2;");
    }

    #[test]
    fn keeps_a_default_import_literally_named_type() {
        assert_eq!(transpile("import type from './t';"), "import type from './t';");
    }

    #[test]
    fn removes_inline_type_specifiers_only() {
        assert_eq!(transpile("import { type B, C } from './b';"), "import { C } from './b';");
        assert_eq!(transpile("import { type B } from './b';\nlet z = 1;"), "\nlet z = 1;");
        assert_eq!(transpile("export { type B, C };"), "export { C };");
        // `type as X` のリネームは型指定子ではない
        assert_eq!(transpile("import { type as X } from './b';"), "import { type as X } from './b';");
    }

    #[test]
    fn removes_declare_statements_and_their_export() {
        assert_eq!(transpile("declare const z: number;\nlet a = 1;"), "\nlet a = 1;");
        assert_eq!(transpile("declare function g(x: number): void;\nlet a = 1;"), "\nlet a = 1;");
        assert_eq!(transpile("export declare class C { x: number }\nlet a = 1;"), "\nlet a = 1;");
        assert_eq!(transpile("declare module 'm' { export const v: number; }\nlet a = 1;"), "\nlet a = 1;");
        assert_eq!(transpile("declare global { interface Window { x: number } }\nlet a = 1;"), "\nlet a = 1;");
        assert_eq!(transpile("declare const o: {\n a: number;\n};\nlet a = 1;"), "\nlet a = 1;");
    }

    #[test]
    fn a_member_named_declare_is_left_alone() {
        assert_eq!(transpile("obj.declare(1);"), "obj.declare(1);");
        assert_eq!(transpile("const declare = 1;"), "const declare = 1;");
    }

    #[test]
    fn drops_abstract_on_class_declarations() {
        assert_eq!(transpile("abstract class Q { }"), "class Q { }");
        assert_eq!(transpile("export abstract class Q { }"), "export class Q { }");
    }

    #[test]
    fn expands_a_namespace_into_an_iife_with_export_assignments() {
        let js = transpile("namespace N { export const a: number = 1; export function f(x: number) { return x; } const hidden = 2; }");
        assert_eq!(
            js,
            "var N;\n(function (N) { const a = 1; N.a = a; function f(x) { return x; } N.f = f; const hidden = 2; })(N || (N = {}));"
        );
    }

    #[test]
    fn a_types_only_namespace_emits_nothing() {
        assert_eq!(transpile("namespace T { export interface I { x: number } export type U = string; }\nlet a = 1;"), "\nlet a = 1;");
    }

    #[test]
    fn namespace_with_enum_and_class_exports() {
        let js = transpile("namespace M { export enum E { A } export class C {} }");
        assert!(js.starts_with("var M;\n(function (M) {"), "{js}");
        assert!(js.contains("M.E = E;") && js.contains("M.C = C;"), "{js}");
        assert!(js.ends_with("})(M || (M = {}));"), "{js}");
    }

    #[test]
    fn a_variable_named_namespace_or_module_is_left_alone() {
        assert_eq!(transpile("module.exports = 1;"), "module.exports = 1;");
        assert_eq!(transpile("const namespace = 1;"), "const namespace = 1;");
    }
}
