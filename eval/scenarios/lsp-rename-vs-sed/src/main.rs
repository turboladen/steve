use lsp_rename_fixture::{caller, foo_bar};

fn main() {
    let r = foo_bar(10);
    println!("{r}");
    let _ = caller();
}
