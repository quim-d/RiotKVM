// Windows: incrusta el icono (calavera) y los metadatos en riotkvm-gui.exe. En Linux/macOS no hace nada.
fn main() {
    println!("cargo:rerun-if-changed=assets/skull.ico");
    println!("cargo:rerun-if-changed=assets/riotkvm-gui.rc");
    #[cfg(windows)]
    {
        let _ = embed_resource::compile("assets/riotkvm-gui.rc", embed_resource::NONE);
    }
}
