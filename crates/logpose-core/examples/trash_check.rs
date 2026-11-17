//! Moves the given path to the desktop Trash with the same code the removal uses (DEL-002).
//! Run: cargo run -p logpose-core --example trash_check -- <path>
fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("path"));
    match logpose_core::removal::system_trash(&[&path]) {
        Ok(()) => println!("moved to the Trash: {}", path.display()),
        Err(e) => println!("failed: {e}"),
    }
}
