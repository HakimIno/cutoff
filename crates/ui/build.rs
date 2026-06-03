fn main() {
    println!("cargo:rerun-if-changed=ui/app-window.slint");
    println!("cargo:rerun-if-changed=ui/components/details-panel.slint");
    slint_build::compile("ui/app-window.slint").expect("compile slint UI");
}
