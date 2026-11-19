fn main() {
    let src = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    print!("{}", rs_typescript::transpile(&src));
}
