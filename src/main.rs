mod app;
mod terminal;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}
