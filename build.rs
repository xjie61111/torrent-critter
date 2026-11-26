fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/cat.ico");
        winres::WindowsResource::new()
            .set_icon("assets/cat.ico")
            .set("FileDescription", "Торрент-зверёк (Torrent Critter)")
            .set("ProductName", "Торрент-зверёк (Torrent Critter)")
            .compile()
            .expect("Не удалось встроить иконку котика в EXE");
    }
}
