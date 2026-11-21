fn main() {
    let mut env = dkvm_cli::Env::real();
    let code = dkvm_cli::run(std::env::args_os(), &mut env);
    std::process::exit(code);
}
