// The alias delegates the same owner entry point, without a fixture/test mode.
#[path = "../src/bin/peerlume-daemon.rs"]
mod entry;

fn main() -> std::io::Result<()> {
    entry::main()
}
