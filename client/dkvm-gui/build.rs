// Windows: incrusta el icono (calavera) y los metadatos en dkvm-gui.exe. En Linux/macOS no hace nada.
fn main() {
    println!("cargo:rerun-if-changed=assets/skull.ico");
    println!("cargo:rerun-if-changed=assets/dkvm-gui.rc");
    #[cfg(windows)]
    {
        let _ = embed_resource::compile("assets/dkvm-gui.rc", embed_resource::NONE);
    }
}
