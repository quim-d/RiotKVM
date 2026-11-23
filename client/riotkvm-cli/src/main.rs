fn main() {
    let mut env = riotkvm_cli::Env::real();
    let code = riotkvm_cli::run(std::env::args_os(), &mut env);
    std::process::exit(code);
}
