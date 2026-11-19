//! 実在のTSに近いサンプルを変換し、Node があれば実際に実行して結果を確かめる(無ければ構文の目視だけの検査に留める)。
use std::process::Command;

#[test]
fn sample_ts_transpiles_to_javascript_that_runs_under_node() {
    let ts = include_str!("sample.ts");
    let js = rs_typescript::transpile(ts);
    // 型だけの構文が残っていない
    for leftover in ["interface ", "implements", "abstract", "declare ", ": number", "satisfies", "import type", "type Id"] {
        assert!(!js.contains(leftover), "`{leftover}` が残っている:\n{js}");
    }
    let Ok(node) = Command::new("node").arg("--version").output() else {
        eprintln!("node が無いので実行確認は省略");
        return;
    };
    if !node.status.success() {
        return;
    }
    // import 行はモジュール解決が要るので落とし、ESM として実行する
    let body: String = js.lines().filter(|l| !l.trim_start().starts_with("import ")).collect::<Vec<_>>().join("\n");
    let dir = std::env::temp_dir().join("rs_typescript_sample");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.mjs");
    std::fs::write(&file, body).unwrap();
    let out = Command::new("node").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "c:1 0 12.57 6 1 5 3 1 1 x 1");
}
