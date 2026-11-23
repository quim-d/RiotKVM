fn main() {
    riotkvm_core::config::adopt_legacy_env(); // DKVM_PORT, DKVM_LAYOUT... (nombre anterior) siguen valiendo
    let mut env = riotkvm_cli::Env::real();
    let code = riotkvm_cli::run(std::env::args_os(), &mut env);
    std::process::exit(code);
}
